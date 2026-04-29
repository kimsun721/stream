use std::{
    net::UdpSocket,
    sync::{Arc, Weak, mpsc::Receiver},
    time::{Duration, Instant},
};

use str0m::{
    Event, IceConnectionState, Input, Output, Rtc,
    change::{SdpAnswer, SdpApi, SdpOffer},
    channel::ChannelData,
    media::{Direction, KeyframeRequest, MediaData, MediaKind, Mid},
};
use tracing::warn;

use crate::{
    sfu::error::{ClientError, ClientResult},
    types::{
        Client, ClientRole, PollResult, RoomId, RoomState, Rooms, TrackIn, TrackInEntry, TrackOut,
        TrackOutState,
    },
};

pub fn register_client(rx: &Receiver<(Rtc, ClientRole, RoomId)>, rooms_arc: &Rooms) {
    if let Ok((rtc, role, room_id)) = rx.try_recv() {
        let mut rooms = rooms_arc.lock().unwrap();

        let mut client = Client::new(rtc, role);

        if let Some(room) = rooms.get_mut(&room_id.0) {
            match role {
                ClientRole::Streamer => {
                    if room
                        .clients
                        .iter()
                        .any(|c| matches!(c.role, ClientRole::Streamer))
                    {
                        warn!("Streamer already connected in room {:?}", &room);
                    } else {
                        room.streamer_id = Some(client.id);
                        room.clients.push(client);
                    }
                }
                ClientRole::Viewer => match room.state {
                    RoomState::IDLE | RoomState::PREVIEW => {
                        warn!("Client connected to an {:?} room", room.state);
                    }
                    RoomState::LIVE => {
                        let tracks: Vec<Weak<TrackIn>> = room
                            .clients
                            .iter()
                            .filter(|c| c.role == ClientRole::Streamer)
                            .flat_map(|c| c.tracks_in.iter().map(|t| Arc::downgrade(&t.id)))
                            .collect();

                        for track_in in tracks {
                            client.tracks_out.push(TrackOut {
                                track_in,
                                state: TrackOutState::ToOpen,
                            });
                        }

                        room.clients.push(client);
                    }
                },
            }
        } else {
            warn!("Room does not exist : {:?}", room_id);
        };
    };
}

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
                            let track_in = self.handle_media_added(m.mid, m.kind);
                            new_tracks.push(track_in);
                        }
                    }
                    Event::MediaData(data) => media_datas.push(data),
                    Event::ChannelOpen(cid, _label) => self.cid = Some(cid),
                    Event::ChannelData(data) => self.handle_channel_data(data)?,

                    Event::KeyframeRequest(request) => {
                        if let Some(streamer_mid) = self.tracks_out.iter().find_map(|t| {
                            if t.state == TrackOutState::Open(request.mid) {
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
                }
                if let TrackOutState::Open(viewer_mid) = t.state {
                    Some(viewer_mid)
                } else {
                    None
                }
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
                    .map_or(true, |r| r.elapsed() >= Duration::from_millis(1000));

                if should_request {
                    if let Some(mut writer) = self.rtc.writer(req.mid) {
                        writer.request_keyframe(req.rid, req.kind)?;
                        track_entry.last_keyframe_request = Some(Instant::now());
                    }
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
            if track_out.state == TrackOutState::ToOpen {
                if let Some(track_in) = track_out.track_in.upgrade() {
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

    fn handle_media_added(&mut self, mid: Mid, kind: MediaKind) -> Arc<TrackIn> {
        let track_in = Arc::new(TrackIn {
            origin: self.id,
            mid,
            kind,
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
