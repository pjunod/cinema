//! Exact-auth worker endpoints for cluster-owned HLS sessions.

use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::peer_transport::exact_auth_from_headers;
use crate::media_sessions::{
    unix_ms, DurableRouteResolution, RelayRequest, RelayResource, RemoteAbortRequest,
    RemoteActivateRequest, RemoteStartRequest, RemoteStartResponse, ABORT_PATH, ACTIVATE_PATH,
    CONTROL_PATH, RELAY_PATH, REMOTE_ACTIVATION_CONFIRMATION_WINDOW, REMOTE_START_OWNERSHIP_HEADER,
    REMOTE_START_OWNERSHIP_V1, START_DEADLINE, START_PATH,
};
use crate::state::AppState;

pub(crate) enum RemoteStartError {
    Status(StatusCode),
    RestartDrain,
    ServingFence,
}

impl From<StatusCode> for RemoteStartError {
    fn from(status: StatusCode) -> Self {
        Self::Status(status)
    }
}

impl IntoResponse for RemoteStartError {
    fn into_response(self) -> Response {
        match self {
            Self::Status(status) => status.into_response(),
            Self::RestartDrain => {
                let mut response = (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::CONTENT_TYPE, "application/json")],
                    crate::serving_fence::RESTART_DRAIN_JSON,
                )
                    .into_response();
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
                response
            }
            Self::ServingFence => {
                let mut response = (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::CONTENT_TYPE, "application/json")],
                    crate::serving_fence::SERVING_FENCED_JSON,
                )
                    .into_response();
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                response
            }
        }
    }
}

const REMOTE_ABORT_CAPACITY: usize = 128;
const REMOTE_ABORT_WAIT: Duration = Duration::from_secs(5);

fn admit_remote_start_serving_authority(
    state: &AppState,
) -> Result<(crate::serving_fence::ServingAuthority, u64), RemoteStartError> {
    let authority = state.serving.authority();
    let generation = authority.admit().ok_or(RemoteStartError::ServingFence)?;
    Ok((authority, generation))
}

