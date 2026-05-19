use std::{
    net::UdpSocket,
    sync::Arc,
    time::{Duration, Instant},
};

use str0m::{
    Event, IceConnectionState, Input, Output,
    change::{SdpAnswer, SdpOffer},
    channel::ChannelData,
    media::{Direction, KeyframeRequest, MediaData, MediaKind, Mid, Rid},
};
use tracing::warn;

use crate::{
    sfu::error::{ClientError, ClientResult},
    types::{Client, ClientRole, PollResult, TrackIn, TrackInEntry, TrackOutState},
};

impl Client {
    pub fn handle_input(&mut self, input: Input) {
        if !self.rtc.is_alive() {
            return;
        }
        if let Err(e) = self.rtc.handle_input(input) {
            warn!("Client ({:?}) disconnected: {:?}", self.id, e);
            self.rtc.disconnect();
        }
    }

    pub fn poll_output(
        self: &mut Client,
        socket: &UdpSocket,
        new_tracks: &mut Vec<Arc<TrackIn>>,
        media_datas: &mut Vec<MediaData>,
        keyframe_requests: &mut Vec<KeyframeRequest>,
    ) -> ClientResult<PollResult> {
        let timeout = loop {
            match self.rtc.poll_output()? {
                Output::Timeout(v) => break v,
                Output::Transmit(v) => {
                    socket.send_to(&v.contents, v.destination)?;
                }

                Output::Event(e) => match e {
                    Event::IceConnectionStateChange(v) => {
                        if v == IceConnectionState::Disconnected {
                            self.rtc.disconnect();
                            return Ok(PollResult::Disconnected);
                        }
                    }
                    Event::MediaAdded(m) => {
                        if self.role == ClientRole::Streamer {
                            let mut rids: Vec<Rid> = Vec::new();
                            if let Some(simulcast) = m.simulcast {
                                for layer in simulcast.send {
                                    rids.push(layer.rid);
                                }
                            };

                            let track_in = self.handle_media_added(m.mid, m.kind, rids);
                            new_tracks.push(track_in);
                        }
                    }
                    Event::MediaData(data) => media_datas.push(data),
                    Event::ChannelOpen(cid, _label) => self.cid = Some(cid),
                    Event::ChannelData(data) => self.handle_channel_data(data)?,

                    Event::KeyframeRequest(mut request) => {
                        if let Some(streamer_mid) = self.tracks_out.iter().find_map(|t| {
                            if t.state == TrackOutState::Open(request.mid) {
                                request.rid = t.chosen_rid;
                                t.track_in.upgrade().map(|ti| ti.mid)
                            } else {
                                None
                            }
                        }) {
                            keyframe_requests.push(KeyframeRequest {
                                mid: streamer_mid,
                                ..request
                            });
                        };
                    }
                    _ => {}
                },
            };
        };

        Ok(PollResult::Timeout(timeout))
    }

    pub fn handle_media_datas(&mut self, datas: &Vec<MediaData>) -> ClientResult<()> {
        for data in datas {
            let Some(mid) = self.tracks_out.iter().find_map(|t| {
                let track_in = t.track_in.upgrade()?;
                if track_in.mid != data.mid {
                    return None;
                };

                if data.rid != t.chosen_rid {
                    return None;
                };

                let TrackOutState::Open(viewer_mid) = t.state else {
                    return None;
                };

                Some(viewer_mid)
            }) else {
                continue;
            };

            let Some(writer) = self.rtc.writer(mid) else {
                continue;
            };

            let Some(pt) = writer.match_params(data.params) else {
                continue;
            };

            writer.write(pt, data.network_time, data.time, data.data.clone())?;
        }
        Ok(())
    }

    pub fn handle_keyframe_requests(
        &mut self,
        keyframe_requests: Vec<KeyframeRequest>,
    ) -> ClientResult<()> {
        for req in keyframe_requests {
            if let Some(track_entry) = self.tracks_in.iter_mut().find(|t| t.id.mid == req.mid) {
                let should_request = track_entry
                    .last_keyframe_request
                    .is_none_or(|r| r.elapsed() >= Duration::from_millis(1000));

                if should_request && let Some(mut writer) = self.rtc.writer(req.mid) {
                    writer.request_keyframe(req.rid, req.kind)?;
                    track_entry.last_keyframe_request = Some(Instant::now());
                };
            };
        }

        Ok(())
    }

    pub fn renegotiate(&mut self) -> ClientResult<()> {
        if self.pending.is_some() || self.cid.is_none() {
            return Ok(());
        }

        let mut change = self.rtc.sdp_api();

        for track_out in self.tracks_out.iter_mut() {
            if track_out.state == TrackOutState::ToOpen
                && let Some(track_in) = track_out.track_in.upgrade()
            {
                let stream_id = track_in.origin.to_string();
                let mid = change.add_media(
                    track_in.kind,
                    Direction::SendOnly,
                    Some(stream_id),
                    None,
                    None,
                );

                track_out.state = TrackOutState::Negotiating(mid);
            }
        }

        if change.has_changes() {
            let (offer, pending) = change.apply().ok_or(ClientError::ApplyFailed)?;

            let mut channel = self
                .cid
                .and_then(|id| self.rtc.channel(id))
                .ok_or(ClientError::ChannelNotFound)?;

            let json = serde_json::to_string(&offer)?;

            channel.write(false, json.as_bytes())?;

            self.pending = Some(pending);
        }
        Ok(())
    }

    pub fn tick(
        &mut self,
        socket: &UdpSocket,
        new_tracks: &mut Vec<Arc<TrackIn>>,
        media_datas: &mut Vec<MediaData>,
        keyframe_requests: &mut Vec<KeyframeRequest>,
    ) -> ClientResult<PollResult> {
        self.renegotiate()?;
        let result = self.poll_output(socket, new_tracks, media_datas, keyframe_requests)?;

        Ok(result)
    }

    fn handle_media_added(
        &mut self,
        mid: Mid,
        kind: MediaKind,
        available_rids: Vec<Rid>,
    ) -> Arc<TrackIn> {
        let track_in = Arc::new(TrackIn {
            origin: self.id,
            mid,
            kind,
            available_rids,
        });

        let track_in_entry = TrackInEntry {
            id: track_in.clone(),
            last_keyframe_request: None,
        };

        self.tracks_in.push(track_in_entry);
        track_in
    }

    fn handle_channel_data(&mut self, data: ChannelData) -> ClientResult<()> {
        if let Ok(offer) = serde_json::from_slice::<'_, SdpOffer>(&data.data) {
            self.handle_offer(offer)?;
        } else if let Ok(answer) = serde_json::from_slice::<'_, SdpAnswer>(&data.data) {
            self.handle_answer(answer)?;
        }
        Ok(())
    }

    fn handle_offer(&mut self, offer: SdpOffer) -> ClientResult<()> {
        let answer = self.rtc.sdp_api().accept_offer(offer)?;

        if let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) {
            let json = serde_json::to_string(&answer)?;
            channel.write(false, json.as_bytes())?;
        };

        Ok(())
    }

    fn handle_answer(&mut self, answer: SdpAnswer) -> ClientResult<()> {
        if let Some(pending) = self.pending.take() {
            self.rtc.sdp_api().accept_answer(pending, answer)?;

            for track in &mut self.tracks_out {
                if let TrackOutState::Negotiating(m) = track.state {
                    track.state = TrackOutState::Open(m);
                }
            }
        }
        Ok(())
    }
}
