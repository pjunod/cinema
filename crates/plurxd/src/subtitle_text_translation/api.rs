//! Closed, authenticated DTOs for translating existing text subtitles.
use super::{artifacts, config, ADAPTER_CONTRACT, CATALOG_REVISION, MODEL_ID};
use crate::{
    http::{
        error::ApiError,
        extract::{AdminUser, AuthUser},
    },
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Json,
};
use plurx_core::store::{
    background_jobs::{
        BackgroundJob, EnqueueJob, EnqueueOutcome, JobPayload, JobRequest, JobState,
    },
    background_jobs_transcription::{artifact_key, ModelAction},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelRequest {
    version: u8,
    request_id: String,
    node_id: String,
    boot_id: String,
    model_id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    version: u8,
    request_id: String,
    source_size: String,
    source_mtime: String,
    source_language: String,
    target_language: String,
    model_id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetryRequest {
    version: u8,
    request_id: String,
}
fn conflict(code: &'static str, message: &str) -> ApiError {
    ApiError::typed(axum::http::StatusCode::CONFLICT, code, message)
}
fn invalid(code: &'static str, message: &str) -> ApiError {
    ApiError::typed(axum::http::StatusCode::BAD_REQUEST, code, message)
}
fn request(version: u8, id: &str) -> Result<(), ApiError> {
    if version != 1 || uuid::Uuid::parse_str(id).is_err() {
        return Err(ApiError::BadRequest(
            "Invalid text translation request identity".into(),
        ));
    }
    Ok(())
}
pub(crate) async fn settings(
    _user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<config::Snapshot>, ApiError> {
    Ok(Json(config::read(&state).await?))
}
pub(crate) async fn update_settings(
    _user: AdminUser,
    State(state): State<AppState>,
    Json(input): Json<config::UpdateRequest>,
) -> Result<Json<config::Snapshot>, ApiError> {
    Ok(Json(config::save(&state, input).await?))
}
pub(crate) async fn models(
    _user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"version":1,"catalog":artifacts::catalog_public(),"workers":super::workers(&state).await?,"server_now_ms":crate::state::clock_ms()}),
    ))
}
pub(crate) async fn options(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    Ok(Json(
        json!({"version":1,"file_id":file.id.to_string(),"source_size":file.size.to_string(),"source_mtime":file.mtime.to_string(),"subtitles":file.subtitle_streams.iter().map(|track|json!({"index":track.index,"language":track.language,"title":track.title,"downloaded_subtitle_id":file.downloaded_subtitle(track.index).map(|caption|caption.stable_id()),"text_eligible":!plurx_core::tracks::is_bitmap_subtitle(&track.codec)})).collect::<Vec<_>>(),"configuration":config::read(&state).await?,"model_id":MODEL_ID,"input_languages":artifacts::catalog_public()["input_languages"],"workers":super::workers(&state).await?,"permissions":{"read":true,"create":user.is_admin},"server_now_ms":crate::state::clock_ms()}),
    ))
}
async fn model_action(
    state: &AppState,
    user_id: i64,
    input: ModelRequest,
    action: ModelAction,
) -> Result<Json<Value>, ApiError> {
    request(input.version, &input.request_id)?;
    let digest =
        plurx_core::store::subtitle_translation::digest(&json!({"action":action,"body":input}))?;
    if let Some(reply) = replay(state, user_id, &input.request_id, &digest).await? {
        return Ok(Json(reply));
    }
    if input.model_id != MODEL_ID
        || input.node_id != state.node_id
        || input.boot_id != state.subtitle_models.boot_id
    {
        return Err(conflict(
            "worker_changed",
            "Refresh the current worker before model setup",
        ));
    }

    let payload = JobPayload::SubtitleTextModelOperation {
        action,
        model_id: MODEL_ID.into(),
        catalog_revision: CATALOG_REVISION.into(),
        model_sha256: artifacts::expected_bundle_digest(),
        adapter_contract: ADAPTER_CONTRACT.into(),
        target_node_id: state.node_id.clone(),
        target_boot_id: state.subtitle_models.boot_id.clone(),
        admin_user_id: user_id,
    };
    enqueue(state, user_id, &input.request_id, digest, payload).await
}
pub(crate) async fn install(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Json(input): Json<ModelRequest>,
) -> Result<Json<Value>, ApiError> {
    model_action(&state, user.id, input, ModelAction::Install).await
}
pub(crate) async fn verify(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Json(input): Json<ModelRequest>,
) -> Result<Json<Value>, ApiError> {
    model_action(&state, user.id, input, ModelAction::Verify).await
}
pub(crate) async fn create(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Path((file_id, index)): Path<(i64, i64)>,
    Json(input): Json<CreateRequest>,
) -> Result<Json<Value>, ApiError> {
    request(input.version, &input.request_id)?;
    let digest = plurx_core::store::subtitle_translation::digest(
        &json!({"file_id":file_id,"subtitle_index":index,"body":input}),
    )?;
    if let Some(reply) = replay(&state, user.id, &input.request_id, &digest).await? {
        return Ok(Json(reply));
    }
    if input.model_id != MODEL_ID || input.target_language != "en" {
        return Err(ApiError::BadRequest(
            "Choose English text translation with the supported model".into(),
        ));
    }
    if !config::read(&state).await?.enabled {
        return Err(conflict(
            "disabled",
            "Enable text subtitle translation in Settings",
        ));
    }
    let language = super::normalize_language(&input.source_language)?;
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    if file.downloaded_subtitles.len() >= plurx_core::store::MAX_DOWNLOADED_SUBTITLES {
        return Err(conflict(
            "quota_exceeded",
            "This source already has the maximum number of acquired or generated captions",
        ));
    }
    if input.source_size != file.size.to_string() || input.source_mtime != file.mtime.to_string() {
        return Err(conflict("source_changed", "The media source changed"));
    }
    if !super::workers(&state)
        .await?
        .iter()
        .any(|worker| worker["status"] == "verified")
    {
        return Err(conflict(
            "translation_unverified",
            "Install and test the text translation model in Settings",
        ));
    }
    let bytes = super::input_bytes(&state, &file, index)
        .await
        .map_err(|_| {
            invalid(
                "subtitle_unavailable",
                "A complete text subtitle is required",
            )
        })?;

    let payload = JobPayload::SubtitleTextTranslate {
        file_id,
        source_size: file.size,
        source_mtime: file.mtime,
        subtitle_index: index,
        input_sha256: hex::encode(Sha256::digest(bytes)),
        engine_sha256: super::engine_identity()
            .await
            .map_err(|_| ApiError::ServiceUnavailable("Text engine identity unavailable".into()))?
            .0,
        source_language: language,
        target_language: "en".into(),
        model_id: MODEL_ID.into(),
        model_sha256: artifacts::expected_bundle_digest(),
        adapter_contract: ADAPTER_CONTRACT.into(),
        target_node_id: state.node_id.clone(),
        admin_user_id: user.id,
    };
    enqueue(&state, user.id, &input.request_id, digest, payload).await
}
pub(crate) async fn retry(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(input): Json<RetryRequest>,
) -> Result<Json<Value>, ApiError> {
    request(input.version, &input.request_id)?;
    let digest = plurx_core::store::subtitle_translation::digest(
        &json!({"retry_job_id":job_id,"body":input}),
    )?;
    if let Some(reply) = replay(&state, user.id, &input.request_id, &digest).await? {
        return Ok(Json(reply));
    }
    let job = state
        .store
        .background_job(&job_id)
        .await?
        .ok_or(ApiError::NotFound("text translation job"))?;
    if !matches!(job.state, JobState::Failed | JobState::Cancelled) {
        return Err(ApiError::Conflict(
            "Refresh the completed attempt before retrying".into(),
        ));
    }
    let mut payload = job.supported_payload()?;
    match &mut payload {
        JobPayload::SubtitleTextTranslate {
            admin_user_id,
            target_node_id,
            ..
        } => {
            *admin_user_id = user.id;
            *target_node_id = state.node_id.clone();
        }
        JobPayload::SubtitleTextModelOperation {
            admin_user_id,
            target_node_id,
            target_boot_id,
            ..
        } => {
            *admin_user_id = user.id;
            *target_node_id = state.node_id.clone();
            *target_boot_id = state.subtitle_models.boot_id.clone();
        }
        _ => return Err(ApiError::NotFound("text translation job")),
    }

    enqueue(&state, user.id, &input.request_id, digest, payload).await
}
async fn replay(
    state: &AppState,
    user_id: i64,
    request_id: &str,
    digest: &str,
) -> Result<Option<Value>, ApiError> {
    let Some(receipt) = state
        .store
        .text_translation_request_receipt(user_id, request_id, digest)
        .await?
    else {
        return Ok(None);
    };
    if receipt["outcome"] == "conflict" {
        return Err(conflict(
            "request_conflict",
            "This request identity belongs to another action",
        ));
    }
    let id = receipt["job_id"]
        .as_str()
        .ok_or_else(|| ApiError::Internal("Text request receipt is invalid".into()))?;
    let job = state
        .store
        .background_job(id)
        .await?
        .ok_or(ApiError::NotFound("text translation job"))?;
    Ok(Some(json!({"version":1,"job_id":id,"state":job.state})))
}

