use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    rc::Rc,
    sync::mpsc::SyncSender,
};

use axum::http::StatusCode;
use str0m::Rtc;
use subtle::ConstantTimeEq;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::types::{
    Client, ClientId, ClientRole, LayerMode, LinkState, RelayStatus, Room, RoomId, RoomState,
    Rooms, StreamKey, TrackOut, TrackOutState,
};

impl Rooms {
    pub fn new() -> Self {
        Rooms(HashMap::new())
    }

    pub fn register_client(
        &mut self,
        rtc: Rtc,
        role: ClientRole,
        room_id: RoomId,
        session_id: Uuid,
        reply: SyncSender<Option<StatusCode>>,
    ) {
        if let Some(room) = self.get_mut(&room_id) {
            room.add_client(rtc, role, session_id, reply);
        } else {
            warn!("Room does not exist : {:?}", room_id);
            reply.send(Some(StatusCode::NOT_FOUND)).ok();
        };
    }

    pub fn create(&mut self, reply: SyncSender<(RoomId, StreamKey)>) {
        let room_id = RoomId::new();
        let stream_key = StreamKey::new();
        let hashed_stream_key = StreamKey::hashed(&stream_key);

        self.insert(
            room_id.clone(),
            Room {
                streamer_id: None,
                clients: vec![],
                state: RoomState::Idle,
                hashed_stream_key,
            },
        );

        reply.send((room_id, stream_key)).ok();
    }

    pub fn get_views(&self, room_id: RoomId, reply: SyncSender<Option<usize>>) {
        match self.get(&room_id) {
            Some(room) => reply.send(Some(room.view_count())).ok(),
            None => reply.send(None).ok(),
        };
    }

    pub fn update_state(
        &mut self,
        room_id: RoomId,
        state: RoomState,
        reply: SyncSender<Option<()>>,
    ) {
        match self.get_mut(&room_id) {
            Some(room) => {
                room.set_state(state);
                reply.send(Some(())).ok()
            }
            None => reply.send(None).ok(),
        };
    }

