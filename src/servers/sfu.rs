use std::{net::UdpSocket, sync::mpsc::Receiver};

use str0m::Rtc;
use tracing::warn;

use crate::{
    servers,
    types::{ClientRole, RoomId, Rooms},
};

pub fn run(rx: Receiver<(Rtc, ClientRole, RoomId)>, socket: UdpSocket, rooms: Rooms) {
    if let Err(e) = servers::udp::run(rx, socket, rooms) {
        warn!("SFU error! {}", e);
    }
}
