//! Exact-auth owner endpoint for bounded HDHomeRun readiness/lineup snapshots.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::Json;

use super::peer_transport::exact_auth_from_headers;
use crate::live_tv::{LiveTvSnapshot, SnapshotRequest, SNAPSHOT_PATH};
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
    authorize(&state, &headers, &body).await?;
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

async fn authorize(state: &AppState, headers: &HeaderMap, body: &[u8]) -> Result<(), StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if state
        .membership
        .authorize_internal_peer_read_request(&auth, "POST", SNAPSHOT_PATH, body)
        .await
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
