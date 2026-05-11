use std::{
    net::SocketAddr,
    sync::mpsc::{self, SyncSender},
    time::Instant,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::Result,
    routing,
};
use axum_server::tls_rustls::RustlsConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use str0m::{Candidate, Rtc, change::SdpOffer};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::error;

use crate::types::{ClientRole, RoomId, RoomState, SfuMessage};

#[derive(Clone)]
struct SdpState {
    addr: SocketAddr,
    tx: SyncSender<SfuMessage>,
}

#[derive(Clone)]
struct ApiState {
    tx: SyncSender<SfuMessage>,
}

#[derive(Deserialize)]
struct OfferRequest {
    #[serde(rename = "type")]
    _sdp_type: String,
    sdp: String,
    role: ClientRole,
    room_id: u64,
}

pub async fn run(addr: SocketAddr, tx: SyncSender<SfuMessage>) -> anyhow::Result<()> {
    let config = RustlsConfig::from_pem_file("certs/cer.pem", "certs/key.pem").await?;

    let https_api = Router::new()
        .route("/offer", routing::post(sdp_offer))
        .layer(CorsLayer::permissive())
        .with_state(SdpState {
            addr,
            tx: tx.clone(),
        });

    let http_api = Router::new()
        .route("/rooms/{room_id}", routing::post(create_room))
        .route("/rooms/{room_id}", routing::get(get_room))
        .route("/rooms/{room_id}", routing::patch(update_room))
        .route("/rooms/{room_id}", routing::delete(delete_room))
        .with_state(ApiState { tx });

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

async fn create_room(
    Path(room_id): Path<u64>,
    State(state): State<ApiState>,
) -> Result<(), StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<()>>(1);

    let msg = SfuMessage::CreateRoom {
        room_id: RoomId(room_id),
        reply: tx,
    };

    state.tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    rx.recv()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::CONFLICT)
}

#[derive(Serialize)]
struct GetRoomResponse {
    views: usize,
}

async fn get_room(
    Path(room_id): Path<u64>,
    State(state): State<ApiState>,
) -> Result<Json<GetRoomResponse>, StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<usize>>(1);

    state
        .tx
        .send(SfuMessage::GetViews {
            room_id: RoomId(room_id),
            reply: tx,
        })
        .ok();

    let views = rx
        .recv()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(GetRoomResponse { views }))
}

#[derive(Deserialize)]
struct UpdateRoomState {
    state: RoomState,
}

async fn update_room(
    State(state): State<ApiState>,
    Path(room_id): Path<u64>,
    Json(payload): Json<UpdateRoomState>,
) -> Result<(), StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<()>>(1);

    let msg = SfuMessage::UpdateRoomState {
        room_id: RoomId(room_id),
        state: payload.state,
        reply: tx,
    };
    state.tx.send(msg).ok();

    rx.recv()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)
}

async fn delete_room(
    Path(room_id): Path<u64>,
    State(state): State<ApiState>,
) -> Result<(), StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<()>>(1);

    let msg = SfuMessage::DeleteRoom {
        room_id: RoomId(room_id),
        reply: tx,
    };

    state.tx.send(msg).ok();

    rx.recv()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)
}

async fn sdp_offer(
    State(state): State<SdpState>,
    Json(payload): Json<OfferRequest>,
) -> Result<Json<Value>, StatusCode> {
    let SdpState { addr, tx } = state;
    let OfferRequest {
        _sdp_type,
        sdp,
        role,
        room_id,
    } = payload;

    let mut rtc = Rtc::builder().build(Instant::now());
    rtc.add_local_candidate(Candidate::host(addr, "udp").map_err(|e| {
        error!("add local candidate failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?);

    let offer = SdpOffer::from_sdp_string(&sdp).map_err(|_| StatusCode::BAD_REQUEST)?;

    let answer = rtc.sdp_api().accept_offer(offer).map_err(|e| {
        error!("accept offer failed: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let msg = SfuMessage::RegisterClient {
        rtc,
        role,
        room_id: RoomId(room_id),
    };

    tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let value = serde_json::to_value(&answer).map_err(|e| {
        error!("json to value failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(value))
}
