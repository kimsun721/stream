use serde::Deserialize;

#[derive(Deserialize)]
pub enum ClientRole {
    Streamer,
    ClientRole,
}

pub struct RoomId(pub u64);
