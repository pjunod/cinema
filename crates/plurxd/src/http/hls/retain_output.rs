//! The viewer's mid-playback "stop retaining this playback's output" switch.
//!
//! Off only. A create can already opt out (`CreateOverrides::retain_output`);
//! this turns it off for a session that is already playing. The session id
//! is the capability, exactly as for `quality-schedule`. The owner abandons
//! the rolling producer's retained collection and cancels the complete-output
//! preparation this session's own start queued
//! ([`crate::transcode::TranscodeManager::stop_output_retention`]).
use super::*;

pub(crate) const RETAIN_OUTPUT_PATH: &str = "/internal/cluster/media/sessions/retain-output";
pub(crate) const RETAIN_OUTPUT_MAX_BYTES: usize = 1024;

/// The public body: `{"retain_output": false}`. `true` is refused, because
/// turning retention back on mid-playback is not offered.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainOutputRequest {
    pub retain_output: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainOutputRelayRequest {
    pub session_id: String,
    pub expected_owner_node_id: String,
}

/// `POST /api/v1/hls/{session}/retain-output`.
pub(crate) async fn retain_output(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    body: Bytes,
) -> Response {
    let Ok(request) = serde_json::from_slice::<RetainOutputRequest>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if request.retain_output || uuid::Uuid::parse_str(&session).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    retain_output_routed(&state, &session, None).await
}

/// The owner side of the relay; authenticated by the internal router.
pub(crate) async fn retain_output_relayed(
    state: &AppState,
    request: RetainOutputRelayRequest,
) -> Response {
    if request.expected_owner_node_id != state.node_id
        || uuid::Uuid::parse_str(&request.session_id).is_err()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    retain_output_routed(
        state,
        &request.session_id,
        Some(&request.expected_owner_node_id),
    )
    .await
}

async fn retain_output_routed(
    state: &AppState,
    session: &str,
    expected_owner: Option<&str>,
) -> Response {
    let route = match state.media_sessions.control_route(session).await {
        Ok(Some(route)) => route,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return retain_output_unavailable(),
    };
    if route.state != "active" || route.lease_expires_at_ms <= unix_ms() {
        return StatusCode::GONE.into_response();
    }
    if expected_owner.is_some_and(|owner| owner != route.owner_node_id) {
        return StatusCode::CONFLICT.into_response();
    }
    if route.owner_node_id != state.node_id {
        let relay = RetainOutputRelayRequest {
            session_id: session.to_owned(),
            expected_owner_node_id: route.owner_node_id.clone(),
        };
        return match state
            .media_sessions
            .retain_output(&route.owner_node_id, &relay)
            .await
        {
            Ok(response) => response,
            Err(_) => retain_output_unavailable(),
        };
    }
    state.transcode.stop_output_retention(session).await;
    (
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    )
        .into_response()
}

fn retain_output_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::RETRY_AFTER, "1"),
            (header::CACHE_CONTROL, "no-store"),
        ],
    )
        .into_response()
}
