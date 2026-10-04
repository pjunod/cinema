//! Independently versioned schedule mutations, never legacy control JSON.
use super::*;
use crate::vodserve::QualityScheduleRequest;
pub(crate) const QUALITY_SCHEDULE_PATH: &str = "/internal/cluster/media/sessions/quality-schedule";
pub(crate) const QUALITY_SCHEDULE_MAX_BYTES: usize = 160 * 1024;
pub(crate) const QUALITY_SCHEDULE_MAX_RESPONSE_BYTES: usize =
    2 * plurx_core::playback::continuous_quality::MAX_QUALITY_LEDGER_BYTES + 8192;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityScheduleRelayRequest {
    pub session_id: String,
    pub expected_owner_node_id: String,
    pub deadline_unix_ms: i64,
    pub request: QualityScheduleRequest,
}

pub(crate) async fn quality_schedule(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    body: Bytes,
) -> Response {
    let Ok(request) = serde_json::from_slice::<QualityScheduleRequest>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    quality_schedule_admitted(
        state,
        session,
        request,
        None,
        unix_ms().saturating_add(12_000),
    )
    .await
}

pub(crate) async fn quality_schedule_admitted(
    state: AppState,
    session: String,
    request: QualityScheduleRequest,
    expected_owner: Option<String>,
    deadline_unix_ms: i64,
) -> Response {
    if !request.valid() || uuid::Uuid::parse_str(&session).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let slots = Arc::clone(SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(64))));
    let Ok(permit) = slots.try_acquire_owned() else {
        return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")]).into_response();
    };
    let remaining = deadline_unix_ms.saturating_sub(unix_ms()).min(12_000);
    if remaining <= 0 {
        return schedule_unavailable();
    }
    let budget = Duration::from_millis(remaining as u64);
    // An HTTP disconnect cannot abandon a Preparing ledger while its owner
    // still owes readiness/refusal settlement. The fixed pool bounds owners.
    let task = tokio::spawn(async move {
        let _permit = permit;
        match tokio::time::timeout(
            budget,
            quality_schedule_routed(
                &state,
                &session,
                request,
                expected_owner.as_deref(),
                deadline_unix_ms,
            ),
        )
        .await
        {
            Ok(response) => response,
            Err(_) => schedule_unavailable(),
        }
    });
    match task.await {
        Ok(response) => response,
        Err(_) => schedule_unavailable(),
    }
}

/// A transient owner/transport failure. Clients settle any 4xx except 429 as
/// a final refusal, so nothing that may succeed on a retry may answer 4xx.
fn schedule_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::RETRY_AFTER, "1"),
            (header::CACHE_CONTROL, "no-store"),
        ],
    )
        .into_response()
}

/// What an owner-side schedule failure means to the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualityScheduleFailure {
    /// The request itself is malformed (400).
    Invalid,
    /// The request's premise no longer holds; retrying the same request
    /// cannot succeed and the client settles it by one ledger read (409).
    Refused,
    /// The parent ended underneath the request (410).
    Ended,
    /// Store, deadline, reattach or revision contention: the same request may
    /// succeed shortly (503 + Retry-After).
    Transient,
}

/// Exact refusal messages produced by `VodServer::quality_schedule_before`
/// and the controlled-admission helpers it calls. That API reports
/// `Result<_, String>`; until it returns a typed error, this table is the one
/// place the HTTP layer decides which of its failures are final. Anything not
/// named here — Store errors, deadlines, "not attached" during reattach,
/// repeated revision races — is transient, because answering a transient
/// failure with a 4xx silently cancels the viewer's change.
pub(crate) const QUALITY_SCHEDULE_REFUSALS: &[&str] = &[
    "quality family changed",
    "quality target is outside its family",
    "quality attachment changed",
    "quality owner epoch changed",
    "quality facts have no attached ledger",
    "quality preparation owner changed",
    "quality window owner changed",
    "quality window target is no longer wanted",
    "quality window transaction disappeared",
    "quality response owner changed",
    "quality ledger parent attachment changed",
    "controlled attachment changed during admission",
    "controlled parent changed during admission",
    "controlled source changed during admission",
    "controlled settlement owner changed",
    "controlled target is outside its parent",
];

pub(crate) fn classify_quality_schedule_error(error: &str) -> QualityScheduleFailure {
    if error == "invalid quality schedule request" {
        QualityScheduleFailure::Invalid
    } else if error == "controlled parent ended" {
        QualityScheduleFailure::Ended
    } else if QUALITY_SCHEDULE_REFUSALS.contains(&error)
        // `QualityTransitionError`'s Display: the ledger state machine
        // refused the transition (stale sequence, conflicting replay, bound).
        || error.starts_with("continuous quality transition: ")
    {
        QualityScheduleFailure::Refused
    } else {
        QualityScheduleFailure::Transient
    }
}

impl QualityScheduleFailure {
    pub(crate) fn response(self) -> Response {
        match self {
            Self::Invalid => StatusCode::BAD_REQUEST.into_response(),
            Self::Refused => StatusCode::CONFLICT.into_response(),
            Self::Ended => StatusCode::GONE.into_response(),
            Self::Transient => schedule_unavailable(),
        }
    }
}