fn remote_start_ownership_v1(headers: &HeaderMap) -> bool {
    headers
        .get(REMOTE_START_OWNERSHIP_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some(REMOTE_START_OWNERSHIP_V1)
}

fn remote_start_success_status(headers: &HeaderMap, created: bool) -> StatusCode {
    let ownership_v1 = remote_start_ownership_v1(headers);
    match (ownership_v1, created) {
        (true, true) => StatusCode::CREATED,
        (true, false) => StatusCode::ALREADY_REPORTED,
        (false, true) => StatusCode::OK,
        // A legacy ingress cannot distinguish a replay and would arm an
        // abort against a worker it did not create. Refuse that one response;
        // its idempotent public retry will recover the durable route instead.
        (false, false) => StatusCode::CONFLICT,
    }
}

pub(super) async fn pin_shared_session_before_deadline<F>(
    deadline: tokio::time::Instant,
    pin: F,
) -> Result<bool, String>
where
    F: std::future::Future<Output = Result<bool, plurx_core::error::StoreError>>,
{
    tokio::time::timeout_at(deadline, pin)
        .await
        .map_err(|_| {
            crate::transcode::start_infrastructure_error(
                "shared cache pin exceeded the start deadline",
            )
        })?
        .map_err(|error| {
            crate::transcode::start_infrastructure_error(format!(
                "pinning the shared cache session: {error}"
            ))
        })
}

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
) -> Result<Response, RemoteStartError> {
    let start_deadline = tokio::time::Instant::now() + START_DEADLINE;
    authorize(&state, &headers, START_PATH, &body).await?;
    let restart_admission = state
        .serving
        .try_restart_admission()
        .await
        .ok_or(RemoteStartError::RestartDrain)?;
    let (serving_authority, admitted_serving_generation) =
        admit_remote_start_serving_authority(&state)?;
    let request = serde_json::from_slice::<RemoteStartRequest>(&body)
        .ok()
        .filter(RemoteStartRequest::is_valid)
        .ok_or(StatusCode::BAD_REQUEST)?;
    if !state.media_pool.remote_placement_ready(&state).await {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into());
    }
    let user = state
        .store
        .get_user(request.user_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let negotiated_ownership = remote_start_ownership_v1(&headers);
    // The worker publication and its activation-confirmation watcher are one
    // owned operation. If the peer disconnects after ffmpeg starts, dropping
    // this HTTP future cannot strand the worker before the watcher is armed.
    let start_state = state.clone();
    let start_task = tokio::spawn(async move {
        let _restart_admission = restart_admission;
        let started = start_state
            .transcode
            .create_cluster_session(
                &request.request,
                request.user_id,
                &user.username,
                start_deadline,
                admitted_serving_generation,
            )
            .await?;
        // Creation publishes the local worker before the shared-cache pin can
        // complete. Keep the exact worker, request claim, and replacement
        // gate owned across every cancellation, error, or panic after that
        // publication point; the confirmation watcher takes this guard below.
        let crate::transcode::ClusterSessionStart {
            info,
            replacement,
            created,
        } = started;
        let guard = Some(if created {
            super::hls::StartedSessionGuard::worker_only(
                start_state.clone(),
                start_state.node_id.clone(),
                request.incarnation_id.clone(),
                info.session_id.clone(),
                request.user_id,
                request.incarnation_id.clone(),
                Some(replacement),
            )
        } else {
            super::hls::StartedSessionGuard::replayed(
                start_state.clone(),
                start_state.node_id.clone(),
                request.incarnation_id.clone(),
                info.session_id.clone(),
                request.user_id,
                request.incarnation_id.clone(),
                Some(replacement),
            )
        });
        if !serving_authority.is_current(admitted_serving_generation) {
            return Err(crate::transcode::serving_fence_error(
                "the node lost authority before the remote worker could be pinned",
            ));
        }
        let provisional_pin_ms =
            i64::try_from(REMOTE_ACTIVATION_CONFIRMATION_WINDOW.as_millis()).unwrap_or(i64::MAX);
        let pinned = pin_shared_session_before_deadline(
            start_deadline,
            start_state.transcode.pin_shared_session(
                &info.session_id,
                &request.incarnation_id,
                1,
                unix_ms().saturating_add(provisional_pin_ms),
            ),
        )
        .await?;
        if !pinned {
            return Err("shared cache generation changed before activation".to_owned());
        }
        if !serving_authority.is_current(admitted_serving_generation) {
            return Err(crate::transcode::serving_fence_error(
                "the node lost authority before remote activation confirmation",
            ));
        }
        let mut response = RemoteStartResponse::from(info);
        if negotiated_ownership {
            response.activation_generation = Some(admitted_serving_generation);
        }
        let confirmation_state = start_state.clone();
        let confirmation_incarnation = request.incarnation_id.clone();
        let confirmation_session = response.session_id.clone();
        tokio::spawn(async move {
            // Ownership crosses into this cancellation-independent watcher
            // before the start task returns its response.
            let mut guard = guard;
            let mut confirmed_lease = None;
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
                            && route.owner_epoch == 1
                            && route.state == "active"
                            && route.publication_ready_at_ms
                                != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
                            && route.lease_expires_at_ms > unix_ms() =>
                    {
                        if !serving_authority.is_current(admitted_serving_generation) {
                            break;
                        }
                        if confirmed_lease.is_some_and(|lease_expires_at_ms| {
                            route.lease_expires_at_ms > lease_expires_at_ms
                        }) {
                            // Both a finite predecessor handoff and a
                            // response-published route enter owner inventory.
                            // A lease advance proves the ordinary lease loop
                            // accepted durable worker ownership; publication
                            // remains fenced until the request resolves, but
                            // the provisional START owner may now retire.
                            if let Some(guard) = guard.as_mut() {
                                guard.disarm();
                            }
                            return;
                        }
                        confirmed_lease = Some(route.lease_expires_at_ms);
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        tokio::time::sleep(Duration::from_millis(500).min(remaining)).await;
                    }
                    Ok(Ok(Some(route)))
                        if route.session_id == confirmation_session
                            && route.owner_node_id == confirmation_state.node_id
                            && route.owner_epoch == 1
                            && route.state == "active"
                            && route.publication_ready_at_ms
                                == plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
                            && route.lease_expires_at_ms > unix_ms()
                            && tokio::time::Instant::now() < deadline =>
                    {
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        tokio::time::sleep(Duration::from_millis(500).min(remaining)).await;
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
            // Dropping the armed guard performs exact local cleanup and
            // releases the replacement gate after an unconfirmed outcome.
        });
        Ok::<(RemoteStartResponse, bool), String>((response, created))
    });
    let (response, created) = start_task
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|error| {
            if error.contains("already used") {
                StatusCode::CONFLICT
            } else if crate::transcode::is_serving_fence_error(&error)
                || crate::transcode::is_start_infrastructure_error(&error)
                || crate::transcode::is_retryable_capacity_error(&error)
            {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            }
        })?;
    let status = remote_start_success_status(&headers, created);
    Ok((status, Json(response)).into_response())
}

