use std::{
    collections::HashMap,
    net::UdpSocket,
    ops::{Deref, DerefMut},
    time::Instant,
};

use uuid::Uuid;

use str0m::{
    Event, IceConnectionState, Rtc,
    bwe::{Bitrate, BweKind},
    change::SdpAnswer,
    channel::ChannelData,
    media::{Direction, KeyframeRequest, KeyframeRequestKind, MediaData, MediaKind, Mid, Rid},
};
use tracing::{debug, error, info, warn};

use crate::{
    config::tuning,
    metrics,
    sfu::error::{ClientError, ClientResult},
    types::{
        AvailableSimulcastLayer, C2sDcPayload, ClientId, LayerMode, LinkState, Peer, PollResult,
        RelayState, RelayStatus, S2cDcPayload, TickResult, TrackOut, TrackOutState,
        UploadProbeResult, Viewer, ViewerEffects, Viewers,
    },
};

impl Viewers {
    pub fn new() -> Viewers {
        Viewers(HashMap::new())
    }
}

impl Viewer {
    pub fn new(rtc: Rtc, session_id: Uuid) -> Viewer {
        Viewer {
            id: ClientId::next(),
            peer: Peer::new(rtc, session_id),
            relay_state: RelayState::new(),
            tracks_out: Vec::new(),
            last_twcc_bitrate: None,
        }
    }

