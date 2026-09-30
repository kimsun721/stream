use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    rc::Rc,
    sync::mpsc::{Sender, SyncSender},
    time::Instant,
};

use axum::http::StatusCode;
use str0m::Rtc;
use subtle::ConstantTimeEq;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{metrics, types::Viewer};
use crate::{
    shards::types::ShardRouteMsg,
    types::{
        ClientId, LayerMode, LinkState, RelayStatus, Room, RoomId, RoomState, Rooms, SocketRoutes,
        StreamKey, Streamer, TrackOut, TrackOutState, Viewers,
    },
};

impl Rooms {
    pub fn new(tx_to_mux: Sender<ShardRouteMsg>, shard: usize) -> Self {
        Rooms {
            rooms: HashMap::new(),
            socket_routes: SocketRoutes::new(tx_to_mux, shard),
        }
    }

    pub fn register_viewer(
        &mut self,
        rtc: Rtc,
        room_id: RoomId,
        session_id: Uuid,
        reply: SyncSender<Option<StatusCode>>,
    ) {
        if let Some(room) = self.get_mut(&room_id) {
            room.add_viewer(rtc, session_id, reply);
        } else {
            warn!("Room does not exist : {:?}", room_id);
            reply.send(Some(StatusCode::NOT_FOUND)).ok();
        };
    }

    pub fn register_streamer(
        &mut self,
        rtc: Rtc,
        room_id: RoomId,
        session_id: Uuid,
        reply: SyncSender<Option<StatusCode>>,
    ) {
        if let Some(room) = self.get_mut(&room_id) {
            room.add_streamer(rtc, session_id, reply);
        } else {
            warn!("Room does not exist : {:?}", room_id);
            reply.send(Some(StatusCode::NOT_FOUND)).ok();
        }
    }

    pub fn create(&mut self, room_id: RoomId, reply: SyncSender<(RoomId, StreamKey)>) {
        let stream_key = StreamKey::new();
        let hashed_stream_key = stream_key.hashed();

        self.insert(
            room_id.clone(),
            Room {
                streamer: None,
                viewers: Viewers::new(),
                state: RoomState::Idle,
                hashed_stream_key,
            },
        );

        reply.send((room_id, stream_key)).ok();
    }

    pub fn get_room(&self, room_id: RoomId, reply: SyncSender<Option<(usize, RoomState)>>) {
        match self.get(&room_id) {
            Some(room) => reply.send(Some((room.view_count(), room.state))).ok(),
            None => reply.send(None).ok(),
        };
    }

    pub fn update_state(
        &mut self,
        room_id: RoomId,
        state: RoomState,
        reply: SyncSender<Option<()>>,
    ) {
        match self.rooms.get_mut(&room_id) {
            Some(room) => {
                if matches!(state, RoomState::Idle) {
                    room.init();
                    self.socket_routes.clean_routes_by_room_id(&room_id);
                }
                room.set_state(state);
                reply.send(Some(())).ok()
            }
            None => reply.send(None).ok(),
        };
    }

    pub fn delete(&mut self, room_id: RoomId, reply: SyncSender<Option<()>>) {
        match self.rooms.remove(&room_id) {
            Some(_) => {
                self.socket_routes.clean_routes_by_room_id(&room_id);
                reply.send(Some(())).ok()
            }
            None => reply.send(None).ok(),
        };
    }

    pub fn resolve_stream_key(
        &self,
        hashed_stream_key: [u8; 32],
        reply: SyncSender<Option<RoomId>>,
    ) {
        let room_id = self.iter().find_map(|(room_id, room)| {
            bool::from(hashed_stream_key.ct_eq(&room.hashed_stream_key)).then_some(room_id.clone())
        });

        reply.send(room_id).ok();
    }

    pub fn terminate_session(
        &mut self,
        room_id: RoomId,
        session_id: Uuid,
        reply: SyncSender<Option<()>>,
    ) {
        if let Some(room) = self.rooms.get_mut(&room_id)
            && let Some(streamer) = &room.streamer
            && streamer.peer.session_id == session_id
        {
            room.init();
            self.socket_routes.clean_routes_by_room_id(&room_id);
            reply.send(Some(())).ok();
        } else {
            reply.send(None).ok();
        };
    }

