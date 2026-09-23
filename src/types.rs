use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    fmt,
    net::SocketAddr,
    rc::{Rc, Weak},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    time::Instant,
};

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use str0m::{
    Rtc,
    bwe::Bitrate,
    change::SdpPendingOffer,
    channel::ChannelId,
    media::{KeyframeRequest, MediaData, MediaKind, Mid, Rid},
};

use derive_more::{Debug, Display, Eq};
use uuid::Uuid;

use crate::{
    config::tuning,
    utils::string::{hash_string, random_string},
};

const STREAM_KEY_LEN: usize = 32;
const ROOM_ID_LEN: usize = 12;

#[derive(Debug)]
pub struct Peer {
    pub rtc: Rtc,
    pub pending: Option<SdpPendingOffer>,
    pub cid: Option<ChannelId>,
    pub connected_at: Instant,
    pub session_id: Uuid,
}

#[derive(Debug)]
pub struct RelayState {
    pub relay_status: Option<RelayStatus>,
    pub perf: PerfWindow,
    pub available_upload: Option<UploadProbeResult>,
    pub relay_outgoing_kbps: Option<u32>,
}

#[derive(Debug)]
pub struct Viewer {
    pub id: ClientId,
    pub peer: Peer,
    pub tracks_out: Vec<TrackOut>,
    pub relay_state: RelayState,
    pub last_twcc_bitrate: Option<Bitrate>,
}

#[derive(Debug)]
pub struct Streamer {
    pub id: ClientId,
    pub peer: Peer,
    pub tracks_in: Vec<TrackInEntry>,
}

#[derive(Debug)]
pub struct Viewers(pub HashMap<ClientId, Viewer>);

#[derive(Debug, Clone, Copy, PartialEq, Display, Hash, Eq)]
pub struct ClientId(u64);

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct RoomId(pub String);

pub struct StreamKey(pub String);

pub type PerfWindow = VecDeque<PerfSample>;

#[derive(Debug)]
pub struct PerfSample {
    pub rtt_ms: u32,
    pub loss_pct: f32,
    pub timestamp: Instant,
}

#[derive(Debug)]
pub struct BitrateEstimator {
    history: VecDeque<(usize, Instant)>,
    accumulated_bytes: u64,
    started_at: Option<Instant>,
    is_estimate_available: bool,
}

pub enum PushOutcome {
    EstimateBecameAvailable,
    NoChange,
}

