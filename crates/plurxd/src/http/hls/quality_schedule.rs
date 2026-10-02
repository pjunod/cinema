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
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let remaining = deadline_unix_ms.saturating_sub(unix_ms()).min(12_000);
    if remaining <= 0 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
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
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    });
    match task.await {
        Ok(response) => response,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn quality_schedule_routed(
    state: &AppState,
    session: &str,
    request: QualityScheduleRequest,
    expected_owner: Option<&str>,
    deadline_unix_ms: i64,
) -> Response {
    let route = match state.media_sessions.control_route(session).await {
        Ok(Some(route)) => route,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if route.incarnation_id != request.generation
        || u64::try_from(route.owner_epoch).ok() != Some(request.control_epoch)
        || expected_owner
            .is_some_and(|owner| owner != route.owner_node_id || owner != state.node_id)
    {
        return StatusCode::CONFLICT.into_response();
    }
    if route.state != "active" || route.lease_expires_at_ms <= unix_ms() {
        return StatusCode::GONE.into_response();
    }
    if let Some(refusal) = library_channel_control_refusal(state, &route).await {
        return refusal;
    }
    if let Some(refusal) = control_owner_refusal(&route, Some(request.control_epoch)) {
        return refusal;
    }
    if state.media_sessions.admit_control(session).is_err() {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
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
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    let remaining = deadline_unix_ms.saturating_sub(unix_ms()).min(12_000);
    if remaining <= 0 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
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
            tracing::debug!(session, %error, "quality schedule refused");
            StatusCode::CONFLICT.into_response()
        }
    }
}