    pub fn reissue_stream_key(&mut self, room_id: RoomId, reply: SyncSender<Option<StreamKey>>) {
        if let Some(room) = self.get_mut(&room_id) {
            let stream_key = StreamKey::new();
            let hashed_stream_key = stream_key.hashed();

            room.hashed_stream_key = hashed_stream_key;

            reply.send(Some(stream_key)).ok();
        } else {
            reply.send(None).ok();
        }
    }
}

impl Room {
    pub fn promote(&mut self, relay_id: ClientId) {
        let Some(leaf_id) = self
            .viewers
            .values()
            .filter(|c| c.relay_state.relay_status.is_none() && c.id != relay_id)
            .min_by_key(|c| c.relay_state.relay_potential())
            .map(|c| c.id)
        else {
            return;
        };

        let Some(relay) = self
            .viewers
            .get_mut(&relay_id)
            .filter(|c| c.relay_state.relay_status.is_none())
        else {
            return;
        };

        if let Err(e) = relay.request_sdp_offer() {
            error!("request_sdp_offer failed: {e}");
            return;
        };

        relay.relay_state.relay_status = Some(RelayStatus::Relay {
            leaf: leaf_id,
            p2p_connected_at: None,
        });
        relay.relay_state.relay_outgoing_kbps = None;

        let Some(leaf) = self.viewers.get_mut(&leaf_id) else {
            return;
        };

        leaf.relay_state.relay_status = Some(RelayStatus::Leaf {
            relay: relay_id,
            link_state: LinkState::Connecting { at: Instant::now() },
        });

        metrics::promoted();

        info!("promote relay={relay_id} leaf={}", leaf_id);
    }

    pub fn demote(&mut self, relay_id: ClientId, leaf_id: ClientId) {
        let Some(relay) = self.viewers.get_mut(&relay_id) else {
            return;
        };

        metrics::demoted();

        if let Err(e) = relay.demote_relay() {
            error!("demote relay failed error={e}");
        };

        let Some(leaf) = self.viewers.get_mut(&leaf_id) else {
            return;
        };

        if let Ok(keyframe_requests) = leaf.demote_leaf()
            && let Some(streamer) = &mut self.streamer
            && let Err(e) = streamer.handle_keyframe_requests(keyframe_requests)
        {
            error!("demote keyframe requests failed: {e}");
        };
    }

    fn view_count(&self) -> usize {
        self.viewers.len()
    }

    fn set_state(&mut self, state: RoomState) {
        self.state = state;
    }

    fn add_viewer(&mut self, rtc: Rtc, session_id: Uuid, reply: SyncSender<Option<StatusCode>>) {
        let mut viewer = Viewer::new(rtc, session_id);

        match self.state {
            RoomState::Idle | RoomState::Preview => {
                warn!("Client connected to an {:?} room", self.state);
                reply.send(Some(StatusCode::NOT_FOUND)).ok();
            }
            RoomState::Live => {
                let tracks: Vec<_> = self
                    .streamer
                    .as_ref()
                    .into_iter()
                    .flat_map(|s| {
                        s.tracks_in
                            .iter()
                            .map(|t| (Rc::downgrade(&t.id), t.id.default_rid()))
                    })
                    .collect();

                for (track_in, default_rid) in tracks {
                    viewer.tracks_out.push(TrackOut {
                        track_in,
                        state: TrackOutState::ToOpen,
                        layer_mode: LayerMode::Auto,
                        chosen_rid: default_rid,
                    });
                }

                if let Some(desired_bitrate) = viewer.calc_desired_bitrate() {
                    viewer.peer.rtc.bwe().set_desired_bitrate(desired_bitrate);
                }

                self.viewers.insert(viewer.id, viewer);
                reply.send(None).ok();
            }
        }
    }

