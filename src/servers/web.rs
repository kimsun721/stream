use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    time::Instant,
};

use axum::{
    Json, RequestPartsExt, Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{
        StatusCode,
        header::{self},
        request::Parts,
    },
    middleware::{self, Next},
    response::{Html, IntoResponse, Response, Result},
    routing,
};
use axum_extra::{
    TypedHeader,
    headers::{Authorization, ContentType, Mime, authorization::Bearer},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use str0m::{
    Candidate, Rtc,
    bwe::Bitrate,
    change::{SdpAnswer, SdpOffer},
};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::error;
use uuid::Uuid;

use crate::{
    config::{WebConfig, tuning},
    metrics,
    shards::types::Shards,
    types::{RoomId, RoomState, SfuMessage, StreamKey},
    utils::string::hash_string,
};

#[derive(Clone)]
struct SdpState {
    addr: SocketAddr,
    shards: Arc<Shards>,
}

#[derive(Clone)]
struct ApiState {
    api_key: String,
    shards: Arc<Shards>,
}

#[derive(Deserialize)]
struct OfferRequest {
    #[serde(rename = "type")]
    _sdp_type: String,
    sdp: String,
    room_id: String,
}

struct AuthStreamKey(RoomId);

pub async fn run(shards: Arc<Shards>, config: WebConfig) -> anyhow::Result<()> {
    let addr = shards.advertised_addr;

    let https_api = Router::new()
        .route("/watch", routing::get(watch_page))
        .route("/offer", routing::post(sdp_offer))
        .route("/whip", routing::post(whip_sdp_offer))
        .route(
            "/whip/sessions/{session_id}",
            routing::delete(whip_terminate_session),
        )
        .layer(CorsLayer::permissive())
        .with_state(SdpState {
            addr,
            shards: shards.clone(),
        });

    let api_state = ApiState {
        shards,
        api_key: config.api_key,
    };

    let http_api = Router::new()
        .route("/metrics.json", routing::get(get_metrics))
        .route("/rooms", routing::post(create_room))
        .route("/rooms/{room_id}", routing::get(get_room))
        .route("/rooms/{room_id}", routing::patch(update_room))
        .route("/rooms/{room_id}", routing::delete(delete_room))
        .route(
            "/rooms/{room_id}/stream-key",
            routing::post(reissue_stream_key),
        )
        .layer(middleware::from_fn_with_state(
            api_state.clone(),
            api_key_auth_middleware,
        ))
        .with_state(api_state);

    let https_server = tokio::spawn(async move {
        let sdp_addr = SocketAddr::new(
            Ipv4Addr::UNSPECIFIED.into(),
            tuning().server.https_sdp_server_port,
        );

        match config.certificate {
            Some(certificate) => axum_server::bind_rustls(sdp_addr, certificate)
                .serve(https_api.into_make_service())
                .await
                .unwrap_or_else(|e| panic!("serving sdp api on {sdp_addr}; error={e}")),
            None => {
                let listener = TcpListener::bind(sdp_addr)
                    .await
                    .unwrap_or_else(|e| panic!("binding {sdp_addr}; error={e}"));

                axum::serve(listener, https_api)
                    .await
                    .unwrap_or_else(|e| panic!("serving sdp api on {sdp_addr}; error={e}"));
            }
        }
    });

    let http_server = tokio::spawn(async move {
        let rest_addr = SocketAddr::new(
            Ipv4Addr::UNSPECIFIED.into(),
            tuning().server.http_rest_server_port,
        );

        let listener = TcpListener::bind(rest_addr)
            .await
            .unwrap_or_else(|e| panic!("binding {rest_addr}; error={e}"));

        axum::serve(listener, http_api)
            .await
            .unwrap_or_else(|e| panic!("serving rest api on {rest_addr}; error={e}"));
    });

    // Unauthenticated, for a scraper on a private network. Off unless a port is
    // configured.
    if let Some(port) = tuning().server.metrics_port {
        let metrics_api = Router::new().route("/metrics", routing::get(get_prometheus_metrics));

        tokio::spawn(async move {
            let metrics_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port);

            let listener = TcpListener::bind(metrics_addr)
                .await
                .unwrap_or_else(|e| panic!("binding {metrics_addr}; error={e}"));

            axum::serve(listener, metrics_api)
                .await
                .unwrap_or_else(|e| panic!("serving metrics on {metrics_addr}; error={e}"));
        });
    }

    let _ = tokio::join!(https_server, http_server);

    Ok(())
}

async fn api_key_auth_middleware(
    State(state): State<ApiState>,
    header: Option<TypedHeader<Authorization<Bearer>>>,
    request: Request,
    next: Next,
) -> Result<Response, impl IntoResponse> {
    let response_header = [(header::WWW_AUTHENTICATE, "Bearer")];
    let Some(TypedHeader(Authorization(bearer))) = header else {
        return Err((StatusCode::UNAUTHORIZED, response_header));
    };

    let is_token_valid = bearer
        .token()
        .as_bytes()
        .ct_eq(state.api_key.as_bytes())
        .into();

    if is_token_valid {
        Ok(next.run(request).await)
    } else {
        Err((StatusCode::UNAUTHORIZED, response_header))
    }
}

