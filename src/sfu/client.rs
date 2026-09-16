use std::{
    cell::RefCell,
    collections::HashMap,
    net::UdpSocket,
    rc::Rc,
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
    config::tuning,
    metrics,
    sfu::error::{ClientError, ClientResult},
    types::{
        AvailableSimulcastLayer, BitrateEstimator, C2sDcPayload, Client, ClientId, ClientRole,
        LayerMode, LinkState, PerfSample, PollResult, PushOutcome, RelayPotential, RelayStatus,
        S2cDcPayload, SimulcastLayer, TrackIn, TrackInEntry, TrackOut, TrackOutState,
        UploadProbeResult,
    },
};

impl Client {
    pub fn handle_input(&mut self, input: Input) {
        if !self.rtc.is_alive() {
            return;
        }
        if let Err(e) = self.rtc.handle_input(input) {
            warn!("Client ({:?}) disconnected: {:?}", self, e);
            self.rtc.disconnect();
        }
    }

    pub fn poll_output(
        self: &mut Client,
        socket: &UdpSocket,
        new_tracks: &mut Vec<Rc<TrackIn>>,
        media_datas: &mut Vec<MediaData>,
        keyframe_requests: &mut Vec<KeyframeRequest>,
        p2p_sdps: &mut Vec<(ClientId, S2cDcPayload)>,
        connected_relays: &mut Vec<ClientId>,
        disconnected_relays: &mut Vec<ClientId>,
        should_reevaluate: &mut bool,
    ) -> ClientResult<PollResult> {
        let timeout = loop {
            match self.rtc.poll_output()? {
                Output::Timeout(v) => break v,
                Output::Transmit(v) => {
                    socket.send_to(&v.contents, v.destination)?;
                    metrics::sent(v.contents.len());
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
                            let mut layers: Vec<SimulcastLayer> = Vec::new();

                            let rids: Vec<Rid> = m
                                .simulcast
                                .map(|simulcast| simulcast.recv.iter().map(|l| l.rid).collect())
                                .unwrap_or_default();

                            for rid in rids {
                                layers.push(SimulcastLayer {
                                    rid,
                                    bitrate_estimator: RefCell::new(BitrateEstimator::new()),
                                });
                            }

                            let track_in = self.handle_media_added(m.mid, m.kind, layers);
                            new_tracks.push(track_in);
                        }
                    }
                    Event::MediaData(data) => {
                        metrics::media_data();

                        if self.role == ClientRole::Streamer
                            && let Some(rid) = data.rid
                            && let Some(track_in) =
                                self.tracks_in.iter_mut().find(|t| t.id.mid == data.mid)
                            && let Some(simulcast_layer) = track_in
                                .id
                                .available_simulcast_layers
                                .iter()
                                .find(|l| l.rid == rid)
                        {
                            let push_outcome = simulcast_layer
                                .bitrate_estimator
                                .borrow_mut()
                                .push(data.data.len(), Instant::now());

                            if matches!(push_outcome, PushOutcome::EstimateBecameAvailable) {
                                *should_reevaluate = true;
                            }
                        }

                        media_datas.push(data);
                    }
                    Event::ChannelOpen(cid, _label) => self.cid = Some(cid),
                    Event::ChannelData(data) => self.handle_channel_data(
                        data,
                        keyframe_requests,
                        p2p_sdps,
                        connected_relays,
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

    pub(crate) fn reevaluate_auto_layers(
        &mut self,
        keyframe_requests: &mut Vec<KeyframeRequest>,
    ) -> ClientResult<()> {
        let mut target_layers: Vec<_> = Vec::new();

        let Some(twcc_bitrate) = self.last_twcc_bitrate.map(|b| u128::from(b.as_u64())) else {
            return Ok(());
        };

        for track_out in self
            .tracks_out
            .iter()
            .filter(|t| matches!(t.layer_mode, LayerMode::Auto))
        {
            let TrackOutState::Open(mid) = track_out.state else {
                continue;
            };

            if let Some(rid) = target_rid_for_bitrate(track_out, twcc_bitrate) {
                target_layers.push((mid, rid));
            }
        }

        for (mid, rid) in target_layers {
            self.set_layer(mid, rid, keyframe_requests)?;
        }

        Ok(())
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
            metrics::media_write(data.data.len());
        }
        Ok(())
    }

    /// What a leaf would have been sent had its relay not been carrying it. The
    /// same match the fanout applies, because a leaf takes one layer and
    /// counting every frame would inflate the figure by the layer count.
    pub fn count_relay_saved(&self, datas: &Vec<MediaData>) {
        for data in datas {
            if self.matching_viewer_mid(data.mid, data.rid).is_some() {
                metrics::relay_saved(data.data.len());
            }
        }
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
        new_tracks: &mut Vec<Rc<TrackIn>>,
        media_datas: &mut Vec<MediaData>,
        keyframe_requests: &mut Vec<KeyframeRequest>,
        p2p_sdps: &mut Vec<(ClientId, S2cDcPayload)>,
        connected_relays: &mut Vec<ClientId>,
        disconnected_relays: &mut Vec<ClientId>,
        should_reevaluate: &mut bool,
    ) -> ClientResult<PollResult> {
        self.renegotiate()?;
        let result = self.poll_output(
            socket,
            new_tracks,
            media_datas,
            keyframe_requests,
            p2p_sdps,
            connected_relays,
            disconnected_relays,
            should_reevaluate,
        )?;

        Ok(result)
    }

    pub fn request_sdp_offer(&mut self) -> ClientResult<()> {
        let payload = S2cDcPayload::RequestOffer {};

        self.send_payload_via_dc(payload)?;

        Ok(())
    }

    pub fn calc_desired_bitrate(&self) -> Option<Bitrate> {
        let mut total_bitrate: Option<u128> = None;

        self.tracks_out.iter().for_each(|to| {
            if let Some(ti) = to.track_in.upgrade()
                && let Some(highest_estimated_bps) = ti.highest_estimated_bps()
            {
                *total_bitrate.get_or_insert_default() += u128::from(highest_estimated_bps);
            };
        });

        if let Some(mut total_bitrate) = total_bitrate {
            let bitrate_margin = total_bitrate
                * (tuning().bitrate.headroom_margin_percent
                    + tuning().bitrate.upswitch_margin_percent)
                / 100;

            total_bitrate += bitrate_margin;

            if let Ok(desired_bitrate) = u64::try_from(total_bitrate) {
                return Some(Bitrate::bps(desired_bitrate));
            } else {
                warn!("bitrate calc overflow {total_bitrate}");
            }
        }

        None
    }

    pub fn relay_potential(&self) -> RelayPotential {
        let cutoff = tuning().relay.available_upload_cutoff_kbps;

        match self.available_upload {
            Some(UploadProbeResult::Probed {
                available_upload_kbps,
            }) => {
                if available_upload_kbps > cutoff {
                    RelayPotential::Qualified
                } else {
                    RelayPotential::Never
                }
            }
            _ => RelayPotential::Unknown,
        }
    }

    fn handle_media_added(
        &mut self,
        mid: Mid,
        kind: MediaKind,
        simulcast_layers: Vec<SimulcastLayer>,
    ) -> Rc<TrackIn> {
        let track_in = Rc::new(TrackIn {
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
        connected_relays: &mut Vec<ClientId>,
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
            C2sDcPayload::Answer { sdp } => self.handle_answer(&sdp, keyframe_requests),
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
                self.set_layer_mode(Mid::from(mid.as_str()), layer_mode, keyframe_requests)
            }
            C2sDcPayload::PerfReport { rtt_ms, loss_pct } => self.perf_report(rtt_ms, loss_pct),
            C2sDcPayload::P2pOffer { sdp } => {
                self.handle_p2p_sdp(S2cDcPayload::P2pOffer { sdp }, p2p_sdps)
            }
            C2sDcPayload::P2pAnswer { sdp } => {
                self.handle_p2p_sdp(S2cDcPayload::P2pAnswer { sdp }, p2p_sdps)
            }
            C2sDcPayload::P2pConnected => {
                if let Ok(Some(relay_id)) = self.handle_p2p_connected() {
                    connected_relays.push(relay_id);
                };

                Ok(())
            }
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

    fn handle_answer(
        &mut self,
        answer: &str,
        keyframe_requests: &mut Vec<KeyframeRequest>,
    ) -> ClientResult<()> {
        let answer = SdpAnswer::from_sdp_string(answer)?;
        let bitrate = self.last_twcc_bitrate;
        let mut payloads = Vec::new();

        let mut bwe_target_layer: Option<(Mid, Rid)> = None;

        if let Some(pending) = self.pending.take() {
            self.rtc.sdp_api().accept_answer(pending, answer)?;

            for track_out in &mut self.tracks_out {
                let TrackOutState::Negotiating(mid) = track_out.state else {
                    continue;
                };
                track_out.state = TrackOutState::Open(mid);

                if let Some(track_in) = track_out.track_in.upgrade() {
                    let available_simulcast_layers: Vec<_> = track_in
                        .available_simulcast_layers
                        .iter()
                        .map(|l| AvailableSimulcastLayer {
                            rid: l.rid.to_string(),
                            bitrate_estimate: l.estimate_bps(),
                        })
                        .collect();

                    let payload = S2cDcPayload::LayerStatus {
                        mid: mid.to_string(),
                        available_simulcast_layers,
                        chosen_layer: track_out.chosen_rid.map(|rid| rid.to_string()),
                        layer_mode: track_out.layer_mode,
                    };

                    payloads.push(payload);
                }

                if let Some(bitrate) = bitrate {
                    let bitrate = u128::from(bitrate.as_u64());

                    let rid = target_rid_for_bitrate(track_out, bitrate);

                    if let Some(rid) = rid {
                        bwe_target_layer = Some((mid, rid));
                    }
                };
            }
        }

        for payload in payloads {
            if let Err(e) = self.send_payload_via_dc(payload) {
                error!("send_payload_via_dc failed error: {e}");
            };
        }

        if let Some((mid, rid)) = bwe_target_layer {
            self.set_layer(mid, rid, keyframe_requests)?;
        }

        Ok(())
    }

    fn handle_p2p_sdp(
        &self,
        sdp: S2cDcPayload,
        p2p_sdps: &mut Vec<(ClientId, S2cDcPayload)>,
    ) -> ClientResult<()> {
        match self.relay_status {
            Some(RelayStatus::Relay { leaf, .. }) => {
                p2p_sdps.push((leaf, sdp));
            }
            Some(RelayStatus::Leaf {
                relay,
                link_state: LinkState::Connecting { .. },
            }) => {
                p2p_sdps.push((relay, sdp));
            }
            _ => (),
        };
        Ok(())
    }

    fn handle_p2p_connected(&mut self) -> ClientResult<Option<ClientId>> {
        if let Some(RelayStatus::Leaf { link_state, relay }) = &mut self.relay_status {
            *link_state = LinkState::Connected;
            Ok(Some(*relay))
        } else {
            Ok(None)
        }
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
                debug!(
                    client_id = %self.id,
                    bitrate_bps = bitrate.as_u64(),
                    "egress TWCC bitrate estimate"
                );
                self.last_twcc_bitrate = Some(bitrate);

                self.reevaluate_auto_layers(keyframe_requests)?;
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
                    rid: to.chosen_rid,
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

        let Some(available_rid) = track_in
            .available_simulcast_layers
            .iter()
            .find(|l| l.rid == rid)
            .map(|l| l.rid)
        else {
            return Ok(());
        };

        let previous_rid = track_out.chosen_rid;

        if previous_rid == Some(rid) {
            return Ok(());
        };

        track_out.chosen_rid = Some(available_rid);

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

        let payload = S2cDcPayload::LayerChanged {
            rid: rid.to_string(),
            mid: mid.to_string(),
        };

        if let Err(e) = self.send_payload_via_dc(payload) {
            error!("send_payload_via_dc failed error: {e}");
        };

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
        let now = Instant::now();

        self.perf.push_back(PerfSample {
            rtt_ms,
            loss_pct,
            timestamp: now,
        });

        while self.perf.front().is_some_and(|p| {
            now.duration_since(p.timestamp) > tuning().perf.window_max_age()
                || self.perf.len() > tuning().perf.window_max_len
        }) {
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

        rtt_ms_avg < tuning().perf.rtt_ms_cutoff
            && loss_pct_avg < tuning().perf.loss_pct_cutoff
            && len > tuning().perf.min_samples_len
    }

    fn send_payload_via_dc(&mut self, payload: S2cDcPayload) -> ClientResult<()> {
        let mut channel = self
            .cid
            .and_then(|id| self.rtc.channel(id))
            .ok_or(ClientError::ChannelNotFound)?;

        let json = serde_json::to_string(&payload)?;

        channel.write(false, json.as_bytes())?;

        Ok(())
    }
}

fn is_layer_threshold_met(bitrate: u128, bitrate_estimate: u64, margin_percent: u128) -> bool {
    let bitrate_estimate = bitrate_estimate as u128;

    let margin = bitrate_estimate * margin_percent / 100;

    bitrate >= bitrate_estimate + margin
}

fn target_rid_for_bitrate(track_out: &TrackOut, bitrate: u128) -> Option<Rid> {
    if let Some(track_in) = track_out.track_in.upgrade() {
        let estimates: Vec<(Rid, u64)> = track_in
            .available_simulcast_layers
            .iter()
            .filter_map(|l| l.estimate_bps().map(|estimate_bps| (l.rid, estimate_bps)))
            .collect();

        let rid = estimates
            .iter()
            .filter(|(_, estimate)| {
                is_layer_threshold_met(bitrate, *estimate, tuning().bitrate.headroom_margin_percent)
            })
            .max_by_key(|(_, estimate)| estimate)
            .or_else(|| estimates.iter().min_by_key(|(_, estimate)| estimate))
            .and_then(|(rid, estimate)| {
                let current_rid = track_out.chosen_rid?;
                let current_layer_estimated_bps = estimates
                    .iter()
                    .find(|(rid, _)| *rid == current_rid)
                    .map(|(_, estimate)| *estimate)?;

                if current_layer_estimated_bps > *estimate {
                    return Some(*rid);
                }

                if is_layer_threshold_met(
                    bitrate,
                    *estimate,
                    tuning().bitrate.headroom_margin_percent
                        + tuning().bitrate.upswitch_margin_percent,
                ) {
                    Some(*rid)
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
    use std::{cell::RefCell, rc::Rc, time::Instant};

    use str0m::{
        Rtc,
        bwe::{Bitrate, BweKind},
        media::{KeyframeRequest, KeyframeRequestKind, MediaKind, Mid, Rid},
    };
    use uuid::Uuid;

    use crate::types::{
        BitrateEstimator, Client, ClientRole, LayerMode, SimulcastLayer, TrackIn, TrackOut,
        TrackOutState,
    };

    fn simulcast_layer(rid: Rid, estimated_bps: u64) -> SimulcastLayer {
        SimulcastLayer {
            rid,
            bitrate_estimator: RefCell::new(BitrateEstimator::with_estimated_bps(estimated_bps)),
        }
    }

    fn viewer_with_track_out() -> (Client, Rc<TrackIn>) {
        let mut client = Client::new(Rtc::new(Instant::now()), ClientRole::Viewer, Uuid::new_v4());

        let streamer_mid = Mid::from("streamer-video");
        let viewer_mid = Mid::from("viewer-video");
        let low_rid = Rid::from("l");
        let high_rid = Rid::from("h");

        let track_in = Rc::new(TrackIn {
            origin: client.id,
            mid: streamer_mid,
            kind: MediaKind::Video,
            available_simulcast_layers: vec![
                simulcast_layer(high_rid, 2_000_000),
                simulcast_layer(low_rid, 300_000),
            ],
        });

        let track_out = TrackOut {
            track_in: Rc::downgrade(&track_in),
            state: TrackOutState::Open(viewer_mid),
            chosen_rid: track_in.default_rid(),
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

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("h")));
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

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
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

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert!(keyframe_requests.is_empty());

        client
            .set_layer_mode(
                Mid::from("viewer-video"),
                LayerMode::Auto,
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("h")));
        assert_eq!(keyframe_requests.len(), 1);
        assert_eq!(keyframe_requests[0].mid, Mid::from("streamer-video"));
        assert_eq!(keyframe_requests[0].rid, Some(Rid::from("h")));
    }

    #[test]
    fn bwe_falls_back_to_lowest_layer_when_estimate_is_too_low() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests = Vec::new();
        client.tracks_out[0].chosen_rid = Some(Rid::from("h"));

        client
            .handle_egress_bitrate_estimate(
                BweKind::Twcc(Bitrate::bps(100_000)),
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
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