async fn quality_schedule_routed(
    state: &AppState,
    session: &str,
    request: QualityScheduleRequest,
    expected_owner: Option<&str>,
    deadline_unix_ms: i64,
) -> Response {
    // The session id is the only credential. Resolve it before charging the
    // node-wide budget, or a spray of random UUIDs exhausts that budget and
    // 429s every real session. The lookup is shard-single-flighted and
    // deadline-bounded, which is what quality-control already relies on.
    let route = match state.media_sessions.control_route(session).await {
        Ok(Some(route)) => route,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return schedule_unavailable(),
    };
    if state
        .media_sessions
        .admit_quality_schedule(session)
        .is_err()
    {
        return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")]).into_response();
    }
    if route.incarnation_id != request.generation
        || u64::try_from(route.owner_epoch).ok() != Some(request.control_epoch)
        || expected_owner
            .is_some_and(|owner| owner != route.owner_node_id || owner != state.node_id)
    {
        return StatusCode::CONFLICT.into_response();
    }
    let terminal = route.state == "ended";
    if terminal {
        use plurx_core::playback::continuous_quality::QualityOperation;
        if request.window.is_some()
            || request.frontier.is_some()
            || request.transition.as_ref().is_some_and(|transition| {
                matches!(
                    transition.operation,
                    QualityOperation::Prepare { .. } | QualityOperation::Scheduled { .. }
                )
            })
        {
            return StatusCode::GONE.into_response();
        }
    } else {
        if route.state != "active" || route.lease_expires_at_ms <= unix_ms() {
            return StatusCode::GONE.into_response();
        }
        if let Some(refusal) = library_channel_control_refusal(state, &route).await {
            return refusal;
        }
        if let Some(refusal) = control_owner_refusal(&route, Some(request.control_epoch)) {
            return refusal;
        }
    }
    if terminal {
        // Ended-session reconciliation is a Store compare-and-set against the
        // durable ledger; it needs no owner process. Answer it on whichever
        // node received it, so a departed owner cannot strand reconciliation.
        return terminal_quality_schedule(state, &route, &request).await;
    }
    if route.owner_node_id != state.node_id {
        let relay = QualityScheduleRelayRequest {
            session_id: session.into(),
            expected_owner_node_id: route.owner_node_id.clone(),
            deadline_unix_ms,
            request,
        };
        return match state
            .media_sessions
            .quality_schedule(&route.owner_node_id, &relay)
            .await
        {
            Ok(response) => response,
            Err(_) => schedule_unavailable(),
        };
    }
    let remaining = deadline_unix_ms.saturating_sub(unix_ms()).min(12_000);
    if remaining <= 0 {
        return schedule_unavailable();
    }
    let deadline = Instant::now() + Duration::from_millis(remaining as u64);
    match state
        .transcode
        .vod_quality_schedule_before(session, &state.node_id, &request, deadline)
        .await
    {
        Ok(reply) => {
            let mut response = Json(reply).into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(error) => {
            let failure = classify_quality_schedule_error(&error);
            tracing::debug!(session, %error, ?failure, "quality schedule refused");
            failure.response()
        }
    }
}

async fn terminal_quality_schedule(
    state: &AppState,
    route: &MediaSessionRoute,
    request: &QualityScheduleRequest,
) -> Response {
    let Some(transition) = &request.transition else {
        let snapshot = match state.store.quality_ledger(&request.generation).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return StatusCode::GONE.into_response(),
            Err(_) => return schedule_unavailable(),
        };
        if snapshot.owner_node_id != route.owner_node_id
            || snapshot.owner_epoch != route.owner_epoch
        {
            return StatusCode::CONFLICT.into_response();
        }
        let reply = crate::vodserve::QualityScheduleResponse {
            version: 1,
            generation: request.generation.clone(),
            control_epoch: request.control_epoch,
            attachment: request.attachment.clone(),
            revision: snapshot.revision,
            terminal: true,
            receipt: None,
            ledger: snapshot.ledger,
        };
        if !reply.valid_for(request) {
            return StatusCode::CONFLICT.into_response();
        }
        let mut response = Json(reply).into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        return response;
    };
    for _ in 0..4 {
        let expected = match state.store.quality_ledger(&request.generation).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return StatusCode::GONE.into_response(),
            Err(_) => return schedule_unavailable(),
        };
        let receipt = match state
            .store
            .write_terminal_quality_transition(
                &expected,
                transition,
                &route.owner_node_id,
                unix_ms(),
            )
            .await
        {
            Ok(Some(receipt)) => receipt,
            Ok(None) => continue,
            Err(_) => return StatusCode::CONFLICT.into_response(),
        };
        let snapshot = match state.store.quality_ledger(&request.generation).await {
            Ok(Some(snapshot)) => snapshot,
            _ => return schedule_unavailable(),
        };
        let reply = crate::vodserve::QualityScheduleResponse {
            version: 1,
            generation: request.generation.clone(),
            control_epoch: request.control_epoch,
            attachment: request.attachment.clone(),
            revision: snapshot.revision,
            terminal: true,
            receipt: Some(receipt),
            ledger: snapshot.ledger,
        };
        if !reply.valid_for(request) {
            return StatusCode::CONFLICT.into_response();
        }
        let mut response = Json(reply).into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        return response;
    }
    StatusCode::CONFLICT.into_response()
}
