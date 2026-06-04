use std::{
    collections::HashMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    time::Instant,
};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Display)]
pub struct ClientId(u64);

#[derive(Debug)]
pub struct RoomId(pub u64);

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
    pub available_rids: Vec<Rid>,
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
    pub chosen_rid: Option<Rid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOutState {
    ToOpen,
    Negotiating(Mid),
    Open(Mid),
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
        }
    }
}

impl TrackIn {
    pub fn default_rid(&self) -> Option<Rid> {
        let default = Rid::from("l");

        self.available_rids
            .contains(&default)
            .then_some(default)
            .or_else(|| self.available_rids.first().copied())
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
    Promote {
        room_id: RoomId,
        reply: SyncSender<Option<()>>,
    },
    Demote {
        room_id: RoomId,
        reply: SyncSender<Option<()>>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum C2sDcPayload {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    SetLayer {
        mid: Mid,
        rid: Rid,
    },
    PerfReport {
        rtt_ms: u32,
        loss_pct: f32,
        avail_out_kbs: u32,
    },
    P2pOffer {
        sdp: String,
    },
    P2pAnswer {
        sdp: String,
    },
    P2pConnected,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum S2cDcPayload {
    RequestOffer,
    P2pOffer { sdp: String },
    P2pAnswer { sdp: String },
    Demote,
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
