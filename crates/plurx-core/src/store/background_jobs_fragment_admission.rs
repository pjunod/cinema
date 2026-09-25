//! Convert domain-authorized fragment requests into shared computations.
//! Target identity belongs to the waiter; equivalent source/pipeline builds
//! therefore converge across target nodes.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::background_jobs::{EnqueueJob, JobPayload, JobRequest};
use super::{AnalysisRequest, NewClusterFragmentIndexJob};
use crate::error::StoreError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnqueueFragmentJob {
    pub job: NewClusterFragmentIndexJob,
    pub analysis_request: Option<AnalysisRequest>,
    pub repair: bool,
    pub now_ms: i64,
}

pub(super) fn prepare(input: &EnqueueFragmentJob) -> Result<(EnqueueJob, String), StoreError> {
    let job = &input.job;
    let invalid = || StoreError::Task("invalid durable fragment admission".into());
    let logical_key = super::cluster_fragment_index_key(
        job.file_id,
        job.source_size,
        job.source_mtime,
        &job.source_sha256,
        &job.pipeline_sha256,
    )
    .ok_or_else(invalid)?;
    if !matches!(job.priority.as_str(), "normal" | "forced" | "foreground")
        || !matches!(job.trigger.as_str(), "admin" | "background" | "foreground")
        || job.target_node_id.is_empty()
        || job.target_node_id.len() > 128
        || input.now_ms < 0
    {
        return Err(invalid());
    }
    if let Some(request) = &input.analysis_request {
        if request.component != "fragment_index"
            || request.state != "running"
            || request.owner_node_id.is_empty()
            || request.fence <= 0
            || request.cancel_requested
            || request.file_id != job.file_id
            || request.source_size != job.source_size
            || request.source_mtime != job.source_mtime
            || request.priority != job.priority
            || request.trigger != job.trigger
            || request.target_node_id != job.target_node_id
        {
            return Err(invalid());
        }
        if request.force_rebuild {
            let generation_key = super::cluster_fragment_index_generation_key(
                job.file_id,
                job.source_size,
                job.source_mtime,
                &job.source_sha256,
                &job.pipeline_sha256,
                &request.requested_generation,
            )
            .ok_or_else(invalid)?;
            if generation_key != job.cache_key {
                return Err(invalid());
            }
        }
    }
    let payload = JobPayload::FragmentIndexBuild {
        file_id: job.file_id,
        source_generation: job.source_sha256.clone(),
        source_size: job.source_size,
        source_mtime: job.source_mtime,
        source_sha256: job.source_sha256.clone(),
        cache_key: job.cache_key.clone(),
        pipeline_digest: job.pipeline_sha256.clone(),
    };
    let identity = serde_json::to_vec(
        &serde_json::json!({"payload": payload, "target_node_id": job.target_node_id}),
    )
    .map_err(|error| StoreError::Task(error.to_string()))?;
    let request_digest = hex::encode(Sha256::digest(identity));
    let (scope, request_id, retain_identity) = match &input.analysis_request {
        Some(request) => ("analysis".into(), request.request_id.clone(), true),
        None => (
            format!(
                "fragment-{}:{}",
                if input.repair { "repair" } else { "automatic" },
                job.target_node_id
            ),
            job.cache_key.clone(),
            false,
        ),
    };
    let request = EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload,
        dedupe_key: format!("fragment:{}", job.cache_key),
        priority: match job.priority.as_str() {
            "foreground" => 3,
            "forced" => 2,
            _ => 1,
        },
        not_before_ms: job.not_before_ms.max(input.now_ms),
        now_ms: input.now_ms,
        request: JobRequest {
            scope,
            request_id: request_id.clone(),
            request_digest,
            consumer_kind: "fragment_analysis".into(),
            consumer_ref: request_id,
            target_node_id: Some(job.target_node_id.clone()),
            deadline_ms: if retain_identity {
                None
            } else {
                Some(input.now_ms.saturating_add(86_400_000))
            },
            retain_identity,
        },
    };
    request.validate()?;
    Ok((request, logical_key))
}
