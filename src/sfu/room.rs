use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use crate::types::{Room, Rooms};

impl Rooms {
    pub fn new() -> Self {
        Rooms(HashMap::new())
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
