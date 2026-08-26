use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::{Rc, Weak},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    time::{Duration, Instant},
};

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use str0m::{
    Rtc,
    bwe::Bitrate,
    change::SdpPendingOffer,
    channel::ChannelId,
    media::{MediaKind, Mid, Rid},
};

use derive_more::Display;

const BITRATE_ESTIMATION_SECOND: u64 = 3;

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
    pub connected_at: Instant,
    pub last_twcc_bitrate: Option<Bitrate>,
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
pub struct BitrateEstimator {
    history: VecDeque<(usize, Instant)>,
    accumulated_bytes: u64,
    started_at: Option<Instant>,
}

impl BitrateEstimator {
    pub fn new() -> BitrateEstimator {
        BitrateEstimator {
            history: VecDeque::new(),
            accumulated_bytes: 0,
            started_at: None,
        }
    }

    pub fn push(&mut self, bytes: usize, now: Instant) {
        self.remove_expired();

        if self.history.is_empty() {
            self.started_at = Some(now);
        }

        self.history.push_back((bytes, now));

        self.accumulated_bytes += bytes as u64;
    }

    fn bitrate_estimate(&mut self) -> Option<u64> {
        self.remove_expired();

        if self.history.is_empty()
            || self
                .started_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(BITRATE_ESTIMATION_SECOND))
        {
            None
        } else {
            Some(self.accumulated_bytes * 8 / BITRATE_ESTIMATION_SECOND)
        }
    }

    fn remove_expired(&mut self) {
        while let Some((bytes, timestamp)) = self.history.front() {
            if timestamp.elapsed() > Duration::from_secs(BITRATE_ESTIMATION_SECOND) {
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

impl SimulcastLayer {
    pub fn estimate_bps(&self) -> Option<u64> {
        self.bitrate_estimator.borrow_mut().bitrate_estimate()
    }
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
    pub fn new(rtc: Rtc, role: ClientRole) -> Client {
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
            last_twcc_bitrate: None,
        }
    }
}

impl TrackIn {
    pub fn default_rid(&self) -> Option<Rid> {
        self.available_simulcast_layers.last().map(|l| l.rid)
    }
    pub fn highest_estimated_bps(&self) -> Option<u64> {
        self.available_simulcast_layers
            .iter()
            .filter_map(|layer| layer.estimate_bps())
            .max()
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
    LayerChanged { rid: Rid, mid: Mid },
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

    impl BitrateEstimator {
        pub(crate) fn with_estimated_bps(estimated_bps: u64) -> Self {
            let now = Instant::now();
            let accumulated_bytes = estimated_bps * BITRATE_ESTIMATION_SECOND / 8;
            let mut history = VecDeque::new();
            history.push_back((accumulated_bytes as usize, now));

            BitrateEstimator {
                history,
                accumulated_bytes,
                started_at: Some(now - Duration::from_secs(BITRATE_ESTIMATION_SECOND)),
            }
        }
    }

    fn simulcast_layer(rid: &str, estimated_bps: Option<u64>) -> SimulcastLayer {
        let bitrate_estimator = estimated_bps.map_or_else(
            BitrateEstimator::new,
            BitrateEstimator::with_estimated_bps,
        );

        SimulcastLayer {
            rid: Rid::from(rid),
            bitrate_estimator: RefCell::new(bitrate_estimator),
        }
    }

    #[test]
    fn default_rid_selects_last_simulcast_layer() {
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

        assert_eq!(track.default_rid(), Some(Rid::from("last")));
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
