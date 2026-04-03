use std::{net::SocketAddr, sync::mpsc::SyncSender, time::Instant};

use axum::{Json, Router, extract::State, routing};
use axum_server::tls_rustls::RustlsConfig;
use serde::Deserialize;
use serde_json::Value;
use str0m::{Candidate, Rtc, change::SdpOffer};
use tower_http::cors::CorsLayer;

use crate::types::{ClientRole, RoomId};

#[derive(Clone)]
struct AppState {
    addr: SocketAddr,
    tx: SyncSender<(Rtc, ClientRole, RoomId)>,
}

#[derive(Deserialize)]
struct OfferRequest {
    #[serde(rename = "type")]
    _sdp_type: String,
    sdp: String,
    role: ClientRole,
    room_id: u64,
}

pub async fn run(addr: SocketAddr, tx: SyncSender<(Rtc, ClientRole, RoomId)>) {
    let config = RustlsConfig::from_pem_file("certs/cer.pem", "certs/key.pem")
        .await
        .expect("load pem files");

    let api = Router::new()
        .route("/offer", routing::post(sdp_offer))
        .layer(CorsLayer::permissive())
        .with_state(AppState { addr, tx });

    let https_server = tokio::spawn(async move {
        axum_server::bind_rustls("0.0.0.0:8080".parse::<SocketAddr>().unwrap(), config)
            .serve(api.into_make_service())
            .await
            .expect("bind to 0.0.0.0:8080");
    });

    let _ = tokio::join!(https_server);
}

async fn sdp_offer(
    State(state): State<AppState>,
    Json(payload): Json<OfferRequest>,
) -> Json<Value> {
    let AppState { addr, tx } = state;
    let OfferRequest {
        _sdp_type,
        sdp,
        role,
        room_id,
    } = payload;

    let mut rtc = Rtc::builder().build(Instant::now());
    rtc.add_local_candidate(Candidate::host(addr, "udp").expect("host candidate"));

    let offer = SdpOffer::from_sdp_string(&sdp).expect("valid SDP");
    let answer = rtc
        .sdp_api()
        .accept_offer(offer)
        .expect("offer to be accepted");

    tx.send((rtc, role, RoomId(room_id)))
        .expect("to send Rtc instance");

    Json(serde_json::to_value(&answer).expect("answer to serialize"))
}
