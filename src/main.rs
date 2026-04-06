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

    let host_addr = utils::addr::select_host_address();
    let socket = UdpSocket::bind(format!("{host_addr}:0")).expect("binding a random UDP port");
    let addr = socket.local_addr().expect("a local socket address");
    let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::sync_channel::<(Rtc, ClientRole, RoomId)>(16);

    std::thread::spawn(move || {
        servers::sfu::run(rx, socket).unwrap();
    });

    servers::web::run(addr, tx, rooms).await;
}