#[derive(Debug)]
pub enum UploadProbeResult {
    Probing { probed_at: Instant },
    Probed { available_upload_kbps: u32 },
    Failed { at: Instant },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum RoomState {
    Idle,
    Preview,
    Live,
}

#[derive(Debug)]
pub struct Room {
    pub streamer: Option<Streamer>,
    pub viewers: Viewers,
    pub state: RoomState,
    pub hashed_stream_key: [u8; 32],
}

pub struct Rooms {
    pub rooms: HashMap<RoomId, Room>,
    pub socket_routes: SocketRoutes,
}

#[derive(Debug)]
pub struct TrackIn {
    pub origin: ClientId,
    pub mid: Mid,
    pub kind: MediaKind,
    pub available_simulcast_layers: Vec<SimulcastLayer>,
}

#[derive(Debug)]
pub struct TrackInEntry {
    pub id: Rc<TrackIn>,
    pub last_keyframe_requested_at: HashMap<Option<Rid>, Instant>,
}

#[derive(Debug)]
pub struct TrackOut {
    pub track_in: Weak<TrackIn>,
    pub state: TrackOutState,
    pub chosen_rid: Option<Rid>,
    pub layer_mode: LayerMode,
}

#[derive(Debug)]
pub struct SimulcastLayer {
    pub rid: Rid,
    pub bitrate_estimator: RefCell<BitrateEstimator>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOutState {
    ToOpen,
    Negotiating(Mid),
    Open(Mid),
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum LayerMode {
    Manual,
    Auto,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum RelayPotential {
    Never,
    Unknown,
    Qualified,
}

/// As large as str0m's own `Output`, which is what it carries. Boxing would
/// trade a stack move for a heap allocation per event and would not shrink the
/// move out of str0m in the first place.
#[allow(clippy::large_enum_variant)]
pub enum PollResult {
    Timeout(Instant),
    Event(str0m::Event),
}

pub enum TickResult {
    Timeout(Instant),
    Disconnected,
}

pub struct ViewerEffects {
    pub keyframe_requests: Vec<KeyframeRequest>,
    pub p2p_sdps: Vec<(ClientId, S2cDcPayload)>,
    pub connected_relays: Vec<ClientId>,
    pub disconnected_relays: Vec<ClientId>,
}

pub struct StreamerEffects {
    pub new_tracks: Vec<Rc<TrackIn>>,
    pub media_datas: Vec<MediaData>,
    pub should_reevaluate: bool,
}

pub enum SfuMessage {
    RegisterViewer {
        rtc: Box<Rtc>,
        room_id: RoomId,
        session_id: Uuid,
        reply: SyncSender<Option<StatusCode>>,
    },
    RegisterStreamer {
        rtc: Box<Rtc>,
        room_id: RoomId,
        session_id: Uuid,
        reply: SyncSender<Option<StatusCode>>,
    },
    CreateRoom {
        reply: SyncSender<(RoomId, StreamKey)>,
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
    GetRoom {
        room_id: RoomId,
        reply: SyncSender<Option<(usize, RoomState)>>,
    },
    ResolveStreamKey {
        hashed_stream_key: [u8; 32],
        reply: SyncSender<Option<RoomId>>,
    },
    TerminateSession {
        room_id: RoomId,
        session_id: Uuid,
        reply: SyncSender<Option<()>>,
    },
    ReissueStreamKey {
        room_id: RoomId,
        reply: SyncSender<Option<StreamKey>>,
    },
    Datagram {
        data: Vec<u8>,
        source: SocketAddr,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum C2sDcPayload {
    Offer { sdp: String },
    Answer { sdp: String },
    SetLayer { mid: String, rid: String },
    SetLayerMode { mid: String, layer_mode: LayerMode },
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
    P2pOffer {
        sdp: String,
    },
    P2pAnswer {
        sdp: String,
    },
    Demote,
    ProbeAvailableUpload,
    LayerChanged {
        rid: String,
        mid: String,
    },
    LayerStatus {
        mid: String,
        available_simulcast_layers: Vec<AvailableSimulcastLayer>,
        chosen_layer: Option<String>,
        layer_mode: LayerMode,
    },
}

#[derive(Serialize)]
pub struct AvailableSimulcastLayer {
    pub rid: String,
    pub bitrate_estimate: Option<u64>,
}

#[derive(Debug)]
pub enum RelayStatus {
    Relay {
        leaf: ClientId,
        p2p_connected_at: Option<Instant>,
    },

    Leaf {
        relay: ClientId,
        link_state: LinkState,
    },
}

#[derive(Debug)]
pub enum LinkState {
    Connecting { at: Instant },
    Connected,
}

#[derive(Debug, Clone, Copy)]
pub enum ClientType {
    Viewer,
    Streamer,
}

#[derive(Debug)]
pub struct SocketRoutes(pub HashMap<SocketAddr, (ClientType, RoomId, ClientId)>);

impl ClientId {
    pub fn next() -> ClientId {
        static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
        ClientId(ID_COUNTER.fetch_add(1, Ordering::SeqCst))
    }
}

impl fmt::Debug for StreamKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StreamKey").field(&"[REDACTED]").finish()
    }
}

impl RoomId {
    pub fn new() -> RoomId {
        RoomId(format!("RM_{}", random_string(ROOM_ID_LEN)))
    }
}

impl StreamKey {
    pub fn new() -> StreamKey {
        StreamKey(format!("SK_{}", random_string(STREAM_KEY_LEN)))
    }

    pub fn hashed(&self) -> [u8; 32] {
        hash_string(self.as_str())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl TrackIn {
    pub fn default_rid(&self) -> Option<Rid> {
        self.available_simulcast_layers
            .iter()
            .filter(|l| l.estimate_bps().is_some())
            .min_by_key(|l| l.estimate_bps())
            .or_else(|| self.available_simulcast_layers.first())
            .map(|l| l.rid)
    }
    pub fn highest_estimated_bps(&self) -> Option<u64> {
        self.available_simulcast_layers
            .iter()
            .filter_map(|layer| layer.estimate_bps())
            .max()
    }
}

impl SimulcastLayer {
    pub fn estimate_bps(&self) -> Option<u64> {
        self.bitrate_estimator.borrow_mut().bitrate_estimate()
    }
}

impl BitrateEstimator {
    pub fn new() -> BitrateEstimator {
        BitrateEstimator {
            history: VecDeque::new(),
            accumulated_bytes: 0,
            started_at: None,
            is_estimate_available: false,
        }
    }

    pub fn push(&mut self, bytes: usize, now: Instant) -> PushOutcome {
        self.remove_expired();

        if self.history.is_empty() {
            self.started_at = Some(now);
        }

        self.history.push_back((bytes, now));

        self.accumulated_bytes += bytes as u64;

        if self
            .started_at
            .is_some_and(|at| at.elapsed() >= tuning().bitrate.estimation_interval())
        {
            if self.is_estimate_available {
                PushOutcome::NoChange
            } else {
                self.is_estimate_available = true;
                PushOutcome::EstimateBecameAvailable
            }
        } else {
            self.is_estimate_available = false;
            PushOutcome::NoChange
        }
    }

    fn bitrate_estimate(&mut self) -> Option<u64> {
        self.remove_expired();

        if self.history.is_empty()
            || self
                .started_at
                .is_some_and(|at| at.elapsed() < tuning().bitrate.estimation_interval())
        {
            None
        } else {
            Some(self.accumulated_bytes * 8 / tuning().bitrate.estimate_secs)
        }
    }

    fn remove_expired(&mut self) {
        while let Some((bytes, timestamp)) = self.history.front() {
            if timestamp.elapsed() > tuning().bitrate.estimation_interval() {
                self.accumulated_bytes -= *bytes as u64;
                self.history.pop_front();
            } else {
                break;
            }
        }

        if self.history.is_empty() {
            self.started_at = None;
            self.accumulated_bytes = 0;
        }
    }
}

impl Peer {
    pub fn new(rtc: Rtc, session_id: Uuid) -> Peer {
        Peer {
            rtc,
            pending: None,
            cid: None,
            connected_at: Instant::now(),
            session_id,
        }
    }
}

impl RelayState {
    pub fn new() -> RelayState {
        RelayState {
            relay_status: None,
            perf: VecDeque::new(),
            available_upload: None,
            relay_outgoing_kbps: None,
        }
    }
}

impl SocketRoutes {
    pub fn new() -> SocketRoutes {
        SocketRoutes(HashMap::new())
    }

    pub fn clean_routes_by_room_id(&mut self, target_id: &RoomId) {
        self.0.retain(|_, (_, room_id, _)| room_id != target_id);
    }

    pub fn clean_routes_by_clients(&mut self, client_ids: &[ClientId]) {
        self.0.retain(|_, (_, _, id)| !client_ids.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    impl BitrateEstimator {
        pub(crate) fn with_estimated_bps(estimated_bps: u64) -> Self {
            let now = Instant::now();
            let accumulated_bytes = estimated_bps * tuning().bitrate.estimate_secs / 8;
            let mut history = VecDeque::new();
            history.push_back((accumulated_bytes as usize, now));

            BitrateEstimator {
                history,
                accumulated_bytes,
                started_at: Some(now - tuning().bitrate.estimation_interval()),
                is_estimate_available: true,
            }
        }
    }

    fn simulcast_layer(rid: &str, estimated_bps: Option<u64>) -> SimulcastLayer {
        let bitrate_estimator =
            estimated_bps.map_or_else(BitrateEstimator::new, BitrateEstimator::with_estimated_bps);

        SimulcastLayer {
            rid: Rid::from(rid),
            bitrate_estimator: RefCell::new(bitrate_estimator),
        }
    }

    #[test]
    fn push_reports_when_estimate_becomes_available_once() {
        let mut estimator = BitrateEstimator::new();
        let now = Instant::now();

        assert!(matches!(estimator.push(1_000, now), PushOutcome::NoChange));

        estimator.started_at = Some(now - Duration::from_secs(tuning().bitrate.estimate_secs + 1));

        assert!(matches!(
            estimator.push(1_000, now),
            PushOutcome::EstimateBecameAvailable
        ));
        assert!(matches!(estimator.push(1_000, now), PushOutcome::NoChange));
    }

    #[test]
    fn default_rid_selects_the_cheapest_measured_layer() {
        let track = TrackIn {
            origin: ClientId(0),
            mid: Mid::from("video"),
            kind: MediaKind::Video,
            available_simulcast_layers: vec![
                simulcast_layer("high", Some(2_500_000)),
                simulcast_layer("low", Some(300_000)),
                simulcast_layer("middle", Some(800_000)),
            ],
        };

        assert_eq!(track.default_rid(), Some(Rid::from("low")));
    }

    /// Every layer is unmeasured for the first window of a broadcast, and a
    /// viewer that arrives then still needs a layer: without one it matches no
    /// incoming rid and auto cannot move it, since auto reads the current layer.
    #[test]
    fn default_rid_falls_back_to_the_first_layer_before_any_estimate() {
        let track = TrackIn {
            origin: ClientId(0),
            mid: Mid::from("video"),
            kind: MediaKind::Video,
            available_simulcast_layers: vec![
                simulcast_layer("first", None),
                simulcast_layer("middle", None),
                simulcast_layer("last", None),
            ],
        };

        assert_eq!(track.default_rid(), Some(Rid::from("first")));
    }

    #[test]
    fn default_rid_ignores_layers_without_an_estimate() {
        let track = TrackIn {
            origin: ClientId(0),
            mid: Mid::from("video"),
            kind: MediaKind::Video,
            available_simulcast_layers: vec![
                simulcast_layer("unknown", None),
                simulcast_layer("measured", Some(800_000)),
            ],
        };

        assert_eq!(track.default_rid(), Some(Rid::from("measured")));
    }

    #[test]
    fn default_rid_returns_none_without_simulcast_layers() {
        let track = TrackIn {
            origin: ClientId(0),
            mid: Mid::from("video"),
            kind: MediaKind::Video,
            available_simulcast_layers: vec![],
        };

        assert!(track.default_rid().is_none());
    }

    #[test]
    fn highest_estimated_bps_ignores_layers_without_estimate() {
        let track = TrackIn {
            origin: ClientId(0),
            mid: Mid::from("video"),
            kind: MediaKind::Video,
            available_simulcast_layers: vec![
                simulcast_layer("low", Some(300_000)),
                simulcast_layer("unknown", None),
                simulcast_layer("high", Some(2_000_000)),
            ],
        };

        assert_eq!(track.highest_estimated_bps(), Some(2_000_000));
    }
}
