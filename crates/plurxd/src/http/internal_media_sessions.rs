//! Exact-auth worker endpoints for cluster-owned HLS sessions.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::peer_transport::exact_auth_from_headers;
use crate::media_sessions::{
    unix_ms, RelayRequest, RelayResource, RemoteAbortRequest, RemoteStartRequest,
    RemoteStartResponse, ABORT_PATH, ACTIVATION_CONFIRMATION_WINDOW, RELAY_PATH, START_PATH,
};
use crate::state::AppState;

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    path: &'static str,
    body: &[u8],
) -> Result<(), StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if state
        .membership
        .authorize_internal_peer_request(&auth, "POST", path, body)
        .await
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn authorize_read(
    state: &AppState,
    headers: &HeaderMap,
    path: &'static str,
    body: &[u8],
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

pub(crate) async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<RemoteStartResponse>, StatusCode> {
    authorize(&state, &headers, START_PATH, &body).await?;
    let request = serde_json::from_slice::<RemoteStartRequest>(&body)
        .ok()
        .filter(RemoteStartRequest::is_valid)
        .ok_or(StatusCode::BAD_REQUEST)?;
    if !state.media_pool.remote_placement_ready(&state).await {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let user = state
        .store
        .get_user(request.user_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let response = state
        .transcode
        .create_session(&request.request, &user.username)
        .await
        .map(RemoteStartResponse::from)
        .map_err(|error| {
            if error.contains("already used") {
                StatusCode::CONFLICT
            } else if crate::transcode::is_retryable_capacity_error(&error) {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            }
        })?;
    let confirmation_state = state.clone();
    let confirmation_incarnation = request.incarnation_id.clone();
    let confirmation_session = response.session_id.clone();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + ACTIVATION_CONFIRMATION_WINDOW;
        loop {
            match confirmation_state
                .store
                .media_session_route_by_incarnation(&confirmation_incarnation)
                .await
            {
                Ok(Some(route))
                    if route.session_id == confirmation_session
                        && route.owner_node_id == confirmation_state.node_id
                        && route.state == "active"
                        && route.lease_expires_at_ms > unix_ms() =>
                {
                    confirmation_state
                        .media_sessions
                        .seed_owned_lease(&route)
                        .await;
                    return;
                }
                Ok(Some(_)) => break,
                Ok(None) | Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Ok(None) | Err(_) => break,
            }
        }
        confirmation_state
            .transcode
            .stop_session_for_request(
                &confirmation_incarnation,
                &confirmation_session,
                "cluster activation not confirmed",
            )
            .await;
    });
    Ok(Json(response))
}

pub(crate) async fn abort(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    authorize(&state, &headers, ABORT_PATH, &body).await?;
    let request = serde_json::from_slice::<RemoteAbortRequest>(&body)
        .ok()
        .filter(RemoteAbortRequest::is_valid)
        .ok_or(StatusCode::BAD_REQUEST)?;
    state
        .transcode
        .stop_session_for_request(
            &request.incarnation_id,
            &request.session_id,
            "cluster start aborted",
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn relay(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(request) = serde_json::from_slice::<RelayRequest>(&body)
        .ok()
        .filter(RelayRequest::is_valid)
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let authorization = if matches!(&request.resource, RelayResource::Delete) {
        authorize(&state, &headers, RELAY_PATH, &body).await
    } else {
        authorize_read(&state, &headers, RELAY_PATH, &body).await
    };
    if let Err(status) = authorization {
        return status.into_response();
    }
    let route = match state.media_sessions.route(&request.session_id).await {
        Ok(Some(route)) => route,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if route.owner_node_id != state.node_id {
        return StatusCode::CONFLICT.into_response();
    }
    if !matches!(&request.resource, RelayResource::Delete)
        && (route.state != "active" || route.lease_expires_at_ms <= unix_ms())
    {
        return StatusCode::GONE.into_response();
    }
    super::hls::relay_local(&state, request).await
}