pub(crate) async fn activate(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(status) = authorize(&state, &headers, ACTIVATE_PATH, &body).await {
        return status.into_response();
    }
    let Some(request) = serde_json::from_slice::<RemoteActivateRequest>(&body)
        .ok()
        .filter(|request| request.is_valid_for(&state.node_id))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(restart_admission) = state.serving.try_restart_admission().await else {
        return RemoteStartError::RestartDrain.into_response();
    };
    let authority = state.serving.authority();
    if authority.admit() != Some(request.target_generation) {
        return RemoteStartError::ServingFence.into_response();
    }
    if !state
        .transcode
        .owns_session_for_owner(
            &request.activation.incarnation_id,
            &request.activation.session_id,
            1,
        )
        .await
    {
        return StatusCode::CONFLICT.into_response();
    }
    let predecessor_incarnation = request
        .activation
        .expected_predecessor_incarnation_id
        .clone();
    let activation = request.activation.clone();
    let activation_state = state.clone();
    let mut activation_task = tokio::spawn(async move {
        let _restart_admission = restart_admission;
        super::hls::activate_session_under_authority(
            activation_state,
            request.activation,
            None,
            predecessor_incarnation,
            authority,
            request.target_generation,
            None,
        )
        .await
    });
    match tokio::time::timeout(
        crate::media_sessions::ACTIVATION_STORE_DEADLINE,
        &mut activation_task,
    )
    .await
    {
        Ok(Ok(Ok(_))) => StatusCode::NO_CONTENT.into_response(),
        Ok(Ok(Err(error))) => {
            let confirmation_deadline =
                tokio::time::Instant::now() + crate::media_sessions::ACTIVATION_STORE_DEADLINE;
            if super::hls::wait_for_confirmed_activation(&state, &activation, confirmation_deadline)
                .await
                .is_some()
            {
                StatusCode::ACCEPTED.into_response()
            } else {
                error.into_response()
            }
        }
        Ok(Err(error)) => {
            tracing::error!(%error, "target-owned media activation task failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(_) => {
            tokio::spawn(async move {
                match activation_task.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(
                            ?error,
                            "detached target-owned media activation was rejected"
                        );
                    }
                    Err(error) => {
                        tracing::error!(%error, "detached target-owned media activation task failed");
                    }
                }
            });
            StatusCode::ACCEPTED.into_response()
        }
    }
}