async fn enqueue(
    state: &AppState,
    user_id: i64,
    request_id: &str,
    digest: String,
    payload: JobPayload,
) -> Result<Json<Value>, ApiError> {
    let key = plurx_core::store::subtitle_translation::digest(&payload)?;
    let now = crate::state::clock_ms();
    let outcome = state
        .store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            dedupe_key: format!("subtitle-text:{key}"),
            priority: 1,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: format!("subtitle-text-user:{user_id}"),
                request_id: request_id.into(),
                request_digest: digest,
                consumer_kind: "subtitle_text_translation".into(),
                consumer_ref: key,
                target_node_id: Some(state.node_id.clone()),
                deadline_ms: Some(now + 86_400_000),
                retain_identity: false,
            },
        })
        .await?;
    let id = match outcome {
        EnqueueOutcome::Accepted { job_id, .. } | EnqueueOutcome::Existing { job_id, .. } => job_id,
        EnqueueOutcome::Conflict => {
            return Err(conflict(
                "request_conflict",
                "This request identity was already used for a different action",
            ))
        }
        EnqueueOutcome::QueueFull => {
            return Err(ApiError::ServiceUnavailable(
                "Text translation queue is full".into(),
            ))
        }
        _ => {
            return Err(conflict(
                "state_conflict",
                "Text translation is not currently admissible; refresh its status",
            ))
        }
    };
    let job = state
        .store
        .background_job(&id)
        .await?
        .ok_or(ApiError::NotFound("text translation job"))?;
    state.subtitle_models.translation.wake.notify_one();
    Ok(Json(json!({"version":1,"job_id":id,"state":job.state})))
}
pub(crate) async fn job(
    _user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let job = state
        .store
        .background_job(&id)
        .await?
        .ok_or(ApiError::NotFound("text translation job"))?;
    // Same local authenticated catalogue scope as ordinary /files/{id}/subs reads.
    // No remote source references or library credentials enter this route.
    let payload = job.supported_payload()?;
    if !matches!(
        payload,
        JobPayload::SubtitleTextTranslate { .. } | JobPayload::SubtitleTextModelOperation { .. }
    ) {
        return Err(ApiError::NotFound("text translation job"));
    }
    projection(&state, &job, &payload).await.map(Json)
}
async fn projection(
    state: &AppState,
    job: &BackgroundJob,
    payload: &JobPayload,
) -> Result<Value, ApiError> {
    let mut result = Value::Null;
    if job.state == JobState::Succeeded {
        if let JobPayload::SubtitleTextTranslate { file_id, .. } = payload {
            if let Some(file) = state.store.get_file(*file_id).await? {
                let key = artifact_key(payload)?;
                for track in &file.subtitle_streams {
                    if file
                        .downloaded_subtitle(track.index)
                        .and_then(|caption| caption.text_translation.as_ref())
                        .is_some_and(|origin| origin.artifact_key == key)
                    {
                        result = json!({"file_id":file_id.to_string(),"subtitle_index":track.index,"downloaded_subtitle_id":key,"language":"en"});
                        break;
                    }
                }
            }
        }
    }
    let checkpoint = job
        .checkpoint
        .as_ref()
        .filter(|checkpoint| checkpoint["fence"].as_i64() == Some(job.fence));
    let (phase, reason) = status_phase(
        job.state,
        matches!(payload, JobPayload::SubtitleTextTranslate { .. }),
        checkpoint,
        job.fence,
        state.transcode.speech_capacity_observation().await.2,
    );
    Ok(
        json!({"version":1,"job_id":job.id,"state":job.state,"phase":phase,"reason":reason.or_else(||job.last_error_code.as_deref().map(super::manager::safe_reason)),"retryable":matches!(job.state,JobState::Failed|JobState::Cancelled),"progress":{"completed":checkpoint.and_then(|c|c["completed_cues"].as_u64()).unwrap_or(0),"total":checkpoint.and_then(|c|c["total_cues"].as_u64()).unwrap_or(0)},"result":result}),
    )
}

