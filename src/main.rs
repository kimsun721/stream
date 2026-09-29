use crate::shards::types::Shards;
use ::tracing::error;
use std::sync::Arc;
use str0m::crypto::from_feature_flags;

mod config;
mod metrics;
mod mux;
mod servers;
mod sfu;
mod shards;
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

    let shards = Arc::new(Shards::new(socket, addr));
    let shards_for_mux = shards.clone();

    std::thread::spawn(move || {
        if let Err(e) = mux::run::run(shards_for_mux, socket_for_mux) {
            error!("mux error: {e}");
        };
    });

    if let Err(e) = servers::web::run(shards, config).await {
        error!("web server erorr : {e}");
    };
}
