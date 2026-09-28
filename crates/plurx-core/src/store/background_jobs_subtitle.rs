//! Common ownership for the existing all-track subtitle-source extractor.
//! Analysis requests remain durable demand/history, never a second executor.
use super::background_jobs::{
    decode, encode, enqueue_body, EnqueueJob, EnqueueOutcome, JobPayload, JobRequest, JobToken,
    QueueSql, ENQUEUE_SQL,
};
use super::{AnalysisRequest, SubtitleSourcePublication};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const RESET_LEGACY: &str = "UPDATE analysis_requests SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL, fence = fence + 1 WHERE component = 'subtitle_source' AND state IN ('running','submitted');";

pub(crate) const SCHEMA: &str = include_str!("background_jobs_subtitle.sql");

/// The message `background_subtitle_claimed` raises when the demand behind a
/// queued `subtitle_extract` job is no longer claimable — cancelled, already
/// settled, or its source superseded since the job was admitted. The trigger
/// aborts the claim, so the job row stays `queued` and comes back on every
/// candidate page; without [`demand_gone`] each walker then treats the
/// refusal as an ambiguous write and waits out a full lease before moving on.
pub(crate) const DEMAND_GONE: &str = "subtitle demand no longer claimable";

/// Whether a claim error is that trigger speaking.
pub(crate) fn demand_gone(error: &StoreError) -> bool {
    error.to_string().contains(DEMAND_GONE)
}

