use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Instant,
};

use serde::Deserialize;
use str0m::{
    Rtc,
    change::SdpPendingOffer,
    channel::ChannelId,
    media::{MediaKind, Mid, Rid},
};

#[derive(Deserialize, Debug)]
pub enum ClientRole {
    Streamer,
    ClientRole,
}

#[derive(Debug)]
pub struct Client {
    id: ClientId,
    rtc: Rtc,
    pub role: ClientRole,
    pending: Option<SdpPendingOffer>,
    cid: Option<ChannelId>,
    tracks_in: Vec<TrackInEntry>,
    tracks_out: Vec<TrackOut>,
    chosen_rid: Option<Rid>,
}

#[derive(Debug, Clone)]
pub struct ClientId(u64);
pub struct RoomId(pub u64);

#[derive(Deserialize, Debug)]
pub enum RoomState {
    IDLE,
    PREVIEW,
    LIVE,
}

#[derive(Debug)]
pub struct Room {
    pub streamer_id: Option<ClientId>,
    pub clients: Vec<Client>,
    pub state: RoomState,
}

pub type Rooms = Arc<Mutex<HashMap<u64, Room>>>;

#[derive(Debug)]
struct TrackIn {
    origin: ClientId,
    mid: Mid,
    kind: MediaKind,
}

#[derive(Debug)]
struct TrackInEntry {
    id: Arc<TrackIn>,
    last_keyframe_request: Option<Instant>,
}

#[derive(Debug)]
struct TrackOut {
    track_in: Weak<TrackIn>,
    state: TrackOutState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackOutState {
    ToOpen,
    Negotiating(Mid),
    Open(Mid),
}
