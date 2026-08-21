use std::{
    collections::HashMap,
    net::UdpSocket,
    sync::Arc,
    time::{Duration, Instant},
};

use str0m::{
    Event, IceConnectionState, Input, Output,
    bwe::{Bitrate, BweKind},
    change::{SdpAnswer, SdpOffer},
    channel::ChannelData,
    media::{Direction, KeyframeRequest, KeyframeRequestKind, MediaData, MediaKind, Mid, Rid},
};
use tracing::{debug, error, info, warn};

use crate::{
    sfu::error::{ClientError, ClientResult},
    types::{
        C2sDcPayload, Client, ClientId, ClientRole, LayerMode, LinkState, PerfSample, PollResult,
        RelayStatus, S2cDcPayload, SimulcastLayerProfile, TrackIn, TrackInEntry, TrackOut,
        TrackOutState, UploadProbeResult,
    },
};

const HEADROOM_MARGIN_PERCENT: u128 = 10;
const UPSWITCH_MARGIN_PERCENT: u128 = 10;

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
                            let simulcast_tracks = self.pending_simulcast_tracks.clone();
                            let mut layers: Vec<SimulcastLayerProfile> = Vec::new();
                            for track in simulcast_tracks {
                                if m.mid == track.mid
                                    && let Some(simulcast) = &m.simulcast
                                {
                                    for layer in &simulcast.recv {
                                        for l in track.layers.iter() {
                                            if l.rid == layer.rid {
                                                info!(
                                                    client_id = %self.id,
                                                    mid = ?m.mid,
                                                    rid = ?l.rid,
                                                    max_br_bps = l.max_br,
                                                    "negotiated simulcast layer"
                                                );
                                                layers.push(SimulcastLayerProfile {
                                                    rid: l.rid,
                                                    max_br: l.max_br,
                                                });
                                            }
                                        }
                                    }
                                };
                            }
                            let track_in = self.handle_media_added(m.mid, m.kind, layers);
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
                    Event::EgressBitrateEstimate(kind) => {
                        self.handle_egress_bitrate_estimate(kind, keyframe_requests)?
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

            if rid != t.chosen_rid() {
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
                rid: t.chosen_rid(),
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
                    .last_keyframe_requested_at
                    .get(&req.rid)
                    .is_none_or(|at| at.elapsed() >= Duration::from_millis(1000));

                if should_request && let Some(mut writer) = self.rtc.writer(req.mid) {
                    writer.request_keyframe(req.rid, req.kind)?;
                    track_entry
                        .last_keyframe_requested_at
                        .insert(req.rid, Instant::now());
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

    pub fn calc_desired_bitrate(&self) -> Option<Bitrate> {
        let mut total_bitrate: Option<u128> = None;

        self.tracks_out.iter().for_each(|to| {
            if let Some(ti) = to.track_in.upgrade()
                && let Some(highest_layer) = ti.highest_layer()
            {
                *total_bitrate.get_or_insert_default() += u128::from(highest_layer.max_br);
            };
        });

        if let Some(mut total_bitrate) = total_bitrate {
            let bitrate_margin =
                total_bitrate * (HEADROOM_MARGIN_PERCENT + UPSWITCH_MARGIN_PERCENT) / 100;

            total_bitrate += bitrate_margin;

            if let Ok(desired_bitrate) = u64::try_from(total_bitrate) {
                return Some(Bitrate::bps(desired_bitrate));
            } else {
                warn!("bitrate calc overflow {total_bitrate}");
            }
        }

        None
    }

    fn handle_media_added(
        &mut self,
        mid: Mid,
        kind: MediaKind,
        simulcast_layers: Vec<SimulcastLayerProfile>,
    ) -> Arc<TrackIn> {
        let track_in = Arc::new(TrackIn {
            origin: self.id,
            mid,
            kind,
            available_simulcast_layers: simulcast_layers,
        });

        let track_in_entry = TrackInEntry {
            id: track_in.clone(),
            last_keyframe_requested_at: HashMap::new(),
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
        let payload: C2sDcPayload = match serde_json::from_slice(&data.data) {
            Ok(payload) => payload,
            Err(e) => {
                warn!(
                    error = ?e,
                    channel_data = ?&String::from_utf8_lossy(&data.data),
                    "failed to deserialize channel data"
                );
                return Ok(());
            }
        };

        match payload {
            C2sDcPayload::Offer { sdp } => self.handle_offer(&sdp),
            C2sDcPayload::Answer { sdp } => self.handle_answer(&sdp),
            C2sDcPayload::SetLayer { mid, rid } => {
                let mid = Mid::from(mid.as_str());
                let rid = Rid::from(rid.as_str());

                if self.tracks_out.iter().any(|t| {
                    t.state == TrackOutState::Open(mid) && matches!(t.layer_mode, LayerMode::Manual)
                }) {
                    self.set_layer(mid, rid, keyframe_requests)
                } else {
                    Ok(())
                }
            }
            C2sDcPayload::SetLayerMode { mid, layer_mode } => {
                self.set_layer_mode(mid, layer_mode, keyframe_requests)
            }
            C2sDcPayload::PerfReport { rtt_ms, loss_pct } => self.perf_report(rtt_ms, loss_pct),
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
            C2sDcPayload::AvailableUpload {
                available_upload_kbps,
            } => self.handle_available_upload(available_upload_kbps),
            C2sDcPayload::RelayOutgoing { kbps } => self.handle_relay_outgoing(kbps),
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

    fn handle_available_upload(&mut self, available_upload_kbps: u32) -> ClientResult<()> {
        if let Some(UploadProbeResult::Probing { .. }) = self.available_upload {
            self.available_upload = Some(UploadProbeResult::Probed {
                available_upload_kbps,
            });
        }

        Ok(())
    }

    fn handle_relay_outgoing(&mut self, kbps: u32) -> ClientResult<()> {
        if let Some(RelayStatus::Relay { .. }) = self.relay_status {
            self.relay_outgoing_kbps = Some(kbps);
        }

        Ok(())
    }

    fn handle_egress_bitrate_estimate(
        &mut self,
        kind: BweKind,
        keyframe_requests: &mut Vec<KeyframeRequest>,
    ) -> ClientResult<()> {
        match kind {
            BweKind::Twcc(bitrate) => {
                let mut target_layers: Vec<(Mid, Rid)> = Vec::new();

                debug!(
                    client_id = %self.id,
                    bitrate_bps = bitrate.as_u64(),
                    "egress TWCC bitrate estimate"
                );

                self.last_twcc_bitrate = Some(bitrate);

                let bitrate = u128::from(bitrate.as_u64());

                for track_out in self
                    .tracks_out
                    .iter()
                    .filter(|t| matches!(t.layer_mode, LayerMode::Auto))
                {
                    let TrackOutState::Open(mid) = track_out.state else {
                        continue;
                    };

                    if let Some(rid) = target_rid_for_bitrate(track_out, bitrate) {
                        target_layers.push((mid, rid));
                    }
                }

                for (mid, rid) in target_layers {
                    self.set_layer(mid, rid, keyframe_requests)?
                }
            }
            BweKind::Remb(..) => (),
            _ => (),
        };

        Ok(())
    }

    pub fn demote_leaf(&mut self) -> ClientResult<Vec<KeyframeRequest>> {
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
                    rid: to.chosen_rid(),
                    kind: KeyframeRequestKind::Fir,
                })
            })
            .collect();

        if let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) {
            let json = serde_json::to_string(&S2cDcPayload::Demote)?;
            channel.write(false, json.as_bytes())?;
        };

        Ok(keyframe_requests)
    }

    pub fn demote_relay(&mut self) -> ClientResult<()> {
        self.relay_status = None;

        self.available_upload = Some(UploadProbeResult::Failed { at: Instant::now() });

        self.relay_outgoing_kbps = None;

        if let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) {
            let json = serde_json::to_string(&S2cDcPayload::Demote)?;
            channel.write(false, json.as_bytes())?;
        };

        Ok(())
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

        let Some(available_layer) = track_in
            .available_simulcast_layers
            .iter()
            .find(|l| l.rid == rid)
            .copied()
        else {
            return Ok(());
        };

        let previous_rid = track_out.chosen_rid();

        if previous_rid == Some(rid) {
            return Ok(());
        };

        track_out.chosen_layer = Some(available_layer);

        info!(
            client_id = %self.id,
            mid = ?mid,
            previous_rid = ?previous_rid,
            new_rid = ?rid,
            layer_mode = ?track_out.layer_mode,
            "simulcast layer changed"
        );

        keyframe_requests.push(KeyframeRequest {
            mid: track_in.mid,
            rid: Some(rid),
            kind: str0m::media::KeyframeRequestKind::Fir,
        });

        if let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) {
            let payload = S2cDcPayload::LayerChanged { rid, mid };
            let Ok(json) = serde_json::to_string(&payload) else {
                error!("serde_json to_string failed: client_id={}", self.id);
                return Ok(());
            };

            if let Err(e) = channel.write(false, json.as_bytes()) {
                error!("send layer_changed via dc failed: {e}");
            };
        }

        Ok(())
    }

    fn set_layer_mode(
        &mut self,
        mid: Mid,
        layer_mode: LayerMode,
        keyframe_requests: &mut Vec<KeyframeRequest>,
    ) -> ClientResult<()> {
        let mut bwe_target_rid: Option<Rid> = None;

        for track_out in self
            .tracks_out
            .iter_mut()
            .filter(|t| t.state == TrackOutState::Open(mid))
        {
            track_out.layer_mode = layer_mode;

            if matches!(layer_mode, LayerMode::Auto)
                && let Some(bitrate) = self.last_twcc_bitrate
            {
                let bitrate = u128::from(bitrate.as_u64());
                let rid = target_rid_for_bitrate(track_out, bitrate);

                bwe_target_rid = rid;
            };
        }

        if let Some(rid) = bwe_target_rid {
            self.set_layer(mid, rid, keyframe_requests)?
        }

        Ok(())
    }

    pub fn perf_report(&mut self, rtt_ms: u32, loss_pct: f32) -> ClientResult<()> {
        const MAX_AGE: Duration = Duration::from_secs(60 * 5);
        const MAX_LEN: usize = 1000;

        let now = Instant::now();

        self.perf.push_back(PerfSample {
            rtt_ms,
            loss_pct,
            timestamp: now,
        });

        while self
            .perf
            .front()
            .is_some_and(|p| now.duration_since(p.timestamp) > MAX_AGE || self.perf.len() > MAX_LEN)
        {
            self.perf.pop_front();
        }

        Ok(())
    }

    pub fn probe_available_upload(&mut self) {
        if let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) {
            let payload = S2cDcPayload::ProbeAvailableUpload;
            let Ok(json) = serde_json::to_string(&payload) else {
                error!("serde_json to_string failed: client_id={}", self.id);
                return;
            };

            if let Err(e) = channel.write(false, json.as_bytes()) {
                error!("send probe_upload via dc failed: {e}");
                return;
            };

            self.available_upload = Some(UploadProbeResult::Probing {
                probed_at: (Instant::now()),
            });
        }
    }

    pub fn is_perf_healthy(&self) -> bool {
        const RTT_MS_CUTOFF: u32 = 30;
        const LOSS_PCT_CUTOFF: f32 = 10.0;
        const MIN_SAMPLES_LEN: u32 = 200;

        if self.perf.is_empty() {
            return false;
        }

        let mut rtt_ms_avg = 0;
        let mut loss_pct_avg = 0.0;
        let len = self.perf.len() as u32;

        for p in self.perf.iter() {
            rtt_ms_avg += p.rtt_ms;
            loss_pct_avg += p.loss_pct;
        }

        rtt_ms_avg /= len;
        loss_pct_avg /= len as f32;

        rtt_ms_avg < RTT_MS_CUTOFF && loss_pct_avg < LOSS_PCT_CUTOFF && len > MIN_SAMPLES_LEN
    }
}

