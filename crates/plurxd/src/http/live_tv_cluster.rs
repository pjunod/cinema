//! Placement coordination stays on the configured tuner owner. Recovery and
//! ambiguous retries follow its fixed assignment, never a fresh load ranking.
use std::time::Duration;

use super::peer_transport::{deadline_after, PeerAuthMode};
use crate::live_tv::{cluster::*, *};
use crate::state::AppState;

fn unavailable() -> LiveTvError {
    LiveTvError::OwnerUnavailable(
        "the assigned Live TV processor did not return a verified answer".into(),
    )
}
async fn exchange<T: serde::de::DeserializeOwned>(
    state: &AppState,
    worker: &str,
    path: &str,
    body: Vec<u8>,
    deadline: tokio::time::Instant,
) -> Result<T, LiveTvError> {
    let (node, base) = super::live_tv::owner_peer(state, worker)
        .await
        .map_err(|_| unavailable())?;
    let response = state
        .live_tv_peers
        .request(
            &node,
            &base,
            reqwest::Method::POST,
            path,
            body,
            deadline,
            32 * 1024,
            PeerAuthMode::ExactRequestAndResponse,
        )
        .await
        .map_err(|_| unavailable())?;
    if !response.status.is_success() {
        let value = serde_json::from_slice::<serde_json::Value>(&response.body)
            .map_err(|_| unavailable())?;
        let code = value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let message = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .filter(|m| m.len() <= 2048)
            .unwrap_or("the assigned Live TV processor refused this request")
            .to_owned();
        return Err(match code {
            "tuner_capacity" => LiveTvError::Capacity(message),
            "encoder_capacity" => LiveTvError::EncoderCapacity(message),
            "codec_unsupported" => LiveTvError::CodecUnsupported(message),
            "settings_conflict" => LiveTvError::Conflict(message),
            "stream_failed" => LiveTvError::StreamFailed(message),
            "startup_timeout" => LiveTvError::StartupTimeout(message),
            "capability_expired" => LiveTvError::CapabilityExpired(message),
            _ => unavailable(),
        });
    }
    serde_json::from_slice(&response.body).map_err(|_| unavailable())
}
fn encode(value: &impl serde::Serialize) -> Result<Vec<u8>, LiveTvError> {
    serde_json::to_vec(value)
        .map_err(|_| LiveTvError::InvalidResponse("invalid Live TV peer request".into()))
}

pub(super) async fn start(
    state: &AppState,
    request: LiveTvStartRequest,
) -> Result<LiveTvProvisional, LiveTvError> {
    validate_start_request(&request)?;
    if let Some(playback) = &request.playback {
        playback
            .validate()
            .map_err(|_| LiveTvError::InvalidConfig("invalid playback capabilities".into()))?;
    }
    let config = state.live_tv.config().await?;
    if config.owner_node_id != state.node_id
        || request.expected_owner_node_id != state.node_id
        || config.generation != request.config_generation
        || !config.enabled
        || !config.admission_ready()
    {
        return Err(LiveTvError::Conflict(
            "the tuner owner or configuration changed".into(),
        ));
    }
    // A bounded admission-time sweep, with a common deadline. Unreachable
    // assignments remain owned; exhaustion refuses new starts instead of
    // silently evicting a session whose admission is ambiguous.
    let due = state.live_tv.placements().due();
    let sweep_deadline = deadline_after(Duration::from_millis(200));
    for entry in due {
        if entry.retiring {
            let _ = tokio::time::timeout_at(
                sweep_deadline,
                retire(state, entry.request.user_id, &entry.request.request_id),
            )
            .await;
            if tokio::time::Instant::now() >= sweep_deadline {
                break;
            }
            continue;
        }

        let answer = tokio::time::timeout_at(
            sweep_deadline,
            start_state(state, entry.request.user_id, &entry.request.request_id),
        )
        .await;
        if let Ok(Ok(answer)) = answer {
            if !matches!(answer.state.as_str(), "active" | "starting") {
                let _ = tokio::time::timeout_at(
                    sweep_deadline,
                    retire(state, entry.request.user_id, &entry.request.request_id),
                )
                .await;
            }
        }
        if tokio::time::Instant::now() >= sweep_deadline {
            break;
        }
    }
    let existing = state
        .live_tv
        .placements()
        .get(request.user_id, &request.request_id);
    let worker = match existing {
        Some(held) => held.worker,
        None if state
            .live_tv
            .start_state_local(request.user_id, &request.request_id)
            .state
            != "unknown" =>
        {
            state.node_id.clone()
        }
        None => state.media_pool.live_tv_worker(state).await,
    };
    let entry = state.live_tv.placements().assign(&request, worker)?;
    if entry.worker == state.node_id {
        return state.live_tv.start_local(request).await;
    }
    let wire = ProcessingStart {
        start: PlacedStart::from_request(&request),
        worker: entry.worker.clone(),
        nonce: entry.nonce,
    };
    let response: LiveTvProvisional = exchange(
        state,
        &entry.worker,
        PROCESS_PATH,
        encode(&wire)?,
        deadline_after(Duration::from_secs(32)),
    )
    .await?;
    if capability_owner(&response.capability).ok().as_deref() != Some(entry.worker.as_str())
        || response.session_id != response.capability
        || response.config_generation != request.config_generation
        || response.channel.id != request.channel_id
    {
        return Err(unavailable());
    }
    Ok(response)
}

