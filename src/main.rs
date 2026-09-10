use crate::types::SfuMessage;
use ::tracing::error;
use std::sync::mpsc;
use str0m::crypto::from_feature_flags;

mod config;
mod servers;
mod sfu;
mod types;
mod utils;

#[tokio::main]
async fn main() {
    utils::log::init_log();
    config::init_tuning();

    let config = config::load_web_config().await;

    from_feature_flags().install_process_default();

    let (addr, socket) = sfu::socket::bind_udp_socket();
    let (tx, rx) = mpsc::sync_channel::<SfuMessage>(32);

    std::thread::spawn(move || {
        if let Err(e) = sfu::run::run(rx, socket, addr) {
            error!("udp error : {}", e);
        };
    });

    if let Err(e) = servers::web::run(addr, tx, config).await {
        error!("web server erorr : {}", e);
    };
}
