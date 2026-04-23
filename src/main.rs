use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
};

use str0m::{Rtc, crypto::from_feature_flags};

use crate::types::{ClientRole, RoomId, Rooms};
use ::tracing::error;

mod servers;
mod sfu;
mod types;
mod utils;

#[tokio::main]
async fn main() {
    utils::log::init_log();

    from_feature_flags().install_process_default();

    let (addr, socket) = sfu::udp::bind_udp_socket();

    let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));
    let rooms_for_sfu = rooms.clone();

    let (tx, rx) = mpsc::sync_channel::<(Rtc, ClientRole, RoomId)>(16);

    std::thread::spawn(move || {
        if let Err(e) = sfu::udp::run(rx, socket, rooms_for_sfu) {
            error!("udp error : {}", e);
        };
    });

    if let Err(e) = servers::web::run(addr, tx, rooms).await {
        error!("web server erorr : {}", e);
    };
}
