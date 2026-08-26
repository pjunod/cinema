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
    RemoteStartResponse, ABORT_PATH, CONTROL_PATH, RELAY_PATH,
    REMOTE_ACTIVATION_CONFIRMATION_WINDOW, START_DEADLINE, START_PATH,
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
    let start_deadline = tokio::time::Instant::now() + START_DEADLINE;
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
    // The worker publication and its activation-confirmation watcher are one
    // owned operation. If the peer disconnects after ffmpeg starts, dropping
    // this HTTP future cannot strand the worker before the watcher is armed.
    let start_state = state.clone();
    let start_task = tokio::spawn(async move {
        let started = start_state
            .transcode
            .create_cluster_session(
                &request.request,
                request.user_id,
                &user.username,
                start_deadline,
            )
            .await?;
        let provisional_pin_ms =
            i64::try_from(REMOTE_ACTIVATION_CONFIRMATION_WINDOW.as_millis()).unwrap_or(i64::MAX);
        if !start_state
            .transcode
            .pin_shared_session(
                &started.info.session_id,
                &request.incarnation_id,
                1,
                unix_ms().saturating_add(provisional_pin_ms),
            )
            .await
            .map_err(|error| error.to_string())?
        {
            start_state
                .transcode
                .stop_session_for_request(
                    &request.incarnation_id,
                    &started.info.session_id,
                    "shared cache pin changed",
                )
                .await;
            return Err("shared cache generation changed before activation".to_owned());
        }
        let response = RemoteStartResponse::from(started.info);
        let confirmation_state = start_state.clone();
        let confirmation_incarnation = request.incarnation_id.clone();
        let confirmation_session = response.session_id.clone();
        tokio::spawn(async move {
            // A second replacement on this worker cannot reap this worker
            // before its durable activation has been confirmed or refused.
            let _replacement = started.replacement;
            let deadline = tokio::time::Instant::now() + REMOTE_ACTIVATION_CONFIRMATION_WINDOW;
            loop {
                match tokio::time::timeout_at(
                    deadline,
                    confirmation_state
                        .store
                        .media_session_route_by_incarnation(&confirmation_incarnation),
                )
                .await
                {
                    Ok(Ok(Some(route)))
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
                    Ok(Ok(Some(_))) => break,
                    Ok(Ok(None)) | Ok(Err(_)) if tokio::time::Instant::now() < deadline => {
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        tokio::time::sleep(Duration::from_millis(500).min(remaining)).await;
                    }
                    Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
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
        Ok::<RemoteStartResponse, String>(response)
    });
    let response = start_task
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|error| {
            if error.contains("already used") {
                StatusCode::CONFLICT
            } else if crate::transcode::is_retryable_capacity_error(&error) {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            }
        })?;
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

/// Exact-write-authenticated control relay. The envelope repeats the durable
/// owner tuple so a delayed peer request cannot mutate whichever owner happens
/// to hold the public session id when it arrives.
pub(crate) async fn control(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(request) =
        serde_json::from_slice::<crate::playback_control::ControlRelayRequest>(&body)
            .ok()
            .filter(crate::playback_control::ControlRelayRequest::is_valid)
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if let Err(status) = authorize(&state, &headers, CONTROL_PATH, &body).await {
        return status.into_response();
    }
    let route = match state.store.media_session_route(&request.session_id).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return super::hls::control_error(
                StatusCode::NOT_FOUND,
                "session_gone",
                "no durable media session has this capability",
                None,
                None,
                None,
                None,
            )
        }
        Err(_) => {
            return super::hls::control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable media route is temporarily unavailable",
                None,
                None,
                Some(500),
                None,
            )
        }
    };
    let owner_epoch = u64::try_from(route.owner_epoch).ok();
    if route.owner_node_id != state.node_id
        || route.owner_node_id != request.expected_owner_node_id
        || route.owner_epoch != request.expected_owner_epoch
    {
        return super::hls::control_error(
            StatusCode::CONFLICT,
            "owner_changed",
            "the durable media owner changed before the relayed control arrived",
            Some(route.incarnation_id),
            owner_epoch,
            None,
            None,
        );
    }
    if route.incarnation_id != request.generation {
        return super::hls::control_error(
            StatusCode::CONFLICT,
            "stale_control",
            "the durable media generation changed before the relayed control arrived",
            Some(route.incarnation_id),
            owner_epoch,
            None,
            None,
        );
    }
    if route.state != "active" {
        return super::hls::control_error(
            StatusCode::GONE,
            "session_ended",
            "this media session has ended or been superseded",
            Some(route.incarnation_id),
            owner_epoch,
            None,
            None,
        );
    }
    if route.lease_expires_at_ms <= unix_ms() {
        return super::hls::control_error(
            StatusCode::TOO_EARLY,
            "owner_transition",
            "the media owner lease expired and takeover is not yet settled",
            Some(route.incarnation_id),
            owner_epoch,
            Some(500),
            None,
        );
    }
    super::hls::control_local(&state, &route, request.control).await
}
