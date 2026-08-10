use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    time::Instant,
};

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use str0m::{
    Rtc,
    change::SdpPendingOffer,
    channel::ChannelId,
    media::{MediaKind, Mid, Rid},
};

use derive_more::Display;

#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum ClientRole {
    Streamer,
    Viewer,
}

#[derive(Debug)]
pub struct Client {
    pub id: ClientId,
    pub rtc: Rtc,
    pub role: ClientRole,
    pub pending: Option<SdpPendingOffer>,
    pub cid: Option<ChannelId>,
    pub tracks_in: Vec<TrackInEntry>,
    pub tracks_out: Vec<TrackOut>,
    pub relay_status: Option<RelayStatus>,
    pub perf: PerfWindow,
    pub available_upload: Option<UploadProbeResult>,
    pub relay_outgoing_kbps: Option<u32>,
    pub pending_simulcast_tracks: Vec<SimulcastTrack>,
    pub connected_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Display)]
pub struct ClientId(u64);

#[derive(Debug)]
pub struct RoomId(pub u64);

pub type PerfWindow = VecDeque<PerfSample>;

#[derive(Debug)]
pub struct PerfSample {
    pub rtt_ms: u32,
    pub loss_pct: f32,
    pub timestamp: Instant,
}

#[derive(Debug)]
pub enum UploadProbeResult {
    Probing { probed_at: Instant },
    Probed { available_upload_kbps: u32 },
    Failed { at: Instant },
}

#[derive(Deserialize, Debug, Clone, Copy)]
pub enum RoomState {
    Idle,
    Preview,
    Live,
}

#[derive(Debug)]
pub struct Room {
    pub streamer_id: Option<ClientId>,
    pub clients: Vec<Client>,
    pub state: RoomState,
}

pub struct Rooms(pub HashMap<u64, Room>);

#[derive(Debug)]
pub struct TrackIn {
    pub origin: ClientId,
    pub mid: Mid,
    pub kind: MediaKind,
    pub available_simulcast_layers: Vec<SimulcastLayerProfile>,
}

#[derive(Debug)]
pub struct TrackInEntry {
    pub id: Arc<TrackIn>,
    pub last_keyframe_request: Option<Instant>,
}

#[derive(Debug)]
pub struct TrackOut {
    pub track_in: Weak<TrackIn>,
    pub state: TrackOutState,
    pub chosen_layer: Option<SimulcastLayerProfile>,
    pub layer_mode: LayerMode,
}

#[derive(Debug, Clone, Copy)]
pub struct SimulcastLayerProfile {
    pub rid: Rid,
    pub max_br: u64,
}

#[derive(Debug, Clone)]
pub struct SimulcastTrack {
    pub mid: Mid,
    pub layers: Vec<SimulcastLayerProfile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOutState {
    ToOpen,
    Negotiating(Mid),
    Open(Mid),
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum LayerMode {
    Manual,
    Auto,
}

impl Client {
    pub fn new(
        rtc: Rtc,
        role: ClientRole,
        pending_simulcast_tracks: Vec<SimulcastTrack>,
    ) -> Client {
        static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
        let next_id = ID_COUNTER.fetch_add(1, Ordering::SeqCst);

        Client {
            id: ClientId(next_id),
            role,
            rtc,
            pending: None,
            cid: None,
            tracks_in: vec![],
            tracks_out: vec![],
            relay_status: None,
            perf: VecDeque::new(),
            available_upload: None,
            connected_at: Instant::now(),
            relay_outgoing_kbps: None,
            pending_simulcast_tracks,
        }
    }
}

impl TrackIn {
    pub fn default_layer(&self) -> Option<SimulcastLayerProfile> {
        self.available_simulcast_layers
            .iter()
            .min_by_key(|l| l.max_br)
            .copied()
    }

    pub fn highest_layer(&self) -> Option<SimulcastLayerProfile> {
        self.available_simulcast_layers
            .iter()
            .max_by_key(|l| l.max_br)
            .copied()
    }
}

impl TrackOut {
    pub fn chosen_rid(&self) -> Option<Rid> {
        self.chosen_layer.map(|layer| layer.rid)
    }
}

pub enum PollResult {
    Timeout(Instant),
    Disconnected,
}

pub enum SfuMessage {
    RegisterClient {
        rtc: Box<Rtc>,
        role: ClientRole,
        room_id: RoomId,
        simulcast_tracks: Vec<SimulcastTrack>,
        reply: SyncSender<Option<StatusCode>>,
    },
    CreateRoom {
        room_id: RoomId,
        reply: SyncSender<Option<()>>,
    },
    DeleteRoom {
        room_id: RoomId,
        reply: SyncSender<Option<()>>,
    },
    UpdateRoomState {
        room_id: RoomId,
        state: RoomState,
        reply: SyncSender<Option<()>>,
    },
    GetViews {
        room_id: RoomId,
        reply: SyncSender<Option<usize>>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum C2sDcPayload {
    Offer { sdp: String },
    Answer { sdp: String },
    SetLayer { mid: String, rid: String },
    SetLayerMode { mid: Mid, layer_mode: LayerMode },
    PerfReport { rtt_ms: u32, loss_pct: f32 },
    P2pOffer { sdp: String },
    P2pAnswer { sdp: String },
    P2pConnected,
    P2pDisconnected,
    AvailableUpload { available_upload_kbps: u32 },
    RelayOutgoing { kbps: u32 },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum S2cDcPayload {
    RequestOffer,
    P2pOffer { sdp: String },
    P2pAnswer { sdp: String },
    Demote,
    ProbeAvailableUpload,
}

#[derive(Debug)]
pub enum RelayStatus {
    Relay {
        leaf: ClientId,
    },

    Leaf {
        relay: ClientId,
        link_state: LinkState,
    },
}

#[derive(Debug)]
pub enum LinkState {
    Connecting,
    Connected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layer_selects_lowest_max_br_regardless_of_rid_and_order() {
        let track = TrackIn {
            origin: ClientId(0),
            mid: Mid::from("video"),
            kind: MediaKind::Video,
            available_simulcast_layers: vec![
                SimulcastLayerProfile {
                    rid: Rid::from("first-high"),
                    max_br: 2_000_000,
                },
                SimulcastLayerProfile {
                    rid: Rid::from("middle-low"),
                    max_br: 300_000,
                },
                SimulcastLayerProfile {
                    rid: Rid::from("last-medium"),
                    max_br: 1_000_000,
                },
            ],
        };

        let layer = track.default_layer().expect("default layer should exist");

        assert_eq!(layer.rid, Rid::from("middle-low"));
        assert_eq!(layer.max_br, 300_000);
    }

    #[test]
    fn default_layer_returns_none_without_simulcast_layers() {
        let track = TrackIn {
            origin: ClientId(0),
            mid: Mid::from("video"),
            kind: MediaKind::Video,
            available_simulcast_layers: vec![],
        };

        assert!(track.default_layer().is_none());
    }
}
