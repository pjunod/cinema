//! Exact-auth owner endpoint for bounded HDHomeRun readiness/lineup snapshots.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::peer_transport::exact_auth_from_headers;
use crate::live_tv::{
    LiveTvActivateRequest, LiveTvDrainRequest, LiveTvResourceRequest, LiveTvSnapshot,
    LiveTvStartRequest, LiveTvStopRequest, SnapshotRequest, ACTIVATE_PATH, DRAIN_PATH,
    RESOURCE_PATH, SNAPSHOT_PATH, START_PATH, STOP_PATH,
};
use crate::state::AppState;

pub(crate) async fn snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<([(HeaderName, &'static str); 1], Json<LiveTvSnapshot>), StatusCode> {
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
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(snapshot),
    ))
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
        Ok(response) => Json(response).into_response(),
        Err(error) => wire_error(error),
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
        Ok(response) => Json(response).into_response(),
        Err(error) => wire_error(error),
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
    state
        .live_tv
        .resource_local(request)
        .await
        .map_err(error_status)
}

pub(crate) async fn stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    authorize(&state, &headers, &body, STOP_PATH).await?;
    let request =
        serde_json::from_slice::<LiveTvStopRequest>(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if request.expected_owner_node_id != state.node_id {
        return Err(StatusCode::CONFLICT);
    }
    state
        .live_tv
        .stop_local(&request.capability)
        .await
        .map_err(error_status)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn drain(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let request =
        serde_json::from_slice::<LiveTvDrainRequest>(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    authorize_voter(&state, &headers, &body, DRAIN_PATH, None).await?;
    if request.expected_owner_node_id != state.node_id {
        return Err(StatusCode::CONFLICT);
    }
    let drained = state
        .live_tv
        .drain_stale(request.keep_generation)
        .await
        .map_err(error_status)?;
    Ok(Json(serde_json::json!({ "drained": drained })))
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

fn wire_error(error: crate::live_tv::LiveTvError) -> Response {
    let status = error_status(error.clone());
    (
        status,
        Json(serde_json::json!({
            "code": error.code(),
            "message": error.to_string(),
        })),
    )
        .into_response()
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
