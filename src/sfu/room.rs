use std::{
    collections::{HashMap, hash_map::Entry},
    ops::{Deref, DerefMut},
    sync::mpsc::SyncSender,
};

use crate::types::{ClientRole, Room, RoomId, RoomState, Rooms};

impl Rooms {
    pub fn new() -> Self {
        Rooms(HashMap::new())
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