pub(crate) async fn abort(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    authorize(&state, &headers, ABORT_PATH, &body).await?;
    let request = serde_json::from_slice::<RemoteAbortRequest>(&body)
        .ok()
        .filter(remote_abort_request_is_valid)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let deadline = tokio::time::Instant::now() + REMOTE_ABORT_WAIT;
    let permit = tokio::time::timeout_at(deadline, remote_abort_slots().acquire_owned())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let settlement = tokio::spawn(async move {
        let _permit = permit;
        settle_remote_abort(state, request).await
    });
    match tokio::time::timeout_at(deadline, settlement).await {
        Ok(Ok(true)) => Ok(StatusCode::NO_CONTENT),
        Ok(Ok(false)) => Err(StatusCode::SERVICE_UNAVAILABLE),
        Ok(Err(error)) => {
            tracing::warn!(%error, "remote exact-abort settlement task failed");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
        // The exact cleanup and its bounded slot survive transport/request
        // cancellation; an idempotent retry may join it through the actor.
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

fn remote_abort_request_is_valid(request: &RemoteAbortRequest) -> bool {
    request.is_valid()
}

fn remote_abort_slots() -> std::sync::Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    std::sync::Arc::clone(
        SLOTS.get_or_init(|| {
            std::sync::Arc::new(tokio::sync::Semaphore::new(REMOTE_ABORT_CAPACITY))
        }),
    )
}

pub(super) async fn settle_remote_abort(state: AppState, request: RemoteAbortRequest) -> bool {
    let reason = remote_abort_reason(&request);
    let terminal = crate::vodserve::Terminal::from_control_reason(reason);
    if let Some(terminal) = terminal {
        let exact_terminal_owner = state
            .store
            .media_session_route_by_incarnation(&request.incarnation_id)
            .await
            .ok()
            .flatten()
            .filter(|route| {
                route.incarnation_id == request.incarnation_id
                    && route.session_id == request.session_id
                    && route.owner_node_id == state.node_id
                    && route.owner_epoch == request.expected_owner_epoch
                    && route.state == "ended"
            });
        let Some(exact_terminal_owner) = exact_terminal_owner else {
            return false;
        };
        let terminal = crate::vodserve::Terminal::from_durable_reason(
            exact_terminal_owner.terminal_reason.as_deref(),
        )
        .unwrap_or(terminal);
        let reason = terminal.control_reason();
        let Some(initial_release) = state
            .media_sessions
            .complete_release_with_route(exact_terminal_owner.clone())
            .await
        else {
            return false;
        };
        state
            .transcode
            .begin_session_terminal(&request.session_id, terminal, reason)
            .await;
        let durable_release = if initial_release.terminal_projection_complete() {
            initial_release
        } else {
            let Some(projected) = state
                .media_sessions
                .complete_terminal_projection(
                    &exact_terminal_owner,
                    plurx_core::domain::MediaSessionProjectionCompletion::PredecessorAcknowledged,
                )
                .await
                .filter(|proof| proof.terminal_projection_complete())
            else {
                return false;
            };
            projected
        };
        state
            .transcode
            // The exact owner/epoch Store row was verified terminal above;
            // fresh requests cannot re-enter through this process-local gate.
            .complete_session_release_durable(&durable_release);
        return true;
    }
    if request.expected_owner_epoch > 0 {
        state
            .transcode
            .stop_session_for_owner(
                &request.incarnation_id,
                &request.session_id,
                request.expected_owner_epoch,
                reason,
            )
            .await
            || (request.expected_owner_epoch == 1
                && state
                    .transcode
                    .stop_vod_session_for_request(
                        &request.incarnation_id,
                        &request.session_id,
                        reason,
                    )
                    .await)
    } else {
        // Wire-legacy peers predate takeover and can only own epoch 1.
        state
            .transcode
            .stop_session_for_request(&request.incarnation_id, &request.session_id, reason)
            .await
    }
}

fn remote_abort_reason(request: &RemoteAbortRequest) -> &'static str {
    match request.reason.as_deref() {
        None | Some("cluster start aborted") => "cluster start aborted",
        Some(reason) if crate::vodserve::Terminal::from_control_reason(reason).is_some() => {
            crate::vodserve::Terminal::from_control_reason(reason)
                .expect("guarded terminal control reason")
                .control_reason()
        }
        // Deserialization is filtered through `remote_abort_request_is_valid`
        // before settlement. Keep this helper total so a future internal caller
        // still cannot smuggle request-owned text into the actor.
        Some(_) => "cluster start aborted",
    }
}

pub(crate) async fn relay(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(mut request) = serde_json::from_slice::<RelayRequest>(&body)
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
    // There is no relay capability/version negotiation yet. A legacy sender
    // can supply Range without the newer If-Range proof, so the receiver must
    // independently downgrade every unversioned relay to a full response.
    // Exact authentication was checked against the original wire body above.
    request = sanitize_unversioned_relay(request);
    let now = Instant::now();
    let Some(budget) = request.owner_budget_at(unix_ms()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let request_deadline = now + budget;
    let route = match state
        .media_sessions
        .authoritative_route_resolution_before(
            &request.session_id,
            &state.node_id,
            request_deadline,
        )
        .await
    {
        Ok(DurableRouteResolution::ActiveLocal(route)) => route,
        Ok(DurableRouteResolution::Terminal(route))
            if matches!(&request.resource, RelayResource::Delete)
                && route.owner_node_id == state.node_id =>
        {
            route
        }
        Ok(DurableRouteResolution::Terminal(_)) => return StatusCode::GONE.into_response(),
        Ok(DurableRouteResolution::Absent) => return StatusCode::NOT_FOUND.into_response(),
        Ok(DurableRouteResolution::OwnerTransition(_)) => {
            return StatusCode::CONFLICT.into_response()
        }
        Ok(DurableRouteResolution::ActiveRemote(_)) => return StatusCode::CONFLICT.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if matches!(&request.resource, RelayResource::Delete) && route.state != "active" {
        state
            .media_sessions
            .cache_terminal_route(route.clone())
            .await;
    }
    if let Some(status) = post_classification_route_rejection(&request.resource, &route, unix_ms())
    {
        return status.into_response();
    }
    super::hls::relay_local(&state, request).await
}

fn sanitize_unversioned_relay(mut request: RelayRequest) -> RelayRequest {
    request.headers = request.headers.for_unversioned_peer();
    request
}

fn post_classification_route_rejection(
    resource: &RelayResource,
    route: &plurx_core::domain::MediaSessionRoute,
    now_unix_ms: i64,
) -> Option<StatusCode> {
    if matches!(resource, RelayResource::Delete) {
        return None;
    }
    if route.state != "active" {
        return Some(StatusCode::GONE);
    }
    // An active row whose lease crossed its boundary after the authoritative
    // classification is still a takeover/settlement transition, never a
    // durable terminal fact. 409 makes ingress reclassify.
    (route.lease_expires_at_ms <= now_unix_ms).then_some(StatusCode::CONFLICT)
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
    let generation = request.generation.clone();
    let owner_epoch = u64::try_from(request.expected_owner_epoch).ok();
    let Some(budget) = crate::playback_control::inherited_exchange_budget(
        request.deadline_unix_ms,
        crate::media_sessions::unix_ms(),
    ) else {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
        return super::hls::control_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "control_unavailable",
            "the relayed control exchange arrived after its ingress deadline",
            Some(generation),
            owner_epoch,
            Some(500),
            None,
        );
    };
    match tokio::time::timeout(budget, control_inner(state, headers, body, request)).await {
        Ok(response) => response,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            super::hls::control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the owner exceeded the inherited control deadline",
                Some(generation),
                owner_epoch,
                Some(500),
                None,
            )
        }
    }
}

