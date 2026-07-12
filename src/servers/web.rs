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
use str0m::{Candidate, Rtc, bwe::Bitrate, change::SdpOffer, media::Rid};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::error;

use crate::types::{
    ClientRole, RoomId, RoomState, SfuMessage, SimulcastLayerProfile, SimulcastTrack,
};

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
        .map_err(|e| {
            error!("send to sfu loop failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
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
        .map_err(|e| {
            error!("send to sfu loop failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
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

    state.tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    rx.recv()
        .map_err(|e| {
            error!("send to sfu loop failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
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

    state.tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    rx.recv()
        .map_err(|e| {
            error!("send to sfu loop failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)
}

async fn sdp_offer(
    State(state): State<SdpState>,
    Json(payload): Json<OfferRequest>,
) -> Result<Json<Value>, StatusCode> {
    const BWE_INITIAL_BITRATE_MBPS: u64 = 4;
    const BWE_DESIRED_BITRATE_MPBS: u64 = 10;

    let OfferRequest {
        _sdp_type,
        sdp,
        role,
        room_id,
    } = payload;
    let (tx, rx) = mpsc::sync_channel::<Option<StatusCode>>(1);

    let mut rtc = Rtc::builder()
        .enable_bwe(Some(Bitrate::mbps(BWE_INITIAL_BITRATE_MBPS)))
        .build(Instant::now());

    rtc.bwe()
        .set_desired_bitrate(Bitrate::mbps(BWE_DESIRED_BITRATE_MPBS));

    rtc.add_local_candidate(Candidate::host(state.addr, "udp").map_err(|e| {
        error!("add local candidate failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?);

    let offer = SdpOffer::from_sdp_string(&sdp).map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut simulcast_tracks = Vec::new();

    if role == ClientRole::Streamer {
        for o in offer.media_lines.iter() {
            if let Some(m) = o.simulcast() {
                let mid = o.mid();

                let mut track = SimulcastTrack {
                    mid,
                    layers: Vec::new(),
                };

                for layer in m.send.iter() {
                    let Some(pairs) = &layer.attributes else {
                        return Err(StatusCode::BAD_REQUEST);
                    };

                    if let Some(max_br) = pairs.iter().find_map(|(name, value)| {
                        if name == "max-br" {
                            value.parse::<u64>().ok()
                        } else {
                            None
                        }
                    }) {
                        let rid = Rid::from(layer.restriction_id.0.as_str());

                        track.layers.push(SimulcastLayerProfile { rid, max_br });
                    } else {
                        return Err(StatusCode::BAD_REQUEST);
                    };
                }
                if track.layers.is_empty() {
                    return Err(StatusCode::BAD_REQUEST);
                }

                simulcast_tracks.push(track);
            }
        }
    }

    let answer = rtc.sdp_api().accept_offer(offer).map_err(|e| {
        error!("accept offer failed: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let msg = SfuMessage::RegisterClient {
        rtc: Box::from(rtc),
        role,
        room_id: RoomId(room_id),
        simulcast_tracks,
        reply: tx,
    };

    state.tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let result = rx.recv().map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if let Some(status_code) = result {
        return Err(status_code);
    };

    let value = serde_json::to_value(&answer).map_err(|e| {
        error!("json to value failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(value))
}
