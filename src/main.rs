use crate::types::SfuMessage;
use ::tracing::error;
use std::sync::mpsc;
use str0m::crypto::from_feature_flags;

mod config;
mod metrics;
mod mux;
mod servers;
mod sfu;
mod types;
mod utils;

#[tokio::main]
async fn main() {
    utils::log::init_log();
    config::init_tuning();
    from_feature_flags().install_process_default();

    let config = config::load_web_config().await;

    let (addr, socket) = sfu::socket::bind_udp_socket();
    let socket_for_mux = socket.try_clone().expect("failed to clone udp socket");

    let (sfu_tx, sfu_rx) = mpsc::sync_channel::<SfuMessage>(2048);
    let sfu_tx_for_mux = sfu_tx.clone();

    std::thread::spawn(move || {
        if let Err(e) = sfu::run::run(sfu_rx, socket, addr) {
            error!("udp error : {e}");
        };
    });

    std::thread::spawn(move || {
        if let Err(e) = mux::run::run(sfu_tx_for_mux, socket_for_mux) {
            error!("mux error: {e}");
        };
    });

    if let Err(e) = servers::web::run(addr, sfu_tx, config).await {
        error!("web server erorr : {e}");
    };
}
