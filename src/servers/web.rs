use std::{collections::hash_map::Entry, net::SocketAddr, sync::mpsc::SyncSender, time::Instant};

use axum::{
    Error, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::Result,
    routing,
};
use axum_server::tls_rustls::RustlsConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use str0m::{Candidate, Rtc, change::SdpOffer};
use tokio::{net::TcpListener, task::JoinError};
use tower_http::cors::CorsLayer;

use crate::types::{ClientRole, Room, RoomId, RoomState, Rooms};

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

pub async fn run(
    addr: SocketAddr,
    tx: SyncSender<(Rtc, ClientRole, RoomId)>,
    rooms: Rooms,
) -> anyhow::Result<()> {
    let config = RustlsConfig::from_pem_file("certs/cer.pem", "certs/key.pem").await?;

    let https_api = Router::new()
        .route("/offer", routing::post(sdp_offer))
        .layer(CorsLayer::permissive())
        .with_state(SdpState { addr, tx });

    let http_api = Router::new()
        .route("/rooms/{room_id}", routing::post(create_room))
        .route("/rooms/{room_id}", routing::get(get_room))
        .route("/rooms/{room_id}", routing::patch(update_room))
        .route("/rooms/{room_id}", routing::delete(delete_room))
        .with_state(ApiState { rooms });

    let https_server = tokio::spawn(async move {
        axum_server::bind_rustls(
            "0.0.0.0:8080"
                .parse::<SocketAddr>()
                .expect("bind to 0.0.0.0:8080"),
            config,
        )
        .serve(https_api.into_make_service())
        .await
        .expect("start to 0.0.0.0:8080");
    });

    let http_server = tokio::spawn(async move {
        axum::serve(
            TcpListener::bind("0.0.0.0:8443")
                .await
                .expect("bind to 0.0.0.0:8443"),
            http_api,
        )
        .await
        .expect("start to 0.0.0.0:8443");
    });

    let _ = tokio::join!(https_server, http_server);

    Ok(())
}

async fn create_room(Path(room_id): Path<u64>, State(state): State<ApiState>) -> StatusCode {
    match state.rooms.lock().unwrap().entry(room_id) {
        Entry::Occupied(_) => StatusCode::CONFLICT,
        Entry::Vacant(e) => {
            e.insert(Room {
                streamer_id: None,
                clients: vec![],
                state: RoomState::IDLE,
            });
            StatusCode::CREATED
        }
    }
}

#[derive(Serialize)]
struct GetRoomResponse {
    views: usize,
}

async fn get_room(
    Path(room_id): Path<u64>,
    State(state): State<ApiState>,
) -> Result<Json<GetRoomResponse>, StatusCode> {
    if let Some(room) = state.rooms.lock().unwrap().get(&room_id) {
        let views = room
            .clients
            .iter()
            .filter(|c| c.role == ClientRole::Viewer)
            .count();

        let res = Json(GetRoomResponse { views });
        return Ok(res);
    };
    Err(StatusCode::NOT_FOUND)
}

#[derive(Deserialize)]
struct UpdateRoomState {
    state: RoomState,
}

async fn update_room(
    State(state): State<ApiState>,
    Path(room_id): Path<u64>,
    Json(payload): Json<UpdateRoomState>,
) -> StatusCode {
    if let Some(room) = state.rooms.lock().unwrap().get_mut(&room_id) {
        room.state = payload.state;

        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn delete_room(Path(room_id): Path<u64>, State(state): State<ApiState>) -> StatusCode {
    match state.rooms.lock().unwrap().remove(&room_id) {
        Some(_) => StatusCode::OK,
        None => StatusCode::NOT_FOUND,
    }
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

    rtc.sdp_api().add_channel("sdp".to_string());

    tx.send((rtc, role, RoomId(room_id)))
        .expect("to send Rtc instance");

    Json(serde_json::to_value(&answer).expect("answer to serialize"))
}
