use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::mpsc::{self, SyncSender},
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
    response::{IntoResponse, Response, Result},
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
    types::{ClientRole, RoomId, RoomState, SfuMessage, StreamKey},
    utils::string::hash_string,
};

#[derive(Clone)]
struct SdpState {
    addr: SocketAddr,
    tx: SyncSender<SfuMessage>,
}

#[derive(Clone)]
struct ApiState {
    api_key: String,
    tx: SyncSender<SfuMessage>,
}

#[derive(Deserialize)]
struct OfferRequest {
    #[serde(rename = "type")]
    _sdp_type: String,
    sdp: String,
    room_id: String,
}

struct AuthStreamKey(RoomId);

pub async fn run(
    addr: SocketAddr,
    tx: SyncSender<SfuMessage>,
    config: WebConfig,
) -> anyhow::Result<()> {
    let https_api = Router::new()
        .route("/offer", routing::post(sdp_offer))
        .route("/whip", routing::post(whip_sdp_offer))
        .route(
            "/whip/sessions/{session_id}",
            routing::delete(whip_terminate_session),
        )
        .layer(CorsLayer::permissive())
        .with_state(SdpState {
            addr,
            tx: tx.clone(),
        });

    let api_state = ApiState {
        tx,
        api_key: config.api_key,
    };

    let http_api = Router::new()
        .route("/metrics", routing::get(get_metrics))
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

        let (tx, rx) = mpsc::sync_channel::<Option<RoomId>>(1);
        let hashed_stream_key = hash_string(bearer.token());

        let msg = SfuMessage::ResolveStreamKey {
            hashed_stream_key,
            reply: tx,
        };

        state.tx.send(msg).map_err(|e| {
            error!("send to sfu loop failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;

        rx.recv()
            .map_err(|e| {
                error!("send to sfu loop failed: {}", e);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            })?
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

    let msg = SfuMessage::CreateRoom { reply: tx };

    state.tx.send(msg).map_err(|e| {
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

    state
        .tx
        .send(SfuMessage::GetRoom {
            room_id: RoomId(room_id),
            reply: tx,
        })
        .ok();

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
    Path(room_id): Path<String>,
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

#[derive(Serialize)]
struct ReissueStreamKeyResponse {
    stream_key: String,
}

async fn reissue_stream_key(
    State(state): State<ApiState>,
    Path(room_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<StreamKey>>(1);

    let msg = SfuMessage::ReissueStreamKey {
        room_id: RoomId(room_id),
        reply: tx,
    };

    state.tx.send(msg).map_err(|e| {
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

async fn sdp_offer(
    State(state): State<SdpState>,
    Json(payload): Json<OfferRequest>,
) -> Result<Json<Value>, StatusCode> {
    let OfferRequest {
        _sdp_type,
        sdp,
        room_id,
    } = payload;
    let role = ClientRole::Viewer;

    let (rtc, answer) = create_rtc_from_offer(sdp, role, state.addr)?;

    let session_id = Uuid::new_v4();

    register_client(rtc, role, RoomId(room_id), state.tx, session_id)?;

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
    let role = ClientRole::Streamer;

    let mime: Mime = content_type.into();

    if mime.essence_str() != "application/sdp" {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    };

    let (rtc, answer) = create_rtc_from_offer(sdp, role, state.addr)?;

    let session_id = Uuid::new_v4();
    let location = format!("/whip/sessions/{}", session_id);

    register_client(rtc, role, room_id, state.tx, session_id)?;

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

    let msg = SfuMessage::TerminateSession {
        room_id,
        session_id,
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
        .map(|_| StatusCode::NO_CONTENT)
        .ok_or(StatusCode::NOT_FOUND)
}

fn create_rtc_from_offer(
    sdp: String,
    role: ClientRole,
    addr: SocketAddr,
) -> Result<(Rtc, SdpAnswer), StatusCode> {
    let mut rtc = {
        let mut builder = Rtc::builder();

        if role == ClientRole::Viewer {
            builder = builder.enable_bwe(Some(Bitrate::mbps(tuning().bitrate.initial_mbps)));
        }

        builder.build(Instant::now())
    };

    if role == ClientRole::Viewer {
        rtc.bwe()
            .set_desired_bitrate(Bitrate::mbps(tuning().bitrate.desired_mbps));
    }

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
    rtc: Rtc,
    role: ClientRole,
    room_id: RoomId,
    sfu_tx: SyncSender<SfuMessage>,
    session_id: Uuid,
) -> Result<(), StatusCode> {
    let (tx, rx) = mpsc::sync_channel::<Option<StatusCode>>(1);

    let msg = SfuMessage::RegisterClient {
        rtc: Box::from(rtc),
        role,
        room_id,
        session_id,
        reply: tx,
    };

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