fn is_layer_threshold_met(
    bitrate: u128,
    layer: &SimulcastLayerProfile,
    margin_percent: u128,
) -> bool {
    let max_br = u128::from(layer.max_br);
    let margin = max_br * margin_percent / 100;

    bitrate >= max_br + margin
}

fn target_rid_for_bitrate(track_out: &TrackOut, bitrate: u128) -> Option<Rid> {
    if let Some(track_in) = track_out.track_in.upgrade() {
        let rid = track_in
            .available_simulcast_layers
            .iter()
            .filter(|l| is_layer_threshold_met(bitrate, l, HEADROOM_MARGIN_PERCENT))
            .max_by_key(|l| l.max_br)
            .or_else(|| {
                track_in
                    .available_simulcast_layers
                    .iter()
                    .min_by_key(|l| l.max_br)
            })
            .and_then(|l| {
                let current_layer = track_out.chosen_layer?;

                if current_layer.max_br > l.max_br {
                    return Some(l.rid);
                }

                if is_layer_threshold_met(
                    bitrate,
                    l,
                    HEADROOM_MARGIN_PERCENT + UPSWITCH_MARGIN_PERCENT,
                ) {
                    Some(l.rid)
                } else {
                    None
                }
            });
        return rid;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use str0m::{
        Rtc,
        bwe::{Bitrate, BweKind},
        media::{KeyframeRequest, KeyframeRequestKind, MediaKind, Mid, Rid},
    };

    use crate::types::{
        Client, ClientRole, LayerMode, SimulcastLayerProfile, TrackIn, TrackOut, TrackOutState,
    };

    fn viewer_with_track_out() -> (Client, Arc<TrackIn>) {
        let mut client = Client::new(Rtc::new(Instant::now()), ClientRole::Viewer, vec![]);

        let streamer_mid = Mid::from("streamer-video");
        let viewer_mid = Mid::from("viewer-video");
        let low_rid = Rid::from("l");
        let high_rid = Rid::from("h");

        let track_in = Arc::new(TrackIn {
            origin: client.id,
            mid: streamer_mid,
            kind: MediaKind::Video,
            available_simulcast_layers: vec![
                SimulcastLayerProfile {
                    rid: low_rid,
                    max_br: 300_000,
                },
                SimulcastLayerProfile {
                    rid: high_rid,
                    max_br: 2_000_000,
                },
            ],
        });

        let track_out = TrackOut {
            track_in: Arc::downgrade(&track_in),
            state: TrackOutState::Open(viewer_mid),
            chosen_layer: track_in.default_layer(),
            layer_mode: LayerMode::Auto,
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

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(high_rid));
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

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(Rid::from("l")));
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

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(Rid::from("l")));
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

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(Rid::from("l")));
        assert!(keyframe_requests.is_empty());
    }

    #[test]
    fn set_layer_mode_updates_matching_track() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests = Vec::new();

        client
            .set_layer_mode(
                Mid::from("viewer-video"),
                LayerMode::Manual,
                &mut keyframe_requests,
            )
            .unwrap();

        assert!(matches!(client.tracks_out[0].layer_mode, LayerMode::Manual));
        assert!(keyframe_requests.is_empty());
    }

    #[test]
    fn bwe_switches_layer_in_auto_mode() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests = Vec::new();

        client
            .handle_egress_bitrate_estimate(
                BweKind::Twcc(Bitrate::bps(2_400_000)),
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(Rid::from("h")));
        assert_eq!(keyframe_requests.len(), 1);
        assert_eq!(keyframe_requests[0].mid, Mid::from("streamer-video"));
        assert_eq!(keyframe_requests[0].rid, Some(Rid::from("h")));
    }

    #[test]
    fn bwe_does_not_switch_layer_in_manual_mode() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests = Vec::new();

        client
            .set_layer_mode(
                Mid::from("viewer-video"),
                LayerMode::Manual,
                &mut keyframe_requests,
            )
            .unwrap();
        client
            .handle_egress_bitrate_estimate(
                BweKind::Twcc(Bitrate::bps(2_000_000)),
                &mut keyframe_requests,
            )
            .unwrap();
        client
            .set_layer_mode(
                Mid::from("viewer-video"),
                LayerMode::Manual,
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(Rid::from("l")));
        assert!(keyframe_requests.is_empty());
    }

    #[test]
    fn switching_to_auto_reevaluates_last_twcc_bitrate() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests = Vec::new();

        client
            .set_layer_mode(
                Mid::from("viewer-video"),
                LayerMode::Manual,
                &mut keyframe_requests,
            )
            .unwrap();
        client
            .handle_egress_bitrate_estimate(
                BweKind::Twcc(Bitrate::bps(2_400_000)),
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(Rid::from("l")));
        assert!(keyframe_requests.is_empty());

        client
            .set_layer_mode(
                Mid::from("viewer-video"),
                LayerMode::Auto,
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(Rid::from("h")));
        assert_eq!(keyframe_requests.len(), 1);
        assert_eq!(keyframe_requests[0].mid, Mid::from("streamer-video"));
        assert_eq!(keyframe_requests[0].rid, Some(Rid::from("h")));
    }

    #[test]
    fn bwe_falls_back_to_lowest_layer_when_estimate_is_too_low() {
        let (mut client, track_in) = viewer_with_track_out();
        let mut keyframe_requests = Vec::new();
        let high_layer = track_in
            .available_simulcast_layers
            .iter()
            .find(|layer| layer.rid == Rid::from("h"))
            .copied()
            .expect("high simulcast layer should exist");
        client.tracks_out[0].chosen_layer = Some(high_layer);

        client
            .handle_egress_bitrate_estimate(
                BweKind::Twcc(Bitrate::bps(100_000)),
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid(), Some(Rid::from("l")));
        assert_eq!(keyframe_requests.len(), 1);
        assert_eq!(keyframe_requests[0].rid, Some(Rid::from("l")));
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
