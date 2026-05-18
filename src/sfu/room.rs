use std::{
    collections::{HashMap, hash_map::Entry},
    ops::{Deref, DerefMut},
    sync::{Arc, Weak, mpsc::SyncSender},
};

use str0m::{Rtc, media::Rid};
use tracing::warn;

use crate::types::{
    Client, ClientRole, Room, RoomId, RoomState, Rooms, TrackIn, TrackOut, TrackOutState,
};

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
                    state: RoomState::IDLE,
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
                RoomState::IDLE | RoomState::PREVIEW => {
                    warn!("Client connected to an {:?} room", self.state);
                }
                RoomState::LIVE => {
                    let tracks: Vec<Weak<TrackIn>> = self
                        .clients
                        .iter()
                        .filter(|c| c.role == ClientRole::Streamer)
                        .flat_map(|c| c.tracks_in.iter().map(|t| Arc::downgrade(&t.id)))
                        .collect();

                    let available_rids: Vec<Rid> = self
                        .clients
                        .iter()
                        .filter(|c| c.role == ClientRole::Streamer)
                        .flat_map(|c| c.tracks_in.iter())
                        .flat_map(|t| t.id.available_rids.clone())
                        .collect();

                    let default_rid = Rid::from("l");
                    let chosen_rid = available_rids.contains(&default_rid).then(|| default_rid);

                    for track_in in tracks {
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