impl FromRequestParts<SdpState> for AuthStreamKey {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SdpState,
    ) -> Result<Self, Self::Rejection> {
        let unauthorized = || {
            (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
            )
                .into_response()
        };

        let TypedHeader(Authorization(bearer)) = parts
            .extract::<TypedHeader<Authorization<Bearer>>>()
            .await
            .map_err(|_| unauthorized())?;

        let hashed_stream_key = hash_string(bearer.token());

        let (tx, rx) = mpsc::sync_channel::<Option<RoomId>>(state.shards.shards.len());

        for sfu_tx in state.shards.senders() {
            let msg = SfuMessage::ResolveStreamKey {
                hashed_stream_key,
                reply: tx.clone(),
            };
            sfu_tx.send(msg).map_err(|e| {
                error!("send to sfu loop failed: {}", e);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            })?;
        }

        drop(tx);

        rx.iter()
            .find_map(|reply| reply)
            .map(AuthStreamKey)
            .ok_or_else(unauthorized)
    }
}

#[derive(Serialize)]
struct CreateRoomResponse {
    room_id: String,
    stream_key: String,
}

async fn create_room(State(state): State<ApiState>) -> Result<impl IntoResponse, StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<(RoomId, StreamKey)>(1);

    let room_id = RoomId::new(state.shards.next_shard());

    let Some(sfu_tx) = state.shards.sender_for(&room_id) else {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };

    let msg = SfuMessage::CreateRoom { room_id, reply: tx };

    sfu_tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (room_id, stream_key) = rx.recv().map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok((
        StatusCode::CREATED,
        Json(CreateRoomResponse {
            room_id: room_id.0,
            stream_key: stream_key.0,
        }),
    ))
}

/// Cumulative since start, so a caller takes two readings and subtracts. A read
/// that reset them would spoil the next one.
async fn get_metrics() -> Json<metrics::Snapshot> {
    Json(metrics::snapshot())
}

async fn get_prometheus_metrics() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, metrics::exposition::CONTENT_TYPE)],
        metrics::exposition::render(),
    )
}

#[derive(Serialize)]
struct GetRoomResponse {
    views: usize,
    state: RoomState,
}