    pub fn tick(
        &mut self,
        effects: &mut ViewerEffects,
        socket: &UdpSocket,
    ) -> ClientResult<TickResult> {
        self.renegotiate()?;

        loop {
            match self.peer.poll(socket)? {
                PollResult::Timeout(at) => return Ok(TickResult::Timeout(at)),
                PollResult::Event(event) => match event {
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        self.peer.rtc.disconnect();
                        return Ok(TickResult::Disconnected);
                    }

                    Event::ChannelData(data) => self.handle_channel_data(data, effects)?,

                    Event::KeyframeRequest(request) => {
                        if let Some(translated) = self.translate_keyframe_request(request) {
                            effects.keyframe_requests.push(translated);
                        }
                    }

                    Event::EgressBitrateEstimate(kind) => {
                        self.handle_egress_bitrate_estimate(kind, &mut effects.keyframe_requests)?
                    }
                    _ => {}
                },
            }
        }
    }

    pub fn renegotiate(&mut self) -> ClientResult<()> {
        if self.peer.pending.is_some() || self.peer.cid.is_none() {
            return Ok(());
        }

        let mut change = self.peer.rtc.sdp_api();

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
                .peer
                .cid
                .and_then(|id| self.peer.rtc.channel(id))
                .ok_or(ClientError::ChannelNotFound)?;

            let json = serde_json::to_string(&offer)?;

            channel.write(false, json.as_bytes())?;

            self.peer.pending = Some(pending);
        }
        Ok(())
    }

    fn handle_channel_data(
        &mut self,
        data: ChannelData,
        effects: &mut ViewerEffects,
    ) -> ClientResult<()> {
        let ViewerEffects {
            keyframe_requests,
            p2p_sdps,
            connected_relays,
            disconnected_relays,
        } = effects;

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
            C2sDcPayload::Offer { sdp } => self.peer.handle_offer(&sdp),
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
            C2sDcPayload::PerfReport { rtt_ms, loss_pct } => {
                self.relay_state.perf_report(rtt_ms, loss_pct)
            }
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
            } => self
                .relay_state
                .handle_available_upload(available_upload_kbps),
            C2sDcPayload::RelayOutgoing { kbps } => self.relay_state.handle_relay_outgoing(kbps),
        }
    }

    fn translate_keyframe_request(&self, request: KeyframeRequest) -> Option<KeyframeRequest> {
        self.tracks_out.iter().find_map(|t| {
            if t.state != TrackOutState::Open(request.mid) {
                return None;
            };

            let streamer_mid = t.track_in.upgrade()?.mid;

            Some(KeyframeRequest {
                mid: streamer_mid,
                rid: t.target_rid.or(t.chosen_rid),
                kind: request.kind,
            })
        })
    }

    pub fn handle_media_datas(&mut self, datas: &Vec<MediaData>) -> ClientResult<()> {
        for data in datas {
            let Some((mid, track_out)) = matching_viewer_mid(&mut self.tracks_out, data) else {
                continue;
            };

            let Some(writer) = self.peer.rtc.writer(mid) else {
                continue;
            };

            let Some(pt) = writer.match_params(data.params) else {
                continue;
            };

            writer.write(pt, data.network_time, data.time, data.data.clone())?;

            if track_out.promote_target(data)
                && let Some(rid) = data.rid
            {
                let payload = S2cDcPayload::LayerChanged {
                    rid: rid.to_string(),
                    mid: mid.to_string(),
                };

                if let Err(e) = self.peer.send_payload_via_dc(payload) {
                    error!("send_payload_via_dc failed error: {e}");
                };
            }

            metrics::media_write(data.data.len());
            metrics::forward_delay(data.network_time.elapsed());
        }
        Ok(())
    }

    /// What a leaf would have been sent had its relay not been carrying it. The
    /// same match the fanout applies, because a leaf takes one layer and
    /// counting every frame would inflate the figure by the layer count.
    pub fn count_relay_saved(&mut self, datas: &Vec<MediaData>) {
        for data in datas {
            if matching_viewer_mid(&mut self.tracks_out, data).is_some() {
                metrics::relay_saved(data.data.len());
            }
        }
    }

    pub fn request_sdp_offer(&mut self) -> ClientResult<()> {
        let payload = S2cDcPayload::RequestOffer {};

        self.peer.send_payload_via_dc(payload)?;

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

    fn handle_p2p_connected(&mut self) -> ClientResult<Option<ClientId>> {
        if let Some(RelayStatus::Leaf { link_state, relay }) = &mut self.relay_state.relay_status {
            *link_state = LinkState::Connected;
            Ok(Some(*relay))
        } else {
            Ok(None)
        }
    }

    pub fn demote_leaf(&mut self) -> ClientResult<Vec<KeyframeRequest>> {
        self.relay_state.relay_status = None;

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
                    rid: to.target_rid.or(to.chosen_rid),
                    kind: KeyframeRequestKind::Fir,
                })
            })
            .collect();

        if let Some(mut channel) = self.peer.cid.and_then(|id| self.peer.rtc.channel(id)) {
            let json = serde_json::to_string(&S2cDcPayload::Demote)?;
            channel.write(false, json.as_bytes())?;
        };

        Ok(keyframe_requests)
    }

    pub fn demote_relay(&mut self) -> ClientResult<()> {
        self.relay_state.relay_status = None;

        self.relay_state.available_upload = Some(UploadProbeResult::Failed { at: Instant::now() });

        self.relay_state.relay_outgoing_kbps = None;

        if let Some(mut channel) = self.peer.cid.and_then(|id| self.peer.rtc.channel(id)) {
            let json = serde_json::to_string(&S2cDcPayload::Demote)?;
            channel.write(false, json.as_bytes())?;
        };

        Ok(())
    }

    pub fn probe_available_upload(&mut self) {
        if let Some(mut channel) = self.peer.cid.and_then(|id| self.peer.rtc.channel(id)) {
            let payload = S2cDcPayload::ProbeAvailableUpload;
            let Ok(json) = serde_json::to_string(&payload) else {
                error!("serde_json to_string failed: client_id={}", self.id);
                return;
            };

            if let Err(e) = channel.write(false, json.as_bytes()) {
                error!("send probe_upload via dc failed: {e}");
                return;
            };

            self.relay_state.available_upload = Some(UploadProbeResult::Probing {
                probed_at: (Instant::now()),
            });
        }
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

        if let Some(pending) = self.peer.pending.take() {
            self.peer.rtc.sdp_api().accept_answer(pending, answer)?;

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
                        current_layer: track_out.chosen_rid.map(|rid| rid.to_string()),
                        target_layer: track_out.target_rid.map(|rid| rid.to_string()),
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
            if let Err(e) = self.peer.send_payload_via_dc(payload) {
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
        match self.relay_state.relay_status {
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

    fn handle_p2p_disconnected(
        &mut self,
        keyframe_requests: &mut Vec<KeyframeRequest>,
        disconnected_relays: &mut Vec<ClientId>,
    ) -> ClientResult<()> {
        if let Some(RelayStatus::Leaf { relay, link_state }) = &self.relay_state.relay_status
            && matches!(link_state, LinkState::Connected)
        {
            disconnected_relays.push(*relay);
            for kf in self.demote_leaf()? {
                keyframe_requests.push(kf);
            }
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
            track_out.target_rid = None;

            return Ok(());
        };

        if track_out.target_rid == Some(rid) {
            return Ok(());
        }

        track_out.target_rid = Some(available_rid);

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
}

impl Deref for Viewers {
    type Target = HashMap<ClientId, Viewer>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Viewers {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn matching_viewer_mid<'a>(
    tracks_out: &'a mut [TrackOut],
    data: &MediaData,
) -> Option<(Mid, &'a mut TrackOut)> {
    tracks_out.iter_mut().find_map(|track_out| {
        let track_in = track_out.track_in.upgrade()?;
        if track_in.mid != data.mid {
            return None;
        };

        let TrackOutState::Open(viewer_mid) = track_out.state else {
            return None;
        };

        if !track_out.should_forward(data) {
            return None;
        }

        Some((viewer_mid, track_out))
    })
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
                let current_rid = track_out.target_rid.or(track_out.chosen_rid)?;
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
    use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};

    use str0m::{
        Rtc,
        bwe::{Bitrate, BweKind},
        format::{Codec, CodecExtra, CodecSpec, FormatParams, H264CodecExtra, PayloadParams},
        media::{
            Frequency, KeyframeRequest, KeyframeRequestKind, MediaData, MediaKind, MediaTime, Mid,
            Pt, Rid,
        },
        rtp::SeqNo,
    };
    use uuid::Uuid;

    use super::matching_viewer_mid;
    use crate::types::{
        BitrateEstimator, LayerMode, SimulcastLayer, TrackIn, TrackOut, TrackOutState, Viewer,
    };

    fn simulcast_layer(rid: Rid, estimated_bps: u64) -> SimulcastLayer {
        SimulcastLayer {
            rid,
            bitrate_estimator: RefCell::new(BitrateEstimator::with_estimated_bps(estimated_bps)),
        }
    }

    /// A viewer settled on the low layer, with no switch pending.
    fn viewer_with_track_out() -> (Viewer, Rc<TrackIn>) {
        let mut client = Viewer::new(Rtc::new(Instant::now()), Uuid::new_v4());

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
            target_rid: None,
            layer_mode: LayerMode::Auto,
        };

        client.tracks_out.push(track_out);
        (client, track_in)
    }

    /// A frame of the streamer's video track. Only the fields the forwarding
    /// decision reads carry meaning.
    fn frame(mid: &str, rid: Option<&str>, keyframe: bool) -> MediaData {
        let pt = Pt::from(96);

        MediaData {
            mid: Mid::from(mid),
            pt,
            rid: rid.map(Rid::from),
            params: PayloadParams::new(
                pt,
                None,
                CodecSpec {
                    codec: Codec::H264,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams::default(),
                },
            ),
            time: MediaTime::new(0, Frequency::NINETY_KHZ),
            network_time: Instant::now(),
            seq_range: SeqNo::default()..=SeqNo::default(),
            contiguous: true,
            data: Arc::from(Vec::new()),
            ext_vals: Default::default(),
            codec_extra: CodecExtra::H264(H264CodecExtra {
                is_keyframe: keyframe,
            }),
            last_sender_info: None,
            audio_start_of_talk_spurt: false,
        }
    }

    fn forwarded_to(client: &mut Viewer, data: &MediaData) -> Option<Mid> {
        matching_viewer_mid(&mut client.tracks_out, data).map(|(mid, _)| mid)
    }

    /// What `handle_media_datas` does around the write, without a peer to write
    /// to: the decision, then the commit a successful write would make.
    fn deliver(client: &mut Viewer, data: &MediaData) -> Option<bool> {
        matching_viewer_mid(&mut client.tracks_out, data)
            .map(|(_, track_out)| track_out.promote_target(data))
    }

    #[test]
    fn set_layer_records_a_target_and_requests_its_keyframe() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests: Vec<KeyframeRequest> = Vec::new();
        let high_rid = Rid::from("h");

        client
            .set_layer(Mid::from("viewer-video"), high_rid, &mut keyframe_requests)
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert_eq!(client.tracks_out[0].target_rid, Some(high_rid));
        assert_eq!(keyframe_requests.len(), 1);
        assert_eq!(keyframe_requests[0].mid, Mid::from("streamer-video"));
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
        assert_eq!(client.tracks_out[0].target_rid, None);
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
        assert_eq!(client.tracks_out[0].target_rid, None);
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
        assert_eq!(client.tracks_out[0].target_rid, None);
        assert!(keyframe_requests.is_empty());
    }

    #[test]
    fn set_layer_back_to_the_current_layer_cancels_the_switch() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests: Vec<KeyframeRequest> = Vec::new();
        let viewer_mid = Mid::from("viewer-video");

        client
            .set_layer(viewer_mid, Rid::from("h"), &mut keyframe_requests)
            .unwrap();
        client
            .set_layer(viewer_mid, Rid::from("l"), &mut keyframe_requests)
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert_eq!(client.tracks_out[0].target_rid, None);

        // The high keyframe the cancelled switch asked for no longer moves it.
        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("h"), true)),
            None
        );
        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
    }

    #[test]
    fn set_layer_to_the_pending_target_asks_once() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests: Vec<KeyframeRequest> = Vec::new();
        let viewer_mid = Mid::from("viewer-video");

        client
            .set_layer(viewer_mid, Rid::from("h"), &mut keyframe_requests)
            .unwrap();
        client
            .set_layer(viewer_mid, Rid::from("h"), &mut keyframe_requests)
            .unwrap();

        assert_eq!(client.tracks_out[0].target_rid, Some(Rid::from("h")));
        assert_eq!(keyframe_requests.len(), 1);
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
    fn bwe_targets_a_higher_layer_in_auto_mode() {
        let (mut client, _track_in) = viewer_with_track_out();
        let mut keyframe_requests = Vec::new();

        client
            .handle_egress_bitrate_estimate(
                BweKind::Twcc(Bitrate::bps(2_400_000)),
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert_eq!(client.tracks_out[0].target_rid, Some(Rid::from("h")));
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
        assert_eq!(client.tracks_out[0].target_rid, None);
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

        assert_eq!(client.tracks_out[0].target_rid, None);
        assert!(keyframe_requests.is_empty());

        client
            .set_layer_mode(
                Mid::from("viewer-video"),
                LayerMode::Auto,
                &mut keyframe_requests,
            )
            .unwrap();

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert_eq!(client.tracks_out[0].target_rid, Some(Rid::from("h")));
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

        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("h")));
        assert_eq!(client.tracks_out[0].target_rid, Some(Rid::from("l")));
        assert_eq!(keyframe_requests.len(), 1);
        assert_eq!(keyframe_requests[0].rid, Some(Rid::from("l")));
    }

    #[test]
    fn media_routed_only_when_rid_matches_chosen() {
        let (mut client, _track_in) = viewer_with_track_out();
        let viewer_mid = Mid::from("viewer-video");

        let cases = [
            (Some("l"), Some(viewer_mid)),
            (Some("h"), None),
            (None, None),
        ];

        for (incoming_rid, expected) in cases {
            let data = frame("streamer-video", incoming_rid, false);

            assert_eq!(
                forwarded_to(&mut client, &data),
                expected,
                "incoming rid {incoming_rid:?}"
            );
        }
    }

    #[test]
    fn media_dropped_for_unknown_mid() {
        let (mut client, _track_in) = viewer_with_track_out();

        let data = frame("unknown-video", Some("l"), true);

        assert_eq!(forwarded_to(&mut client, &data), None);
    }

    #[test]
    fn a_delta_frame_of_the_target_does_not_switch() {
        let (mut client, _track_in) = viewer_with_track_out();
        client.tracks_out[0].target_rid = Some(Rid::from("h"));

        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("h"), false)),
            None,
            "a delta frame of the target is not forwarded"
        );
        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("l"), false)),
            Some(false),
            "the current layer keeps flowing"
        );
        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert_eq!(client.tracks_out[0].target_rid, Some(Rid::from("h")));
    }

    #[test]
    fn a_keyframe_of_another_layer_does_not_switch() {
        let (mut client, _track_in) = viewer_with_track_out();
        client.tracks_out[0].target_rid = Some(Rid::from("m"));

        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("l"), true)),
            Some(false),
            "a keyframe of the current layer is forwarded but switches nothing"
        );
        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("h"), true)),
            None,
            "a keyframe of a third layer is not forwarded"
        );
        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
        assert_eq!(client.tracks_out[0].target_rid, Some(Rid::from("m")));
    }

    #[test]
    fn a_keyframe_of_the_target_switches() {
        let (mut client, _track_in) = viewer_with_track_out();
        client.tracks_out[0].target_rid = Some(Rid::from("h"));

        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("h"), true)),
            Some(true),
            "the keyframe is forwarded as the first frame of the new layer"
        );
        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("h")));
        assert_eq!(client.tracks_out[0].target_rid, None);

        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("h"), false)),
            Some(false)
        );
        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("l"), false)),
            None,
            "the old layer stops"
        );
    }

    #[test]
    fn a_new_viewer_starts_on_a_keyframe() {
        let (mut client, track_in) = viewer_with_track_out();
        client.tracks_out[0].chosen_rid = None;
        client.tracks_out[0].target_rid = track_in.default_rid();

        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("l"), false)),
            None
        );
        assert_eq!(
            deliver(&mut client, &frame("streamer-video", Some("l"), true)),
            Some(true)
        );
        assert_eq!(client.tracks_out[0].chosen_rid, Some(Rid::from("l")));
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
    fn keyframe_request_asks_for_the_pending_target() {
        let (mut client, _track_in) = viewer_with_track_out();
        client.tracks_out[0].target_rid = Some(Rid::from("h"));

        let translated = client
            .translate_keyframe_request(KeyframeRequest {
                mid: Mid::from("viewer-video"),
                rid: None,
                kind: KeyframeRequestKind::Pli,
            })
            .expect("request should map to the streamer track");

        assert_eq!(translated.rid, Some(Rid::from("h")));
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
