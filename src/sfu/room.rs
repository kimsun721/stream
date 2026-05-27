use std::{
    collections::{HashMap, hash_map::Entry},
    ops::{Deref, DerefMut},
    sync::{Arc, mpsc::SyncSender},
};

use str0m::Rtc;
use tracing::warn;

use crate::types::{Client, ClientRole, Room, RoomId, RoomState, Rooms, TrackOut, TrackOutState};

impl Rooms {
    pub fn new() -> Self {
        Rooms(HashMap::new())
    }

    pub fn register_client(&mut self, rtc: Rtc, role: ClientRole, room_id: RoomId) {
        if let Some(room) = self.get_mut(&room_id.0) {
            room.add_client(rtc, role);
        } else {
            warn!("Room does not exist : {:?}", room_id);
        };
    }

    pub fn create(&mut self, room_id: RoomId, reply: SyncSender<Option<()>>) {
        match self.entry(room_id.0) {
            Entry::Occupied(_) => reply.send(None).ok(),
            Entry::Vacant(e) => {
                e.insert(Room {
                    streamer_id: None,
                    clients: vec![],
                    state: RoomState::Idle,
                });

                reply.send(Some(())).ok()
            }
        };
    }

    pub fn get_views(&self, room_id: RoomId, reply: SyncSender<Option<usize>>) {
        match self.get(&room_id.0) {
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
        match self.get_mut(&room_id.0) {
            Some(room) => {
                room.set_state(state);
                reply.send(Some(())).ok()
            }
            None => reply.send(None).ok(),
        };
    }

    pub fn delete(&mut self, room_id: RoomId, reply: SyncSender<Option<()>>) {
        match self.remove(&room_id.0) {
            Some(_) => reply.send(Some(())).ok(),
            None => reply.send(None).ok(),
        };
    }
}

impl Room {
    fn view_count(&self) -> usize {
        self.clients
            .iter()
            .filter(|c| c.role == ClientRole::Viewer)
            .count()
    }

    fn set_state(&mut self, state: RoomState) {
        self.state = state;
    }

    fn add_client(&mut self, rtc: Rtc, role: ClientRole) {
        let mut client = Client::new(rtc, role);

        match role {
            ClientRole::Streamer => {
                if self
                    .clients
                    .iter()
                    .any(|c| matches!(c.role, ClientRole::Streamer))
                {
                    warn!("Streamer already connected in room {:?}", &self);
                } else {
                    self.streamer_id = Some(client.id);
                    self.clients.push(client);
                }
            }
            ClientRole::Viewer => match self.state {
                RoomState::Idle | RoomState::Preview => {
                    warn!("Client connected to an {:?} room", self.state);
                }
                RoomState::Live => {
                    let tracks: Vec<_> = self
                        .clients
                        .iter()
                        .filter(|c| c.role == ClientRole::Streamer)
                        .flat_map(|c| {
                            c.tracks_in
                                .iter()
                                .map(|t| (Arc::downgrade(&t.id), t.id.default_rid()))
                        })
                        .collect();

                    for (track_in, chosen_rid) in tracks {
                        client.tracks_out.push(TrackOut {
                            track_in,
                            state: TrackOutState::ToOpen,
                            chosen_rid,
                        });
                    }

                    self.clients.push(client);
                }
            },
        };
    }
}

impl Deref for Rooms {
    type Target = HashMap<u64, Room>;
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

    use str0m::Rtc;

    use crate::types::{Client, ClientRole, Room, RoomId, RoomState, Rooms};

    fn reply<T>() -> (mpsc::SyncSender<Option<T>>, mpsc::Receiver<Option<T>>) {
        mpsc::sync_channel(1)
    }

    fn rtc() -> Rtc {
        Rtc::new(Instant::now())
    }

    #[test]
    fn rooms_create_inserts_idle_room_and_rejects_duplicate() {
        let mut rooms = Rooms::new();

        let (tx, rx) = reply();
        rooms.create(RoomId(7), tx);
        assert_eq!(rx.recv().unwrap(), Some(()));
        assert!(matches!(rooms.get(&7).unwrap().state, RoomState::Idle));

        let (tx, rx) = reply();
        rooms.create(RoomId(7), tx);
        assert_eq!(rx.recv().unwrap(), None);
    }

    #[test]
    fn rooms_update_state_updates_existing_room_only() {
        let mut rooms = Rooms::new();

        let (tx, rx) = reply();
        rooms.create(RoomId(7), tx);
        assert_eq!(rx.recv().unwrap(), Some(()));

        let (tx, rx) = reply();
        rooms.update_state(RoomId(7), RoomState::Live, tx);
        assert_eq!(rx.recv().unwrap(), Some(()));
        assert!(matches!(rooms.get(&7).unwrap().state, RoomState::Live));

        let (tx, rx) = reply();
        rooms.update_state(RoomId(8), RoomState::Live, tx);
        assert_eq!(rx.recv().unwrap(), None);
    }

    #[test]
    fn rooms_get_views_counts_viewers_and_reports_missing_room() {
        let mut rooms = Rooms::new();

        let (tx, rx) = reply();
        rooms.create(RoomId(7), tx);
        assert_eq!(rx.recv().unwrap(), Some(()));

        let (tx, rx) = reply();
        rooms.update_state(RoomId(7), RoomState::Live, tx);
        assert_eq!(rx.recv().unwrap(), Some(()));

        rooms.register_client(rtc(), ClientRole::Streamer, RoomId(7));
        rooms.register_client(rtc(), ClientRole::Viewer, RoomId(7));

        let (tx, rx) = reply();
        rooms.get_views(RoomId(7), tx);
        assert_eq!(rx.recv().unwrap(), Some(1));

        let (tx, rx) = reply();
        rooms.get_views(RoomId(8), tx);
        assert_eq!(rx.recv().unwrap(), None);
    }

    #[test]
    fn rooms_delete_removes_existing_room_only() {
        let mut rooms = Rooms::new();

        let (tx, rx) = reply();
        rooms.create(RoomId(7), tx);
        assert_eq!(rx.recv().unwrap(), Some(()));

        let (tx, rx) = reply();
        rooms.delete(RoomId(7), tx);
        assert_eq!(rx.recv().unwrap(), Some(()));
        assert!(!rooms.contains_key(&7));

        let (tx, rx) = reply();
        rooms.delete(RoomId(7), tx);
        assert_eq!(rx.recv().unwrap(), None);
    }

    #[test]
    fn add_duplicate_streamer() {
        let clients: Vec<Client> = Vec::new();

        let mut room = Room {
            streamer_id: None,
            clients,
            state: RoomState::Live,
        };

        room.add_client(Rtc::new(Instant::now()), ClientRole::Streamer);
        room.add_client(Rtc::new(Instant::now()), ClientRole::Streamer);
        room.add_client(Rtc::new(Instant::now()), ClientRole::Streamer);

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
            };

            room.add_client(Rtc::new(Instant::now()), ClientRole::Viewer);

            assert_eq!(
                room.clients.len(),
                clients_len,
                "add_client unit test for RoomState: {state:?} case"
            );
        }
    }
}
