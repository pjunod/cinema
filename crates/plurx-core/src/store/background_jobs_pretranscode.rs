//! Domain projection for speculative producers. Execution ownership stays in
//! the common queue; these fields describe media and policy only.
use super::background_jobs::{BackgroundJob, EnqueueJob, JobPayload, JobRequest, JobState};
use crate::domain::{NewPretranscodeJob, PretranscodeJob};
use crate::error::StoreError;
use sha2::{Digest, Sha256};

pub fn enqueue_request(job: &NewPretranscodeJob) -> Result<EnqueueJob, StoreError> {
    let requirements = serde_json::from_str(&job.requirements_json)
        .map_err(|error| StoreError::Task(format!("invalid transcode requirements: {error}")))?;
    let payload = JobPayload::TranscodePrepare {
        file_id: job.file_id,
        source_generation: format!("{}:{}:{}", job.file_id, job.source_size, job.source_mtime),
        source_size: job.source_size,
        source_mtime: job.source_mtime,
        recipe_key: job.dedupe_key.clone(),
        target_height: u32::try_from(job.target_height)
            .map_err(|_| StoreError::Task("invalid target height".into()))?,
        policy_generation: job.policy_generation.clone(),
        requirements,
        reason: job.reason.clone(),
    };
    let mut identity =
        serde_json::to_value(&payload).map_err(|error| StoreError::Task(error.to_string()))?;
    identity
        .as_object_mut()
        .expect("tagged payload object")
        .remove("reason");
    let request_digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&identity).map_err(|error| StoreError::Task(error.to_string()))?,
    ));
    let request = EnqueueJob {
        id: job.id.clone(),
        payload,
        dedupe_key: job.dedupe_key.clone(),
        priority: u8::from(job.reason == "next_up" || job.reason == "in_progress"),
        not_before_ms: job.not_before_ms,
        now_ms: job.created_at_ms,
        request: JobRequest {
            scope: "automatic:transcode".into(),
            request_id: job.dedupe_key.clone(),
            request_digest,
            consumer_kind: "transcode_prepare".into(),
            consumer_ref: job.file_id.to_string(),
            target_node_id: None,
            deadline_ms: Some(job.created_at_ms.saturating_add(86_400_000)),
            retain_identity: false,
        },
    };
    request.validate()?;
    Ok(request)
}

pub fn projection(job: &BackgroundJob) -> Result<PretranscodeJob, StoreError> {
    let JobPayload::TranscodePrepare {
        file_id,
        source_size,
        source_mtime,
        target_height,
        policy_generation,
        requirements,
        reason,
        ..
    } = &job.supported_payload()?
    else {
        return Err(StoreError::Task(
            "job is not a transcode preparation".into(),
        ));
    };
    Ok(PretranscodeJob {
        id: job.id.clone(),
        dedupe_key: job.dedupe_key.clone(),
        file_id: *file_id,
        source_size: *source_size,
        source_mtime: *source_mtime,
        target_height: i64::from(*target_height),
        policy_generation: policy_generation.clone(),
        requirements_json: serde_json::to_string(requirements)
            .map_err(|error| StoreError::Task(error.to_string()))?,
        reason: reason.clone(),
        priority: i64::from(job.priority),
        state: match job.state {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Cancelling => "cancelling",
            JobState::Succeeded => "ready",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
        .into(),
        owner_node_id: job
            .token
            .as_ref()
            .map(|token| token.node_id.clone())
            .unwrap_or_default(),
        fence: job.fence,
        lease_expires_ms: job.token.as_ref().map_or(0, |token| token.lease_expires_ms),
        attempts: job.failed_attempts,
        not_before_ms: job.not_before_ms,
        created_at_ms: job.created_at_ms,
        updated_at_ms: job.updated_at_ms,
    })
}
