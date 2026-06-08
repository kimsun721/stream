use std::{
    net::UdpSocket,
    sync::Arc,
    time::{Duration, Instant},
};

use str0m::{
    Event, IceConnectionState, Input, Output,
    change::{SdpAnswer, SdpOffer},
    channel::ChannelData,
    media::{Direction, KeyframeRequest, KeyframeRequestKind, MediaData, MediaKind, Mid, Rid},
};
use tracing::{debug, warn};

use crate::{
    sfu::error::{ClientError, ClientResult},
    types::{
        C2sDcPayload, Client, ClientId, ClientRole, LinkState, PollResult, RelayStatus,
        S2cDcPayload, TrackIn, TrackInEntry, TrackOutState,
    },
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
        p2p_sdps: &mut Vec<(ClientId, S2cDcPayload)>,
        disconnected_relays: &mut Vec<ClientId>,
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
                    Event::ChannelData(data) => self.handle_channel_data(
                        data,
                        keyframe_requests,
                        p2p_sdps,
                        disconnected_relays,
                    )?,

                    Event::KeyframeRequest(request) => {
                        if let Some(translated) = self.translate_keyframe_request(request) {
                            keyframe_requests.push(translated);
                        }
                    }
                    _ => {}
                },
            };
        };

        Ok(PollResult::Timeout(timeout))
    }

    fn matching_viewer_mid(&self, mid: Mid, rid: Option<Rid>) -> Option<Mid> {
        self.tracks_out.iter().find_map(|t| {
            let track_in = t.track_in.upgrade()?;
            if track_in.mid != mid {
                return None;
            };

            if rid != t.chosen_rid {
                return None;
            };

            let TrackOutState::Open(viewer_mid) = t.state else {
                return None;
            };

            Some(viewer_mid)
        })
    }

    fn translate_keyframe_request(&self, request: KeyframeRequest) -> Option<KeyframeRequest> {
        self.tracks_out.iter().find_map(|t| {
            if t.state != TrackOutState::Open(request.mid) {
                return None;
            };

            let streamer_mid = t.track_in.upgrade()?.mid;

            Some(KeyframeRequest {
                mid: streamer_mid,
                rid: t.chosen_rid,
                kind: request.kind,
            })
        })
    }

    pub fn handle_media_datas(&mut self, datas: &Vec<MediaData>) -> ClientResult<()> {
        for data in datas {
            let Some(mid) = self.matching_viewer_mid(data.mid, data.rid) else {
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
        p2p_sdps: &mut Vec<(ClientId, S2cDcPayload)>,
        disconnected_relays: &mut Vec<ClientId>,
    ) -> ClientResult<PollResult> {
        self.renegotiate()?;
        let result = self.poll_output(
            socket,
            new_tracks,
            media_datas,
            keyframe_requests,
            p2p_sdps,
            disconnected_relays,
        )?;

        Ok(result)
    }

    pub fn request_sdp_offer(&mut self) -> ClientResult<()> {
        let mut channel = self
            .cid
            .and_then(|id| self.rtc.channel(id))
            .ok_or(ClientError::ChannelNotFound)?;

        let payload = S2cDcPayload::RequestOffer {};
        let json = serde_json::to_string(&payload)?;

        channel.write(false, json.as_bytes())?;

        Ok(())
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

    fn handle_channel_data(
        &mut self,
        data: ChannelData,
        keyframe_requests: &mut Vec<KeyframeRequest>,
        p2p_sdps: &mut Vec<(ClientId, S2cDcPayload)>,
        disconnected_relays: &mut Vec<ClientId>,
    ) -> ClientResult<()> {
        let payload: C2sDcPayload = serde_json::from_slice(&data.data)?;

        match payload {
            C2sDcPayload::Offer { sdp } => self.handle_offer(&sdp),
            C2sDcPayload::Answer { sdp } => self.handle_answer(&sdp),
            C2sDcPayload::SetLayer { mid, rid } => self.set_layer(mid, rid, keyframe_requests),
            C2sDcPayload::PerfReport {
                rtt_ms,
                loss_pct,
                avail_out_kbs,
            } => self.perf_report(rtt_ms, loss_pct, avail_out_kbs),
            C2sDcPayload::P2pOffer { sdp } => {
                self.handle_p2p_sdp(S2cDcPayload::P2pOffer { sdp }, p2p_sdps)
            }
            C2sDcPayload::P2pAnswer { sdp } => {
                self.handle_p2p_sdp(S2cDcPayload::P2pAnswer { sdp }, p2p_sdps)
            }
            C2sDcPayload::P2pConnected => self.handle_p2p_connected(),
            C2sDcPayload::P2pDisconnected => {
                self.handle_p2p_disconnected(keyframe_requests, disconnected_relays)
            }
        }
    }

    fn handle_offer(&mut self, offer: &str) -> ClientResult<()> {
        let offer = SdpOffer::from_sdp_string(offer)?;

        let answer = self.rtc.sdp_api().accept_offer(offer)?;

        if let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) {
            let json = serde_json::to_string(&answer)?;
            channel.write(false, json.as_bytes())?;
        };

        Ok(())
    }

    fn handle_answer(&mut self, answer: &str) -> ClientResult<()> {
        let answer = SdpAnswer::from_sdp_string(answer)?;

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

    fn handle_p2p_sdp(
        &self,
        sdp: S2cDcPayload,
        p2p_sdps: &mut Vec<(ClientId, S2cDcPayload)>,
    ) -> ClientResult<()> {
        match self.relay_status {
            Some(RelayStatus::Relay { leaf }) => {
                p2p_sdps.push((leaf, sdp));
            }
            Some(RelayStatus::Leaf {
                relay,
                link_state: LinkState::Connecting,
            }) => {
                p2p_sdps.push((relay, sdp));
            }
            _ => (),
        };
        Ok(())
    }

    fn handle_p2p_connected(&mut self) -> ClientResult<()> {
        if let Some(RelayStatus::Leaf { link_state, .. }) = &mut self.relay_status {
            *link_state = LinkState::Connected;
        }

        Ok(())
    }

    fn handle_p2p_disconnected(
        &mut self,
        keyframe_requests: &mut Vec<KeyframeRequest>,
        disconnected_relays: &mut Vec<ClientId>,
    ) -> ClientResult<()> {
        if let Some(RelayStatus::Leaf { relay, link_state }) = &self.relay_status
            && matches!(link_state, LinkState::Connected)
        {
            disconnected_relays.push(*relay);
            for kf in self.demote_leaf()? {
                keyframe_requests.push(kf);
            }
        }

        Ok(())
    }

    fn demote_leaf(&mut self) -> ClientResult<Vec<KeyframeRequest>> {
        self.relay_status = None;

        let keyframe_requests: Vec<_> = self
            .tracks_out
            .iter()
            .filter_map(|to| {
                let track_in = to.track_in.upgrade()?;

                if track_in.kind == MediaKind::Audio {
                    return None;
                }

                Some(KeyframeRequest {
                    mid: track_in.mid,
                    rid: to.chosen_rid,
                    kind: KeyframeRequestKind::Fir,
                })
            })
            .collect();

        Ok(keyframe_requests)
    }

    fn set_layer(
        &mut self,
        mid: Mid,
        rid: Rid,
        keyframe_requests: &mut Vec<KeyframeRequest>,
    ) -> ClientResult<()> {
        let Some(track_out) = self
            .tracks_out
            .iter_mut()
            .find(|t| t.state == TrackOutState::Open(mid))
        else {
            return Ok(());
        };

        let Some(track_in) = track_out.track_in.upgrade() else {
            return Ok(());
        };

        if !track_in.available_rids.contains(&rid) {
            return Ok(());
        };

        if track_out.chosen_rid == Some(rid) {
            return Ok(());
        };

        track_out.chosen_rid = Some(rid);

        keyframe_requests.push(KeyframeRequest {
            mid: track_in.mid,
            rid: Some(rid),
            kind: str0m::media::KeyframeRequestKind::Fir,
        });

        Ok(())
    }

    pub fn perf_report(
        &mut self,
        rtt_ms: u32,
        loss_pct: f32,
        avail_out_kbps: u32,
    ) -> ClientResult<()> {
        debug!(
            client_id = %self.id,
            rtt_ms,
            loss_pct,
            avail_out_kbps,
            "perf_report"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use str0m::{
        Rtc,
        media::{KeyframeRequest, KeyframeRequestKind, MediaKind, Mid, Rid},
    };

    use crate::types::{Client, ClientRole, TrackIn, TrackOut, TrackOutState};

    fn viewer_with_track_out() -> (Client, Arc<TrackIn>) {
        let mut client = Client::new(Rtc::new(Instant::now()), ClientRole::Viewer);

        let streamer_mid = Mid::from("streamer-video");
        let viewer_mid = Mid::from("viewer-video");
        let low_rid = Rid::from("l");
        let high_rid = Rid::from("h");

        let track_in = Arc::new(TrackIn {
            origin: client.id,
            mid: streamer_mid,
            kind: MediaKind::Video,
            available_rids: vec![low_rid, high_rid],
        });

        let track_out = TrackOut {
            track_in: Arc::downgrade(&track_in),
            state: TrackOutState::Open(viewer_mid),
            chosen_rid: Some(low_rid),
        };

        client.tracks_out.push(track_out);
        (client, track_in)
    }

    #[test]
    fn set_layer_updates_chosen_rid_and_requests_keyframe() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests: Vec<KeyframeRequest> = Vec::new();
        let viewer_mid = Mid::from("viewer-video");
        let streamer_mid = Mid::from("streamer-video");
        let high_rid = Rid::from("h");

        client
            .set_layer(viewer_mid, high_rid, &mut keyframe_requests)
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(high_rid));
        assert_eq!(keyframe_requests.len(), 1);
        assert_eq!(keyframe_requests[0].mid, streamer_mid);
        assert_eq!(keyframe_requests[0].rid, Some(high_rid));
    }

    #[test]
    fn set_layer_ignores_unknown_mid() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests: Vec<KeyframeRequest> = Vec::new();

        client
            .set_layer(
                Mid::from("unknown-video"),
                Rid::from("h"),
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert!(keyframe_requests.is_empty());
    }

    #[test]
    fn set_layer_ignores_unknown_rid() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests: Vec<KeyframeRequest> = Vec::new();

        client
            .set_layer(
                Mid::from("viewer-video"),
                Rid::from("x"),
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert!(keyframe_requests.is_empty());
    }

    #[test]
    fn set_layer_ignores_no_change() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests: Vec<KeyframeRequest> = Vec::new();

        client
            .set_layer(
                Mid::from("viewer-video"),
                Rid::from("l"),
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert!(keyframe_requests.is_empty());
    }

    #[test]
    fn media_routed_only_when_rid_matches_chosen() {
        let (client, _track_in) = viewer_with_track_out();
        let streamer_mid = Mid::from("streamer-video");
        let viewer_mid = Mid::from("viewer-video");

        let cases = [
            (Some(Rid::from("l")), Some(viewer_mid)),
            (Some(Rid::from("h")), None),
            (None, None),
        ];

        for (incoming_rid, expected) in cases {
            let viewer_mid = client.matching_viewer_mid(streamer_mid, incoming_rid);

            assert_eq!(viewer_mid, expected, "incoming rid {incoming_rid:?}",);
        }
    }

    #[test]
    fn media_dropped_for_unknown_mid() {
        let (client, _track_in) = viewer_with_track_out();

        let viewer_mid =
            client.matching_viewer_mid(Mid::from("unknown-video"), Some(Rid::from("l")));

        assert_eq!(viewer_mid, None);
    }

    #[test]
    fn keyframe_request_remapped_to_streamer_mid_and_chosen_rid() {
        let (client, _track_in) = viewer_with_track_out();

        let translated = client
            .translate_keyframe_request(KeyframeRequest {
                mid: Mid::from("viewer-video"),
                rid: None,
                kind: KeyframeRequestKind::Pli,
            })
            .expect("request should map to the streamer track");

        assert_eq!(translated.mid, Mid::from("streamer-video"));
        assert_eq!(translated.rid, Some(Rid::from("l")));
        assert_eq!(translated.kind, KeyframeRequestKind::Pli);
    }

    #[test]
    fn keyframe_request_dropped_for_unknown_mid() {
        let (client, _track_in) = viewer_with_track_out();

        let translated = client.translate_keyframe_request(KeyframeRequest {
            mid: Mid::from("unknown-video"),
            rid: None,
            kind: KeyframeRequestKind::Pli,
        });

        assert!(translated.is_none());
    }
}
