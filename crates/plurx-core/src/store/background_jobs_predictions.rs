//! Predictions are bounded admission intents for existing typed analysis workers.
//! They own only their own analysis request; real demand may adopt subtitles.
use super::background_jobs::{decode, digest, encode, QueueSql};
use super::NewAnalysisRequest;
use crate::cluster::coordination::Lease;
use crate::error::StoreError;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = include_str!("background_jobs_predictions.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 79;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPredictions {
    pub requests: Vec<NewAnalysisRequest>,
    pub desired_files: Vec<i64>,
    pub lease: Lease,
    pub replacement: Lease,
    pub now_ms: i64,
}

pub(super) async fn sync<T: QueueSql>(store: &T, input: SyncPredictions) -> Result<(), StoreError> {
    let lease = &input.lease;
    let replacement = &input.replacement;
    if input.requests.len() > 64
        || input.desired_files.len() > 64
        || input.desired_files.iter().any(|id| *id <= 0)
        || input.now_ms < 0
        || input.now_ms > i64::MAX - 86400000
        || lease.resource != "candidate:prediction"
        || lease.resource != replacement.resource
        || lease.owner_node_id != replacement.owner_node_id
        || lease.fence != replacement.fence
        || lease.fence == 0
        || lease.fence > i64::MAX as u64
        || lease.revision == 0
        || lease.revision >= i64::MAX as u64
        || replacement.revision != lease.revision + 1
        || replacement.expires_at_unix_ms <= lease.expires_at_unix_ms
        || input.requests.iter().any(|request| {
            !digest(&request.request_id)
                || request.requested_generation.strip_prefix("predict:")
                    != Some(request.request_id.as_str())
                || request.file_id <= 0
                || request.source_size < 0
                || request.source_mtime < 0
                || request.priority != "normal"
                || request.trigger != "background"
                || request.force_rebuild
                || !request.video_identity.is_empty()
                || !matches!(
                    request.component.as_str(),
                    "fragment_index" | "subtitle_source"
                )
                || request.pipeline_version.is_empty()
                || request.pipeline_version.len() > 128
                || request.target_node_id.len() > 128
                || (request.component == "subtitle_source" && !request.target_node_id.is_empty())
                || (request.component == "fragment_index" && request.target_node_id.is_empty())
        })
    {
        return Err(StoreError::Task(
            "invalid prediction intent or producer fence".into(),
        ));
    }
    let body = encode(&input)?;
    if body.len() > 64 * 1024 {
        return Err(StoreError::Task("prediction page too large".into()));
    }
    let rows = store.queue_sql(r#"
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT 'sync-predictions', 'sync_predictions', $1, 'true'
WHERE EXISTS (SELECT 1 FROM job_leases WHERE resource = json_extract($1,'$.lease.resource')
 AND owner_node_id = json_extract($1,'$.lease.owner_node_id') AND fence = json_extract($1,'$.lease.fence')
 AND revision = json_extract($1,'$.lease.revision') AND expires_at_ms = json_extract($1,'$.lease.expires_at_unix_ms')
 AND expires_at_ms > json_extract($1,'$.now_ms')
 AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || owner_node_id))
RETURNING result_json
"#.into(), body, true, true).await?;
    if rows.is_empty() {
        return Err(StoreError::FenceRejected {
            resource: lease.resource.clone(),
            owner_node_id: lease.owner_node_id.clone(),
            fence: lease.fence,
        });
    }
    Ok(())
}

pub(super) async fn pending<T: QueueSql>(
    store: &T,
    now_ms: i64,
) -> Result<Vec<NewAnalysisRequest>, StoreError> {
    if now_ms < 0 {
        return Err(StoreError::Task("invalid prediction time".into()));
    }
    let rows = store.queue_sql(r#"
SELECT json_set(request_json, '$.created_at_ms', json_extract($1,'$.now_ms'), '$.not_before_ms', json_extract($1,'$.now_ms')) AS result_json
FROM background_predictions prediction WHERE state = 'pending' AND expires_ms > json_extract($1,'$.now_ms')
 AND NOT EXISTS (SELECT 1 FROM analysis_requests WHERE request_id = prediction.request_id)
ORDER BY created_at_ms, request_id LIMIT 64
"#.into(), encode(&serde_json::json!({"now_ms":now_ms}))?, false, true).await?;
    rows.iter().map(|row| decode(row)).collect()
}