    pub fn delete(&mut self, room_id: RoomId, reply: SyncSender<Option<()>>) {
        match self.remove(&room_id) {
            Some(_) => reply.send(Some(())).ok(),
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
}

impl Room {
    pub fn promote(&mut self, relay_id: ClientId) {
        let Some(leaf_id) = self
            .clients
            .iter()
            .find(|c| {
                matches!(c.role, ClientRole::Viewer) && c.relay_status.is_none() && c.id != relay_id
            })
            .map(|c| c.id)
        else {
            return;
        };

        let Some(relay) = self
            .clients
            .iter_mut()
            .find(|c| c.id == relay_id && c.relay_status.is_none())
        else {
            return;
        };

        if let Err(e) = relay.request_sdp_offer() {
            error!("request_sdp_offer failed: {e}");
            return;
        };

        relay.relay_status = Some(RelayStatus::Relay { leaf: leaf_id });
        relay.relay_outgoing_kbps = None;

        let Some(leaf) = self.clients.iter_mut().find(|c| c.id == leaf_id) else {
            return;
        };

        leaf.relay_status = Some(RelayStatus::Leaf {
            relay: relay_id,
            link_state: LinkState::Connecting,
        });

        info!("promote relay={relay_id} leaf={}", leaf_id);
    }

    pub fn demote(&mut self, relay_id: ClientId, leaf_id: ClientId) {
        let Some(relay) = self.clients.iter_mut().find(|c| c.id == relay_id) else {
            return;
        };

        if let Err(e) = relay.demote_relay() {
            error!("demote relay failed error={e}");
        };

        let Some(leaf) = self.clients.iter_mut().find(|c| c.id == leaf_id) else {
            return;
        };

        if let Ok(keyframe_requests) = leaf.demote_leaf()
            && let Some(streamer) = self
                .clients
                .iter_mut()
                .find(|c| c.role == ClientRole::Streamer)
            && let Err(e) = streamer.handle_keyframe_requests(keyframe_requests)
        {
            error!("demote keyframe requests failed: {e}");
        };
    }

    fn view_count(&self) -> usize {
        self.clients
            .iter()
            .filter(|c| c.role == ClientRole::Viewer)
            .count()
    }

    fn set_state(&mut self, state: RoomState) {
        self.state = state;
    }

    fn add_client(
        &mut self,
        rtc: Rtc,
        role: ClientRole,
        session_id: Uuid,
        reply: SyncSender<Option<StatusCode>>,
    ) {
        let mut client = Client::new(rtc, role, session_id);

        match role {
            ClientRole::Streamer => {
                if self
                    .clients
                    .iter()
                    .any(|c| matches!(c.role, ClientRole::Streamer))
                {
                    warn!("Streamer already connected in room {:?}", &self);
                    reply.send(Some(StatusCode::CONFLICT)).ok();
                } else {
                    self.streamer_id = Some(client.id);
                    self.clients.push(client);

                    reply.send(None).ok();
                }
            }
            ClientRole::Viewer => match self.state {
                RoomState::Idle | RoomState::Preview => {
                    warn!("Client connected to an {:?} room", self.state);
                    reply.send(Some(StatusCode::NOT_FOUND)).ok();
                }
                RoomState::Live => {
                    let tracks: Vec<_> = self
                        .clients
                        .iter()
                        .filter(|c| c.role == ClientRole::Streamer)
                        .flat_map(|c| {
                            c.tracks_in
                                .iter()
                                .map(|t| (Rc::downgrade(&t.id), t.id.default_rid()))
                        })
                        .collect();

                    for (track_in, default_rid) in tracks {
                        client.tracks_out.push(TrackOut {
                            track_in,
                            state: TrackOutState::ToOpen,
                            layer_mode: LayerMode::Auto,
                            chosen_rid: default_rid,
                        });
                    }

                    if let Some(desired_bitrate) = client.calc_desired_bitrate() {
                        client.rtc.bwe().set_desired_bitrate(desired_bitrate);
                    }

                    self.clients.push(client);
                    reply.send(None).ok();
                }
            },
        };
    }

    pub fn viewers_mut(&mut self) -> impl Iterator<Item = &mut Client> {
        self.clients
            .iter_mut()
            .filter(|c| c.role == ClientRole::Viewer)
    }
}

impl Deref for Rooms {
    type Target = HashMap<RoomId, Room>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Rooms {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Instant};

    use axum::http::StatusCode;
    use str0m::Rtc;
    use uuid::Uuid;

    use crate::types::{Client, ClientRole, Room, RoomId, RoomState, Rooms, StreamKey};

    fn reply<T>() -> (mpsc::SyncSender<Option<T>>, mpsc::Receiver<Option<T>>) {
        mpsc::sync_channel(1)
    }

    fn rtc() -> Rtc {
        Rtc::new(Instant::now())
    }

    fn create_room(rooms: &mut Rooms) -> (RoomId, StreamKey) {
        let (tx, rx) = mpsc::sync_channel(1);
        rooms.create(tx);
        rx.recv().unwrap()
    }

    fn missing_id() -> RoomId {
        RoomId("RM_missing".to_string())
    }

    #[test]
    fn rooms_create_inserts_idle_room_with_unique_id() {
        let mut rooms = Rooms::new();

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
        let mut rooms = Rooms::new();

        let (room_id, stream_key) = create_room(&mut rooms);

        assert_eq!(
            rooms.get(&room_id).unwrap().hashed_stream_key,
            stream_key.hashed()
        );
    }

    #[test]
    fn rooms_update_state_updates_existing_room_only() {
        let mut rooms = Rooms::new();

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
        let mut rooms = Rooms::new();

        let (room_id, _) = create_room(&mut rooms);

        let (tx, rx) = reply();
        rooms.update_state(room_id.clone(), RoomState::Live, tx);
        assert_eq!(rx.recv().unwrap(), Some(()));

        let (tx, _rx) = reply::<StatusCode>();
        rooms.register_client(
            rtc(),
            ClientRole::Streamer,
            room_id.clone(),
            Uuid::new_v4(),
            tx,
        );
        let (tx, _rx) = reply::<StatusCode>();
        rooms.register_client(
            rtc(),
            ClientRole::Viewer,
            room_id.clone(),
            Uuid::new_v4(),
            tx,
        );

        let (tx, rx) = reply();
        rooms.get_views(room_id, tx);
        assert_eq!(rx.recv().unwrap(), Some(1));

        let (tx, rx) = reply();
        rooms.get_views(missing_id(), tx);
        assert_eq!(rx.recv().unwrap(), None);
    }

    #[test]
    fn rooms_delete_removes_existing_room_only() {
        let mut rooms = Rooms::new();

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
        let clients: Vec<Client> = Vec::new();

        let mut room = Room {
            streamer_id: None,
            clients,
            state: RoomState::Live,
            hashed_stream_key: [0; 32],
        };

        let (tx, _rx) = reply::<StatusCode>();
        room.add_client(
            Rtc::new(Instant::now()),
            ClientRole::Streamer,
            Uuid::new_v4(),
            tx,
        );
        let (tx, _rx) = reply::<StatusCode>();
        room.add_client(
            Rtc::new(Instant::now()),
            ClientRole::Streamer,
            Uuid::new_v4(),
            tx,
        );
        let (tx, _rx) = reply::<StatusCode>();
        room.add_client(
            Rtc::new(Instant::now()),
            ClientRole::Streamer,
            Uuid::new_v4(),
            tx,
        );

        assert_eq!(room.clients.len(), 1);
    }

    #[test]
    fn add_viewer_each_room_state() {
        let cases = [
            (RoomState::Live, 1),
            (RoomState::Idle, 0),
            (RoomState::Preview, 0),
        ];

        for (state, clients_len) in cases {
            let mut room = Room {
                streamer_id: None,
                clients: Vec::new(),
                state: state.clone(),
                hashed_stream_key: [0; 32],
            };

            let (tx, _rx) = reply::<StatusCode>();
            room.add_client(
                Rtc::new(Instant::now()),
                ClientRole::Viewer,
                Uuid::new_v4(),
                tx,
            );

            assert_eq!(
                room.clients.len(),
                clients_len,
                "add_client unit test for RoomState: {state:?} case"
            );
        }
    }
}