pub(super) async fn process(
    state: &AppState,
    wire: ProcessingStart,
) -> Result<LiveTvProvisional, LiveTvError> {
    if wire.worker != state.node_id {
        return Err(unavailable());
    }
    let request = wire.start.clone().into_request();
    let config = state.live_tv.config().await?;
    if config.owner_node_id != request.expected_owner_node_id
        || config.owner_node_id == state.node_id
        || config.generation != request.config_generation
    {
        return Err(unavailable());
    }
    if let Some(answer) = state.live_tv.recover_processing(&request).await? {
        return Ok(answer);
    }
    let deadline = deadline_after(Duration::from_secs(30));
    let snapshot = super::live_tv::owner_snapshot_within(state, &config, true, false, deadline)
        .await
        .map_err(|_| unavailable())?;
    let (owner, base) = super::live_tv::owner_peer(state, &config.owner_node_id)
        .await
        .map_err(|_| unavailable())?;
    let response = state
        .live_tv_peers
        .request_stream(
            &owner,
            &base,
            reqwest::Method::POST,
            INGEST_PATH,
            encode(&wire)?,
            deadline,
            PeerAuthMode::ExactRequest,
        )
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    tokio::time::timeout_at(
        deadline,
        state
            .live_tv
            .start_with_ingest(request, Some((snapshot, response))),
    )
    .await
    .map_err(|_| {
        LiveTvError::StartupTimeout("the remote Live TV startup deadline expired".into())
    })?
}

pub(super) async fn retire(
    state: &AppState,
    user: i64,
    request: &str,
) -> Result<LiveTvRetireOutcome, LiveTvError> {
    let held = state.live_tv.placements().begin_retire(user, request)?;
    let result = match held.filter(|e| e.worker != state.node_id) {
        Some(entry) => {
            exchange(
                state,
                &entry.worker,
                RETIRE_PATH,
                encode(&LiveTvRetireRequest {
                    expected_owner_node_id: entry.worker.clone(),
                    user_id: user,
                    request_id: request.into(),
                })?,
                deadline_after(Duration::from_secs(5)),
            )
            .await?
        }
        None => state.live_tv.retire_local(user, request).await,
    };
    state.live_tv.placements().finish(user, request);
    // Also tombstone the coordinator: an old ingress using the legacy start
    // path must not recreate an id retired through the placed protocol.
    let _ = state.live_tv.retire_local(user, request).await;
    Ok(result)
}
pub(super) async fn resume(
    state: &AppState,
    user: i64,
    request: &str,
) -> Result<LiveTvResumeAnswer, LiveTvError> {
    let held = state.live_tv.placements().get(user, request);
    match held.filter(|e| e.worker != state.node_id) {
        Some(entry) => {
            exchange(
                state,
                &entry.worker,
                RESUME_PATH,
                encode(&LiveTvResumeRequest {
                    expected_owner_node_id: entry.worker.clone(),
                    user_id: user,
                    request_id: request.into(),
                })?,
                deadline_after(Duration::from_secs(5)),
            )
            .await
        }
        None => Ok(state.live_tv.resume_local(user, request).await),
    }
}
pub(super) async fn start_state(
    state: &AppState,
    user: i64,
    request: &str,
) -> Result<LiveTvStartState, LiveTvError> {
    let held = state.live_tv.placements().get(user, request);
    match held.filter(|e| e.worker != state.node_id) {
        Some(entry) => {
            exchange(
                state,
                &entry.worker,
                START_STATE_PATH,
                encode(&LiveTvStartStateRequest {
                    expected_owner_node_id: entry.worker.clone(),
                    user_id: user,
                    request_id: request.into(),
                })?,
                deadline_after(Duration::from_secs(5)),
            )
            .await
        }
        None => Ok(state.live_tv.start_state_local(user, request)),
    }
}