/// The error code a job retired for that reason carries.
pub(crate) const DEMAND_GONE_CODE: &str = "subtitle_demand_gone";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubtitleJobWrite {
    Representation {
        publication: SubtitleSourcePublication,
    },
    Complete {
        result_key: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteSubtitleJob {
    pub token: JobToken,
    pub request: AnalysisRequest,
    pub output: SubtitleJobWrite,
    pub now_ms: i64,
}

pub(super) async fn intents<T: QueueSql>(
    store: &T,
    limit: usize,
) -> Result<Vec<String>, StoreError> {
    if !(1..=128).contains(&limit) {
        return Err(StoreError::Task("invalid subtitle outbox page".into()));
    }
    store.queue_sql(r#"
SELECT json_quote(request.request_id) AS result_json FROM analysis_requests request
WHERE request.component = 'subtitle_source' AND request.state = 'queued' AND request.cancel_requested = 0
    AND NOT EXISTS (SELECT 1 FROM background_job_waiters waiter WHERE waiter.request_scope = 'subtitle'
        AND waiter.request_id = request.request_id)
ORDER BY CASE request.priority WHEN 'foreground' THEN 0 ELSE 1 END, request.created_at_ms, request.request_id
LIMIT json_extract($1, '$.limit')
"#.into(), encode(&serde_json::json!({"limit": limit}))?, false, false).await?.iter().map(|row| decode(row)).collect()
}

pub(super) async fn enqueue<T: QueueSql>(
    store: &T,
    input: AnalysisRequest,
    now_ms: i64,
) -> Result<EnqueueOutcome, StoreError> {
    if input.component != "subtitle_source"
        || !input.video_identity.is_empty()
        || !input.target_node_id.is_empty()
    {
        return Err(StoreError::Task("invalid subtitle demand".into()));
    }
    let pipeline_digest = hex::encode(Sha256::digest(input.pipeline_version.as_bytes()));
    let payload = JobPayload::SubtitleExtract {
        file_id: input.file_id,
        source_generation: input.request_id.clone(),
        track: None,
        pipeline_digest,
    };
    let digest = hex::encode(Sha256::digest(encode(&payload)?));
    let request = EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload,
        dedupe_key: format!("subtitle:{}", input.request_id),
        priority: if input.priority == "foreground" { 2 } else { 0 },
        not_before_ms: input.not_before_ms,
        now_ms,
        request: JobRequest {
            scope: "subtitle".into(),
            request_id: input.request_id.clone(),
            request_digest: digest,
            consumer_kind: "subtitle_source".into(),
            consumer_ref: input.request_id.clone(),
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
    };
    request.validate()?;
    let mut body = enqueue_body(store, &request).await?;
    body["subtitle_request"] =
        serde_json::to_value(input).map_err(|error| StoreError::Task(error.to_string()))?;
    let rows = store
        .queue_sql(ENQUEUE_SQL.into(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing subtitle admission verdict".into()))?,
    )
}

const WRITE_SQL: &str = r#"
INSERT INTO background_job_commands(id, operation, request_json, result_json)
SELECT json_extract($1, '$.token.claim_id'), 'subtitle_output', $1,
CASE WHEN EXISTS (
    SELECT 1 FROM background_jobs job JOIN analysis_requests request
        ON request.request_id = json_extract(job.payload_json, '$.source_generation')
    JOIN files file ON file.id = request.file_id AND file.size = request.source_size AND file.mtime = request.source_mtime
    WHERE job.id = json_extract($1, '$.token.job_id') AND job.kind = 'subtitle_extract' AND job.payload_version = 1
        AND request.request_id = json_extract($1, '$.request.request_id')
        AND request.file_id = json_extract(job.payload_json, '$.file_id')
        AND request.file_id = json_extract($1, '$.request.file_id')
        AND request.source_size = json_extract($1, '$.request.source_size')
        AND request.source_mtime = json_extract($1, '$.request.source_mtime')
        AND request.pipeline_version = json_extract($1, '$.request.pipeline_version')
        AND request.component = 'subtitle_source' AND request.cancel_requested = 0
        AND request.fence = json_extract($1, '$.request.fence')
        AND job.fence = json_extract($1, '$.token.fence')
        AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || json_extract($1, '$.token.node_id'))
        AND (
            (job.state = 'succeeded' AND request.state = 'ready' AND json_extract($1, '$.output.kind') = 'complete'
                AND job.result_ref = json_extract($1, '$.output.result_key')
                AND EXISTS (SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id = job.id
                    AND attempt.fence = job.fence AND attempt.claim_id = json_extract($1, '$.token.claim_id')
                    AND attempt.owner_node_id = json_extract($1, '$.token.node_id')
                    AND attempt.owner_boot_id = json_extract($1, '$.token.boot_id')))
            OR (job.state = 'running' AND request.state = 'running'
                AND request.owner_node_id = job.owner_node_id
                AND job.owner_node_id = json_extract($1, '$.token.node_id')
                AND job.owner_boot_id = json_extract($1, '$.token.boot_id')
                AND job.claim_id = json_extract($1, '$.token.claim_id')
                AND job.revision = json_extract($1, '$.token.revision')
                AND job.lease_expires_ms = json_extract($1, '$.token.lease_expires_ms')
                AND job.lease_expires_ms > json_extract($1, '$.now_ms')
                AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
                    WHERE required.job_id = job.id AND NOT EXISTS (SELECT 1 FROM background_job_reservations held
                        WHERE held.job_id = job.id AND held.fence = job.fence AND held.resource_key = required.resource_key
                            AND held.expires_at_ms >= job.lease_expires_ms)))
        )
) THEN 'true' ELSE 'false' END
RETURNING result_json
"#;

pub(super) async fn write<T: QueueSql>(
    store: &T,
    request: WriteSubtitleJob,
) -> Result<bool, StoreError> {
    request.token.validate()?;
    if request.now_ms < 0 || request.request.component != "subtitle_source" {
        return Err(StoreError::Task("invalid subtitle publication".into()));
    }
    match &request.output {
        SubtitleJobWrite::Representation { publication } => {
            if !super::fragment_index_cluster::valid_subtitle_source_publication(publication)
                || publication.node_id != request.token.node_id
                || publication.file_id != request.request.file_id
                || publication.source_size != request.request.source_size
                || publication.source_mtime != request.request.source_mtime
                || publication.origin != "extracted"
            {
                return Err(StoreError::Task(
                    "subtitle publication does not match its source owner".into(),
                ));
            }
        }
        SubtitleJobWrite::Complete { result_key }
            if result_key.is_empty() || result_key.len() > 160 =>
        {
            return Err(StoreError::Task("invalid subtitle result".into()))
        }
        _ => {}
    }
    let rows = store
        .queue_sql(WRITE_SQL.into(), encode(&request)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing subtitle publication verdict".into()))?,
    )
}