async fn get_room(
    Path(room_id): Path<String>,
    State(state): State<ApiState>,
) -> Result<Json<GetRoomResponse>, StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<(usize, RoomState)>>(1);
    let room_id = RoomId(room_id);

    let Some(sfu_tx) = state.shards.sender_for(&room_id) else {
        return Err(StatusCode::NOT_FOUND);
    };

    sfu_tx.send(SfuMessage::GetRoom { room_id, reply: tx }).ok();

    let (views, state) = rx
        .recv()
        .map_err(|e| {
            error!("send to sfu loop failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(GetRoomResponse { views, state }))
}

#[derive(Deserialize)]
struct UpdateRoomState {
    state: RoomState,
}

async fn update_room(
    State(state): State<ApiState>,
    Path(room_id): Path<String>,
    Json(payload): Json<UpdateRoomState>,
) -> Result<(), StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<()>>(1);
    let room_id = RoomId(room_id);

    let Some(sfu_tx) = state.shards.sender_for(&room_id) else {
        return Err(StatusCode::NOT_FOUND);
    };

    let msg = SfuMessage::UpdateRoomState {
        room_id,
        state: payload.state,
        reply: tx,
    };

    sfu_tx.send(msg).map_err(|e| {
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
    Path(room_id): Path<String>,
    State(state): State<ApiState>,
) -> Result<(), StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<()>>(1);
    let room_id = RoomId(room_id);

    let Some(sfu_tx) = state.shards.sender_for(&room_id) else {
        return Err(StatusCode::NOT_FOUND);
    };

    let msg = SfuMessage::DeleteRoom { room_id, reply: tx };

    sfu_tx.send(msg).map_err(|e| {
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

#[derive(Serialize)]
struct ReissueStreamKeyResponse {
    stream_key: String,
}

async fn reissue_stream_key(
    State(state): State<ApiState>,
    Path(room_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<StreamKey>>(1);
    let room_id = RoomId(room_id);

    let Some(sfu_tx) = state.shards.sender_for(&room_id) else {
        return Err(StatusCode::NOT_FOUND);
    };

    let msg = SfuMessage::ReissueStreamKey { room_id, reply: tx };

    sfu_tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let stream_key = rx.recv().map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let Some(stream_key) = stream_key else {
        return Err(StatusCode::NOT_FOUND);
    };

    Ok((
        StatusCode::OK,
        Json(ReissueStreamKeyResponse {
            stream_key: stream_key.0,
        }),
    ))
}

/// A minimal viewer, so the quick start ends at a picture rather than at an API.
async fn watch_page() -> Html<&'static str> {
    Html(include_str!("../../static/watch.html"))
}

async fn sdp_offer(
    State(state): State<SdpState>,
    Json(payload): Json<OfferRequest>,
) -> Result<Json<Value>, StatusCode> {
    let OfferRequest {
        _sdp_type,
        sdp,
        room_id,
    } = payload;

    let (rtc, answer) = viewer_rtc_from_offer(sdp, state.addr)?;
    let session_id = Uuid::new_v4();
    let (tx, rx) = mpsc::sync_channel::<Option<StatusCode>>(1);
    let room_id = RoomId(room_id);

    let Some(sfu_tx) = state.shards.sender_for(&room_id) else {
        return Err(StatusCode::NOT_FOUND);
    };

    let msg = SfuMessage::RegisterViewer {
        rtc: Box::from(rtc),
        room_id,
        session_id,
        reply: tx,
    };

    register_client(msg, rx, sfu_tx.to_owned())?;

    let value = serde_json::to_value(&answer).map_err(|e| {
        error!("json to value failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(value))
}

async fn whip_sdp_offer(
    State(state): State<SdpState>,
    AuthStreamKey(room_id): AuthStreamKey,
    TypedHeader(content_type): TypedHeader<ContentType>,
    sdp: String,
) -> Result<impl IntoResponse, StatusCode> {
    let mime: Mime = content_type.into();

    if mime.essence_str() != "application/sdp" {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    };

    let (rtc, answer) = streamer_rtc_from_offer(sdp, state.addr)?;
    let session_id = Uuid::new_v4();
    let location = format!("/whip/sessions/{}", session_id);
    let (tx, rx) = mpsc::sync_channel::<Option<StatusCode>>(1);

    let Some(sfu_tx) = state.shards.sender_for(&room_id) else {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };

    let msg = SfuMessage::RegisterStreamer {
        rtc: Box::from(rtc),
        room_id,
        session_id,
        reply: tx,
    };

    register_client(msg, rx, sfu_tx.to_owned())?;

    Ok((
        StatusCode::CREATED,
        [
            (header::CONTENT_TYPE, "application/sdp".to_string()),
            (header::LOCATION, location),
        ],
        answer.to_sdp_string(),
    ))
}

async fn whip_terminate_session(
    Path(session_id): Path<Uuid>,
    AuthStreamKey(room_id): AuthStreamKey,
    State(state): State<SdpState>,
) -> Result<StatusCode, StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<()>>(1);

    let Some(sfu_tx) = state.shards.sender_for(&room_id) else {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };

    let msg = SfuMessage::TerminateSession {
        room_id,
        session_id,
        reply: tx,
    };

    sfu_tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    rx.recv()
        .map_err(|e| {
            error!("send to sfu loop failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .map(|_| StatusCode::NO_CONTENT)
        .ok_or(StatusCode::NOT_FOUND)
}

fn viewer_rtc_from_offer(sdp: String, addr: SocketAddr) -> Result<(Rtc, SdpAnswer), StatusCode> {
    let mut rtc = Rtc::builder()
        .enable_bwe(Some(Bitrate::mbps(tuning().bitrate.initial_mbps)))
        .build(Instant::now());

    rtc.bwe()
        .set_desired_bitrate(Bitrate::mbps(tuning().bitrate.desired_mbps));

    accept_offer(rtc, addr, sdp)
}

fn streamer_rtc_from_offer(sdp: String, addr: SocketAddr) -> Result<(Rtc, SdpAnswer), StatusCode> {
    let rtc = Rtc::builder().build(Instant::now());

    accept_offer(rtc, addr, sdp)
}

fn accept_offer(
    mut rtc: Rtc,
    addr: SocketAddr,
    sdp: String,
) -> Result<(Rtc, SdpAnswer), StatusCode> {
    rtc.add_local_candidate(Candidate::host(addr, "udp").map_err(|e| {
        error!("add local candidate failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?);

    let offer = SdpOffer::from_sdp_string(&sdp).map_err(|_| StatusCode::BAD_REQUEST)?;

    let answer = rtc.sdp_api().accept_offer(offer).map_err(|e| {
        error!("accept offer failed: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    Ok((rtc, answer))
}

fn register_client(
    msg: SfuMessage,
    rx: Receiver<Option<StatusCode>>,
    sfu_tx: SyncSender<SfuMessage>,
) -> Result<(), StatusCode> {
    sfu_tx.send(msg).map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if let Some(status_code) = rx.recv().map_err(|e| {
        error!("send to sfu loop failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })? {
        return Err(status_code);
    };

    Ok(())
}