fn status_phase(
    state: JobState,
    is_text_translate: bool,
    checkpoint: Option<&Value>,
    fence: i64,
    capacity_available: bool,
) -> (Option<&str>, Option<&'static str>) {
    let checkpoint = checkpoint.filter(|checkpoint| checkpoint["fence"].as_i64() == Some(fence));
    let phase = checkpoint.and_then(|checkpoint| checkpoint["phase"].as_str());
    let waiting = state == JobState::Queued && is_text_translate && !capacity_available;
    if waiting {
        (
            Some("waiting_for_capacity"),
            Some("waiting_for_playback_capacity"),
        )
    } else {
        (phase, None)
    }
}

#[cfg(test)]
mod status_phase_tests {
    use super::status_phase;
    use plurx_core::store::background_jobs::JobState;
    use serde_json::json;

    #[test]
    fn fresh_queued_translation_reports_capacity_wait() {
        assert_eq!(
            status_phase(JobState::Queued, true, None, 1, false),
            (
                Some("waiting_for_capacity"),
                Some("waiting_for_playback_capacity")
            )
        );
    }

    #[test]
    fn queued_model_operation_keeps_its_download_phase() {
        let checkpoint = json!({"fence": 1, "phase": "downloading"});
        assert_eq!(
            status_phase(JobState::Queued, false, Some(&checkpoint), 1, false),
            (Some("downloading"), None)
        );
    }

    #[test]
    fn running_model_operation_preserves_capacity_phase() {
        let checkpoint = json!({"fence": 1, "phase": "waiting_for_capacity"});
        assert_eq!(
            status_phase(JobState::Running, false, Some(&checkpoint), 1, true),
            (Some("waiting_for_capacity"), None)
        );
    }

    #[test]
    fn stale_checkpoint_is_ignored() {
        let checkpoint = json!({"fence": 0, "phase": "downloading"});
        assert_eq!(
            status_phase(JobState::Queued, false, Some(&checkpoint), 1, false),
            (None, None)
        );
    }
}
