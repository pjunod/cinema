//! Exact-auth owner endpoint for bounded HDHomeRun readiness/lineup snapshots.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::peer_transport::exact_auth_from_headers;
use crate::live_tv::{
    LiveTvActivateRequest, LiveTvDrainAck, LiveTvDrainRequest, LiveTvResourceRequest,
    LiveTvStartRequest, LiveTvStopRequest, SnapshotRequest, ACTIVATE_PATH, DRAIN_PATH,
    RESOURCE_PATH, SNAPSHOT_PATH, START_PATH, STOP_PATH,
};
use crate::state::AppState;

pub(crate) async fn snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    // This is a process-local atomic read and `/readyz` already exposes the
    // same fact. Put it before signature work so a fenced owner cannot reach
    // configuration, network, or FFmpeg code under any authentication shape.
    if !state.serving.is_ready() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    authorize(&state, &headers, &body, SNAPSHOT_PATH).await?;
    if !state
        .membership
        .live_tv_protocol_pending_nodes()
        .await
        .is_ok_and(|nodes| nodes.is_empty())
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let request =
        serde_json::from_slice::<SnapshotRequest>(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let config = state
        .live_tv
        .config()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if config.generation != request.generation || config.owner_node_id != state.node_id {
        return Err(StatusCode::CONFLICT);
    }
    let snapshot = state
        .live_tv
        .local_snapshot(&config, request.force, request.probe_graph)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    signed_json_response(&state, &headers, SNAPSHOT_PATH, StatusCode::OK, &snapshot)
}

pub(crate) async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(status) = require_start_authority(&state).await {
        return status.into_response();
    }
    let request = match serde_json::from_slice::<LiveTvStartRequest>(&body) {
        Ok(request) => request,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if let Err(status) = authorize_voter(
        &state,
        &headers,
        &body,
        START_PATH,
        Some(&request.source_node_id),
    )
    .await
    {
        return status.into_response();
    }
    let Some(_restart_admission) = state.serving.try_restart_admission().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match state.live_tv.start_local(request).await {
        Ok(response) => {
            signed_json_response(&state, &headers, START_PATH, StatusCode::OK, &response)
                .unwrap_or_else(IntoResponse::into_response)
        }
        Err(error) => signed_wire_error(&state, &headers, START_PATH, error),
    }
}

