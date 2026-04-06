use std::{net::SocketAddr, sync::mpsc::SyncSender, time::Instant};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing,
};
use axum_server::tls_rustls::RustlsConfig;
use serde::Deserialize;
use serde_json::Value;
use str0m::{Candidate, Rtc, change::SdpOffer};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

use crate::types::{ClientRole, RoomId, RoomState, Rooms};

#[derive(Clone)]
struct SdpState {
    addr: SocketAddr,
    tx: SyncSender<(Rtc, ClientRole, RoomId)>,
}

#[derive(Clone)]
struct ApiState {
    rooms: Rooms,
}

#[derive(Deserialize)]
struct OfferRequest {
    #[serde(rename = "type")]
    _sdp_type: String,
    sdp: String,
    role: ClientRole,
    room_id: u64,
}

pub async fn run(addr: SocketAddr, tx: SyncSender<(Rtc, ClientRole, RoomId)>, rooms: Rooms) {
    let config = RustlsConfig::from_pem_file("certs/cer.pem", "certs/key.pem")
        .await
        .expect("load pem files");

    let https_api = Router::new()
        .route("/offer", routing::post(sdp_offer))
        .layer(CorsLayer::permissive())
        .with_state(SdpState { addr, tx });

    let https_server = tokio::spawn(async move {
        axum_server::bind_rustls("0.0.0.0:8080".parse::<SocketAddr>().unwrap(), config)
            .serve(https_api.into_make_service())
            .await
            .expect("bind to 0.0.0.0:8080");
    });

    let http_api = Router::new()
        .route("/rooms/{room_id}", routing::post(create_room))
        .route("/rooms/{room_id}", routing::get(get_room))
        .with_state(ApiState { rooms });

    let http_server = tokio::spawn(async move {
        axum::serve(
            TcpListener::bind("0.0.0.0:8443")
                .await
                .expect("bind to 0.0.0.0:8443"),
            http_api,
        )
        .await
        .unwrap();
    });

    let _ = tokio::join!(https_server, http_server);
}

async fn create_room(Path(room_id): Path<u64>) -> StatusCode {
    StatusCode::CREATED
}

async fn get_room(Path(room_id): Path<u64>) -> StatusCode {
    StatusCode::OK
}

#[derive(Deserialize)]
struct UpdateRoomState {
    state: RoomState,
}

async fn update_room(Path(room_id): Path<u64>, Json(payload): Json<UpdateRoomState>) -> StatusCode {
    StatusCode::OK
}

async fn sdp_offer(
    State(state): State<SdpState>,
    Json(payload): Json<OfferRequest>,
) -> Json<Value> {
    let SdpState { addr, tx } = state;
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