async fn control_inner(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
    request: crate::playback_control::ControlRelayRequest,
) -> Response {
    if let Err(status) = authorize(&state, &headers, CONTROL_PATH, &body).await {
        return status.into_response();
    }
    let route = match state.store.media_session_route(&request.session_id).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return super::hls::control_error(
                StatusCode::NOT_FOUND,
                "session_gone",
                "no durable media session has this capability",
                None,
                None,
                None,
                None,
            );
        }
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return super::hls::control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable media route is temporarily unavailable",
                None,
                None,
                Some(500),
                None,
            );
        }
    };
    let owner_epoch = u64::try_from(route.owner_epoch).ok();
    if route.owner_node_id != state.node_id
        || route.owner_node_id != request.expected_owner_node_id
        || route.owner_epoch != request.expected_owner_epoch
    {
        crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerChanged);
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
        crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
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
    match super::hls::terminal_ack_replay(
        &state,
        &route,
        &request.control,
        request.deadline_unix_ms,
    )
    .await
    {
        Ok(Some(replay)) => return super::hls::terminal_ack_response(replay),
        Ok(None) => {}
        Err(()) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return super::hls::control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the terminal control acknowledgement is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                owner_epoch,
                Some(500),
                None,
            );
        }
    }
    if let Err(retry_after_ms) = state.media_sessions.admit_control(&request.session_id) {
        crate::playback_control::record(crate::playback_control::MetricOutcome::RateLimited);
        return super::hls::control_error(
            StatusCode::TOO_MANY_REQUESTS,
            "control_rate_limited",
            "the owner control budget is exhausted",
            None,
            None,
            Some(retry_after_ms),
            None,
        );
    }
    if route.state != "active" {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
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
    // The owner side of the relay. This returns before `control_local` ever
    // reaches `verify_authority`, so it has to classify rather than assume, or
    // the same binary answers one route two ways depending on which node the
    // client happened to reach. Condition and answer both live in the shared
    // function; this site owns no part of the decision.
    if let Some(refusal) = super::hls::control_owner_refusal(&route, owner_epoch) {
        return refusal;
    }
    super::hls::control_local(&state, &route, request.control, request.deadline_unix_ms).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_start_requires_current_local_serving_authority() {
        let (_app, state) = super::super::tests::test_app_with_state();
        let (_, first_generation) = admit_remote_start_serving_authority(&state)
            .unwrap_or_else(|_| panic!("initial serving authority"));

        state.serving.validation_set_ready(false).await;
        assert!(matches!(
            admit_remote_start_serving_authority(&state),
            Err(RemoteStartError::ServingFence)
        ));
        let response = RemoteStartError::ServingFence.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("1"))
        );

        state.serving.validation_set_ready(true).await;
        let (authority, recovered_generation) = admit_remote_start_serving_authority(&state)
            .unwrap_or_else(|_| panic!("recovered serving authority"));
        assert_ne!(recovered_generation, first_generation);
        assert!(authority.is_current(recovered_generation));
        assert!(!authority.is_current(first_generation));
    }

    #[tokio::test]
    async fn remote_start_pin_error_is_returned_to_the_guarded_start() {
        let result = pin_shared_session_before_deadline(
            tokio::time::Instant::now() + Duration::from_secs(1),
            async {
                Err(plurx_core::error::StoreError::Database(
                    "injected pin failure".to_owned(),
                ))
            },
        )
        .await;
        assert_eq!(
            result,
            Err(crate::transcode::start_infrastructure_error(
                "pinning the shared cache session: database error: injected pin failure"
            ))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn remote_start_pin_timeout_is_bound_to_the_start_deadline() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let pin = std::future::pending::<Result<bool, plurx_core::error::StoreError>>();
        let pending = pin_shared_session_before_deadline(deadline, pin);
        tokio::pin!(pending);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            pending.await,
            Err(crate::transcode::start_infrastructure_error(
                "shared cache pin exceeded the start deadline"
            ))
        );
    }

    #[test]
    fn remote_start_ownership_status_is_explicit_and_legacy_replays_fail_closed() {
        let mut negotiated = HeaderMap::new();
        negotiated.insert(
            REMOTE_START_OWNERSHIP_HEADER,
            HeaderValue::from_static(REMOTE_START_OWNERSHIP_V1),
        );
        assert_eq!(
            remote_start_success_status(&negotiated, true),
            StatusCode::CREATED
        );
        assert_eq!(
            remote_start_success_status(&negotiated, false),
            StatusCode::ALREADY_REPORTED
        );

        let legacy = HeaderMap::new();
        assert_eq!(remote_start_success_status(&legacy, true), StatusCode::OK);
        assert_eq!(
            remote_start_success_status(&legacy, false),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test(start_paused = true)]
    async fn remote_start_request_cancellation_drops_the_inflight_pin() {
        struct PinDropProbe(Option<std::sync::Arc<std::sync::atomic::AtomicBool>>);

        impl Drop for PinDropProbe {
            fn drop(&mut self) {
                if let Some(dropped) = self.0.take() {
                    dropped.store(true, std::sync::atomic::Ordering::Release);
                }
            }
        }

        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = PinDropProbe(Some(std::sync::Arc::clone(&dropped)));
        let task = tokio::spawn(pin_shared_session_before_deadline(
            tokio::time::Instant::now() + Duration::from_secs(5),
            async move {
                let _probe = probe;
                std::future::pending::<Result<bool, plurx_core::error::StoreError>>().await
            },
        ));
        task.abort();
        let _ = task.await;
        assert!(
            dropped.load(std::sync::atomic::Ordering::Acquire),
            "request cancellation must drop the in-flight pin operation"
        );
    }

    #[test]
    fn active_lease_expiring_after_classification_is_transition_not_gone() {
        let route = plurx_core::domain::MediaSessionRoute {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            user_id: 7,
            playback_id: "relay-expiry".to_owned(),
            request_fingerprint: "a".repeat(64),
            owner_node_id: "node-a".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: 10_000,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: 1,
        };
        assert_eq!(
            post_classification_route_rejection(&RelayResource::Status, &route, 10_000),
            Some(StatusCode::CONFLICT)
        );
        let mut ended = route;
        ended.state = "ended".to_owned();
        assert_eq!(
            post_classification_route_rejection(&RelayResource::Status, &ended, 9_999),
            Some(StatusCode::GONE)
        );
    }

    #[test]
    fn remote_exact_abort_preserves_release_reason_and_legacy_default() {
        let mut request = RemoteAbortRequest {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            expected_owner_epoch: 1,
            reason: Some("released by client".to_owned()),
        };
        assert_eq!(remote_abort_reason(&request), "released by client");
        request.reason = None;
        assert_eq!(remote_abort_reason(&request), "cluster start aborted");
    }

    #[test]
    fn legacy_range_only_relay_is_downgraded_at_the_receiving_endpoint() {
        let request: RelayRequest = serde_json::from_value(serde_json::json!({
            "session_id": uuid::Uuid::new_v4().to_string(),
            "resource": {
                "resource": "segment",
                "segment": "seg00001.m4s"
            },
            "deadline_unix_ms": unix_ms().saturating_add(1_000),
            "headers": {
                "range": "bytes=10-19"
            }
        }))
        .expect("legacy relay envelope");
        assert_eq!(request.headers.range.as_deref(), Some("bytes=10-19"));
        assert!(request.headers.if_range.is_none());

        let sanitized = sanitize_unversioned_relay(request);
        assert!(sanitized.headers.range.is_none());
        assert!(sanitized.headers.if_range.is_none());
        assert_eq!(sanitized.headers.if_none_match, None);
    }
}