    fn add_streamer(&mut self, rtc: Rtc, session_id: Uuid, reply: SyncSender<Option<StatusCode>>) {
        if self.streamer.is_none() {
            self.state = RoomState::Preview;

            self.streamer = Some(Streamer::new(rtc, session_id));
            reply.send(None).ok();
        } else {
            warn!("Streamer already connected in room {:?}", &self);
            reply.send(Some(StatusCode::CONFLICT)).ok();
        }
    }

    pub fn init(&mut self) {
        self.viewers.clear();
        self.state = RoomState::Idle;
        self.streamer = None;
    }
}

impl Deref for Rooms {
    type Target = HashMap<RoomId, Room>;
    fn deref(&self) -> &Self::Target {
        &self.rooms
    }
}

impl DerefMut for Rooms {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rooms
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::mpsc, time::Instant};

    use axum::http::StatusCode;
    use str0m::Rtc;
    use uuid::Uuid;

    use crate::{
        shards::types::ShardRouteMsg,
        types::{ClientId, ClientType, Room, RoomId, RoomState, Rooms, StreamKey, Viewers},
    };

    /// The receiver is handed back so the test keeps it alive; dropping it
    /// would turn every route notice into a failed send.
    fn rooms() -> (Rooms, mpsc::Receiver<ShardRouteMsg>) {
        let (tx, rx) = mpsc::channel();
        (Rooms::new(tx, 0), rx)
    }

    fn reply<T>() -> (mpsc::SyncSender<Option<T>>, mpsc::Receiver<Option<T>>) {
        mpsc::sync_channel(1)
    }

    fn rtc() -> Rtc {
        Rtc::new(Instant::now())
    }

    fn create_room(rooms: &mut Rooms) -> (RoomId, StreamKey) {
        let (tx, rx) = mpsc::sync_channel(1);
        rooms.create(RoomId::new(0), tx);
        rx.recv().unwrap()
    }

    fn missing_id() -> RoomId {
        RoomId("RM_missing".to_string())
    }

    #[test]
    fn rooms_create_inserts_idle_room_with_unique_id() {
        let (mut rooms, _routes) = rooms();

        let (first_id, _) = create_room(&mut rooms);
        let (second_id, _) = create_room(&mut rooms);

        assert_ne!(first_id, second_id);
        assert!(matches!(
            rooms.get(&first_id).unwrap().state,
            RoomState::Idle
        ));
        assert!(matches!(
            rooms.get(&second_id).unwrap().state,
            RoomState::Idle
        ));
    }

    #[test]
    fn rooms_create_returns_key_matching_stored_hash() {
        let (mut rooms, _routes) = rooms();

        let (room_id, stream_key) = create_room(&mut rooms);

        assert_eq!(
            rooms.get(&room_id).unwrap().hashed_stream_key,
            stream_key.hashed()
        );
    }

    #[test]
    fn rooms_reissue_stream_key_retires_the_old_key() {
        let (mut rooms, _routes) = rooms();

        let (room_id, old_key) = create_room(&mut rooms);

        let (tx, rx) = reply();
        rooms.reissue_stream_key(room_id.clone(), tx);
        let new_key = rx.recv().unwrap().expect("room exists");

        let (tx, rx) = reply();
        rooms.resolve_stream_key(old_key.hashed(), tx);
        assert_eq!(rx.recv().unwrap(), None, "the old key must stop resolving");

        let (tx, rx) = reply();
        rooms.resolve_stream_key(new_key.hashed(), tx);
        assert_eq!(rx.recv().unwrap(), Some(room_id));
    }

    #[test]
    fn rooms_reissue_stream_key_reports_missing_room() {
        let (mut rooms, _routes) = rooms();

        let (tx, rx) = reply();
        rooms.reissue_stream_key(missing_id(), tx);
        assert!(rx.recv().unwrap().is_none());
    }

    #[test]
    fn rooms_update_state_updates_existing_room_only() {
        let (mut rooms, _routes) = rooms();

        let (room_id, _) = create_room(&mut rooms);

        let (tx, rx) = reply();
        rooms.update_state(room_id.clone(), RoomState::Live, tx);
        assert_eq!(rx.recv().unwrap(), Some(()));
        assert!(matches!(
            rooms.get(&room_id).unwrap().state,
            RoomState::Live
        ));

        let (tx, rx) = reply();
        rooms.update_state(missing_id(), RoomState::Live, tx);
        assert_eq!(rx.recv().unwrap(), None);
    }

    #[test]
    fn rooms_get_views_counts_viewers_and_reports_missing_room() {
        let (mut rooms, _routes) = rooms();

        let (room_id, _) = create_room(&mut rooms);

        let (tx, _rx) = reply::<StatusCode>();
        rooms.register_streamer(rtc(), room_id.clone(), Uuid::new_v4(), tx);
        assert!(matches!(
            rooms.get(&room_id).unwrap().state,
            RoomState::Preview
        ));

        let (tx, rx) = reply();
        rooms.update_state(room_id.clone(), RoomState::Live, tx);
        assert_eq!(rx.recv().unwrap(), Some(()));

        let (tx, _rx) = reply::<StatusCode>();
        rooms.register_viewer(rtc(), room_id.clone(), Uuid::new_v4(), tx);

        let (tx, rx) = reply();
        rooms.get_room(room_id, tx);
        let (views, state) = rx.recv().unwrap().unwrap();
        assert_eq!(views, 1);
        assert!(matches!(state, RoomState::Live));

        let (tx, rx) = reply::<(usize, RoomState)>();
        rooms.get_room(missing_id(), tx);
        assert!(rx.recv().unwrap().is_none());
    }

    #[test]
    fn rooms_delete_removes_existing_room_only() {
        let (mut rooms, _routes) = rooms();

        let (room_id, _) = create_room(&mut rooms);

        let (tx, rx) = reply();
        rooms.delete(room_id.clone(), tx);
        assert_eq!(rx.recv().unwrap(), Some(()));
        assert!(!rooms.contains_key(&room_id));

        let (tx, rx) = reply();
        rooms.delete(room_id, tx);
        assert_eq!(rx.recv().unwrap(), None);
    }

    #[test]
    fn add_duplicate_streamer() {
        let mut room = Room {
            streamer: None,
            viewers: Viewers::new(),
            state: RoomState::Live,
            hashed_stream_key: [0; 32],
        };

        for _ in 0..3 {
            let (tx, _rx) = reply::<StatusCode>();
            room.add_streamer(Rtc::new(Instant::now()), Uuid::new_v4(), tx);
        }

        assert!(room.streamer.is_some());
    }

    #[test]
    fn add_viewer_each_room_state() {
        let cases = [
            (RoomState::Live, 1),
            (RoomState::Idle, 0),
            (RoomState::Preview, 0),
        ];

        for (state, viewers_len) in cases {
            let mut room = Room {
                streamer: None,
                viewers: Viewers::new(),
                state,
                hashed_stream_key: [0; 32],
            };

            let (tx, _rx) = reply::<StatusCode>();
            room.add_viewer(Rtc::new(Instant::now()), Uuid::new_v4(), tx);

            assert_eq!(
                room.viewers.len(),
                viewers_len,
                "add_viewer unit test for RoomState: {state:?} case"
            );
        }
    }

    #[test]
    fn rooms_delete_disowns_only_the_deleted_room_routes() {
        let (mut rooms, routes) = rooms();
        let (deleted, _) = create_room(&mut rooms);
        let (kept, _) = create_room(&mut rooms);

        let gone: SocketAddr = "127.0.0.1:50001".parse().unwrap();
        let stays: SocketAddr = "127.0.0.1:50002".parse().unwrap();

        rooms.socket_routes.routes.insert(
            gone,
            (ClientType::Viewer, deleted.clone(), ClientId::next()),
        );
        rooms
            .socket_routes
            .routes
            .insert(stays, (ClientType::Viewer, kept, ClientId::next()));

        let (tx, _rx) = reply();
        rooms.delete(deleted, tx);

        let disowned: Vec<SocketAddr> = routes
            .try_iter()
            .map(|notice| match notice {
                ShardRouteMsg::Disowned { source, .. } => source,
                ShardRouteMsg::Owned { source, .. } => panic!("{source} claimed on delete"),
            })
            .collect();

        assert_eq!(disowned, vec![gone]);
    }
}