pub(crate) async fn activate(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(status) = require_start_authority(&state).await {
        return status.into_response();
    }
    let request = match serde_json::from_slice::<LiveTvActivateRequest>(&body) {
        Ok(request) => request,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let signer = match authorize_voter(&state, &headers, &body, ACTIVATE_PATH, None).await {
        Ok(signer) => signer,
        Err(status) => return status.into_response(),
    };
    let Some(_restart_admission) = state.serving.try_restart_admission().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match state.live_tv.activate_local(&request, &signer).await {
        Ok(response) => {
            signed_json_response(&state, &headers, ACTIVATE_PATH, StatusCode::OK, &response)
                .unwrap_or_else(IntoResponse::into_response)
        }
        Err(error) => signed_wire_error(&state, &headers, ACTIVATE_PATH, error),
    }
}

pub(crate) async fn resource(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    if !state.serving.is_ready() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    authorize(&state, &headers, &body, RESOURCE_PATH).await?;
    let request = serde_json::from_slice::<LiveTvResourceRequest>(&body)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let content_type = match &request {
        LiveTvResourceRequest::Segment { .. } => None,
        LiveTvResourceRequest::Playlist { .. } => Some("application/vnd.apple.mpegurl"),
        _ => Some("application/json"),
    };
    let response = state.live_tv.resource_local(request).await;
    let response = match response {
        Ok(response) => response,
        Err(error) => return Ok(signed_wire_error(&state, &headers, RESOURCE_PATH, error)),
    };
    if let Some(content_type) = content_type {
        let status = response.status();
        let bytes = axum::body::to_bytes(
            response.into_body(),
            crate::live_tv::MAX_PLAYLIST_BYTES as usize,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        signed_bytes_response(
            &state,
            &headers,
            RESOURCE_PATH,
            status,
            content_type,
            bytes.to_vec(),
        )
    } else {
        Ok(response)
    }
}

pub(crate) async fn stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers, &body, STOP_PATH).await?;
    let request =
        serde_json::from_slice::<LiveTvStopRequest>(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if request.expected_owner_node_id != state.node_id {
        return Err(StatusCode::CONFLICT);
    }
    if let Err(error) = state.live_tv.stop_local(&request.capability).await {
        return Ok(signed_wire_error(&state, &headers, STOP_PATH, error));
    }
    signed_bytes_response(
        &state,
        &headers,
        STOP_PATH,
        StatusCode::NO_CONTENT,
        "application/json",
        Vec::new(),
    )
}

pub(crate) async fn drain(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let request =
        serde_json::from_slice::<LiveTvDrainRequest>(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let signer = authorize_voter(&state, &headers, &body, DRAIN_PATH, None).await?;
    if request.expected_owner_node_id != state.node_id {
        return Err(StatusCode::CONFLICT);
    }
    if request.target_node_id != signer
        || request.drain_before_generation < 0
        || uuid::Uuid::parse_str(&request.request_nonce).is_err()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let config = state.live_tv.config().await.map_err(error_status)?;
    if request.drain_before_generation > config.generation {
        return Err(StatusCode::CONFLICT);
    }
    let drained = state
        .live_tv
        .drain_before(request.drain_before_generation)
        .await
        .map_err(error_status)?;
    let mut ack = LiveTvDrainAck {
        owner_node_id: state.node_id.clone(),
        target_node_id: signer.clone(),
        request_nonce: request.request_nonce,
        drained_before_generation: request.drain_before_generation,
        drained,
        signature: String::new(),
    };
    let payload = ack.signing_payload().map_err(error_status)?;
    ack.signature = state
        .membership
        .sign_internal_peer_response(&signer, &ack.request_nonce, DRAIN_PATH, &payload)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(
        serde_json::to_value(ack).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

async fn require_start_authority(state: &AppState) -> Result<(), StatusCode> {
    if !state.serving.is_ready()
        || !state
            .membership
            .live_tv_protocol_pending_nodes()
            .await
            .is_ok_and(|nodes| nodes.is_empty())
    {
        Err(StatusCode::SERVICE_UNAVAILABLE)
    } else {
        Ok(())
    }
}

fn error_status(error: crate::live_tv::LiveTvError) -> StatusCode {
    use crate::live_tv::LiveTvError;
    match error {
        LiveTvError::InvalidConfig(_)
        | LiveTvError::InvalidResponse(_)
        | LiveTvError::ChannelNotFound(_)
        | LiveTvError::DrmUnsupported(_) => StatusCode::BAD_REQUEST,
        LiveTvError::Conflict(_) => StatusCode::CONFLICT,
        LiveTvError::CapabilityExpired(_) => StatusCode::GONE,
        LiveTvError::StartupTimeout(_) => StatusCode::REQUEST_TIMEOUT,
        LiveTvError::Disabled(_)
        | LiveTvError::Capacity(_)
        | LiveTvError::TunerUnavailable(_)
        | LiveTvError::CodecUnsupported(_)
        | LiveTvError::StreamFailed(_)
        | LiveTvError::DeviceUnavailable(_)
        | LiveTvError::OwnerUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn signed_wire_error(
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
    error: crate::live_tv::LiveTvError,
) -> Response {
    let status = error_status(error.clone());
    signed_json_response(
        state,
        headers,
        path,
        status,
        &serde_json::json!({
            "code": error.code(),
            "message": error.to_string(),
        }),
    )
    .unwrap_or_else(IntoResponse::into_response)
}

fn signed_json_response<T: serde::Serialize>(
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
    status: StatusCode,
    value: &T,
) -> Result<Response, StatusCode> {
    let body = serde_json::to_vec(value).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    signed_bytes_response(state, headers, path, status, "application/json", body)
}

fn signed_bytes_response(
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
    status: StatusCode,
    content_type: &'static str,
    body: Vec<u8>,
) -> Result<Response, StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let payload = super::peer_transport::signed_response_payload(status.as_u16(), &body);
    let signature = state
        .membership
        .sign_internal_peer_response(&auth.node_id, &auth.nonce, path, &payload)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(super::peer_transport::RESPONSE_SIGNATURE_HEADER, signature)
        .body(axum::body::Body::from(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    path: &str,
) -> Result<(), StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if state
        .membership
        .authorize_internal_peer_read_request(&auth, "POST", path, body)
        .await
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn authorize_voter(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    path: &str,
    expected_signer: Option<&str>,
) -> Result<String, StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if expected_signer.is_some_and(|expected| expected != auth.node_id) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if state
        .membership
        .authorize_internal_peer_voter_request(&auth, "POST", path, body)
        .await
        .unwrap_or(false)
    {
        Ok(auth.node_id)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
