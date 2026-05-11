use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
};

use str0m::{Rtc, crypto::from_feature_flags};

use crate::types::{ClientRole, RoomId, Rooms, SfuMessage};
use ::tracing::error;

mod servers;
mod sfu;
mod types;
mod utils;

#[tokio::main]
async fn main() {
    utils::log::init_log();

    from_feature_flags().install_process_default();

    let (addr, socket) = sfu::socket::bind_udp_socket();

    let (tx, rx) = mpsc::sync_channel::<SfuMessage>(32);

    std::thread::spawn(move || {
        if let Err(e) = sfu::run::run(rx, socket) {
            error!("udp error : {}", e);
        };
    });

    if let Err(e) = servers::web::run(addr, tx).await {
        error!("web server erorr : {}", e);
    };
}
