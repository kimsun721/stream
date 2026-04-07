use std::{
    collections::HashMap,
    net::UdpSocket,
    sync::{Arc, Mutex, mpsc},
};

use str0m::{Rtc, crypto::from_feature_flags};

use crate::types::{ClientRole, RoomId, Rooms};

mod servers;
mod types;
mod utils;

#[tokio::main]
async fn main() {
    utils::log::init_log();

    from_feature_flags().install_process_default();

    let (addr, socket) = servers::udp::bind_udp_socket();

    let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::sync_channel::<(Rtc, ClientRole, RoomId)>(16);

    std::thread::spawn(move || {
        servers::sfu::run(rx, socket).unwrap();
    });

    servers::web::run(addr, tx, rooms).await;
}
