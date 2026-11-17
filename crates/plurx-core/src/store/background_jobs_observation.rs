//! Bounded operator observations. Ownership tokens and payloads remain private
//! to execution paths; these records can be shown without exposing source paths.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCount {
    pub kind: String,
    pub state: String,
    pub count: i64,
    pub oldest_age_ms: i64,
    /// Manual library work due for dispatch for at least five minutes.
    #[serde(default)]
    pub overdue_manual: i64,
}

pub const JOB_PROCESSING_WINDOW_MS: i64 = 5 * 60 * 1000;

/// Completed worker attempts still present in the observation window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeJobCompletions {
    pub node_id: String,
    pub succeeded: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobAttemptObservation {
    pub fence: i64,
    pub node_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub outcome: Option<String>,
    pub error_code: Option<String>,
}

/// A source lease: one reader, on one node. Paths and ownership tokens stay private; a
/// phase timestamp is reported only for analysis, where it is recorded apart
/// from lease renewal. `None` means useful progress is not observable here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceIoHolder {
    #[serde(default = "source_occupancy_default")]
    pub active_occupancy: bool,
    pub origin_node_id: Option<String>,
    pub origin_job_kind: Option<String>,
    pub origin_job_state: Option<String>,
    pub resource_key: String,
    pub work_id: String,
    pub kind: String,
    pub priority: String,
    pub started_at_ms: i64,
    pub lease_expires_at_ms: i64,
    pub phase: Option<String>,
    pub last_phase_at_ms: Option<i64>,
    /// What this lease is charged against: `root` (its canonical root),
    /// `domain` (its mapped storage domain), `unrooted` (the node-local
    /// fallback), or `none` (playback, paused, or past its grace).
    #[serde(default)]
    pub counts_against: Option<String>,
    /// The lease serves live demand, so it may take its resource's reserved live slot.
    #[serde(default)]
    pub live_demand: bool,
    #[serde(default)]
    pub lease_id: Option<String>,
}

fn source_occupancy_default() -> bool {
    true
}
/// Every source lease and stored key. `{live}` and `{counted}` are the claim's
/// own terms (`source_leases::{live, counted}`) over the alias `held`.
pub(super) fn source_io_holders_sql() -> String {
    SOURCE_IO_HOLDERS_SQL
        .replace(
            "{live}",
            &super::source_leases::live("json_extract($1,'$.now_ms')"),
        )
        .replace(
            "{counted}",
            &super::source_leases::counted("json_extract($1,'$.now_ms')"),
        )
}
const SOURCE_IO_HOLDERS_SQL: &str = r#"
SELECT json_object('resource_key', resource_key, 'work_id', work_id,
    'kind', kind, 'priority', priority, 'started_at_ms', started_at_ms,
    'origin_node_id',origin_node_id,'origin_job_kind',origin_job_kind,'origin_job_state',origin_job_state,
    'lease_expires_at_ms', lease_expires_at_ms, 'phase', phase,
    'last_phase_at_ms', last_phase_at_ms, 'active_occupancy',json(CASE WHEN active_occupancy=1 THEN 'true' ELSE 'false' END),
    'counts_against', counts_against, 'live_demand', json(CASE WHEN live_demand=1 THEN 'true' ELSE 'false' END),
    'lease_id', lease_id) AS result_json
FROM (
  -- One row per source lease and stored key: who holds which source
  -- capacity on which node, and why.
  SELECT key.value AS resource_key, held.lease_id,
    CASE held.owner_kind WHEN 'playback' THEN held.lease_id ELSE held.work_id END AS work_id,
    CASE held.owner_kind WHEN 'playback' THEN 'foreground_source' WHEN 'job' THEN COALESCE(job.kind,json_extract(held.owner_json,'$.job_kind'))
      WHEN 'analysis' THEN 'analysis_source' ELSE 'background_source' END AS kind,
    CASE WHEN held.owner_kind='playback' THEN 'foreground' WHEN {live} THEN 'viewer' ELSE 'maintenance' END AS priority,
    CASE WHEN held.owner_kind='playback' THEN 0 WHEN {live} THEN 1 ELSE 0 END AS live_demand,
    COALESCE(attempt.started_at_ms, analysis_attempt.started_at_ms, held.started_at_ms) AS started_at_ms,
    held.expires_at_ms AS lease_expires_at_ms,
    CASE WHEN held.phase='paused' THEN 'source_paused'
      WHEN held.expires_at_ms<=json_extract($1,'$.now_ms') THEN 'cleanup_pending'
      WHEN held.owner_kind='playback' THEN held.phase
      WHEN held.owner_kind='analysis' THEN analysis_attempt.phase
      WHEN held.owner_kind='lease' THEN 'read_attestation' WHEN held.owner_kind='offline' THEN 'offline_source'
      ELSE NULL END AS phase,
    CASE WHEN held.owner_kind='analysis' THEN analysis_attempt.phase_updated_at_ms ELSE held.renewed_at_ms END AS last_phase_at_ms,
    CASE WHEN held.owner_kind='playback' THEN
        CASE WHEN held.phase IN ('source_idle','sharing') AND json_extract(held.projection_json,'$.source_quiescent')=1 THEN 0 ELSE 1 END
      WHEN {counted} THEN 1 ELSE 0 END AS active_occupancy,
    CASE WHEN held.owner_kind='playback' OR NOT {counted} THEN 'none' WHEN key.value='source_io' THEN 'unrooted' WHEN substr(key.value,1,15)='source_io:root/' THEN 'root' ELSE 'domain' END AS counts_against,
    held.node_id AS origin_node_id,
    CASE held.owner_kind WHEN 'job' THEN COALESCE(job.kind,json_extract(held.owner_json,'$.job_kind')) END AS origin_job_kind,
    job.state AS origin_job_state
  FROM source_leases held JOIN (SELECT lease_id,resource_key AS value FROM source_lease_effective_resources) key ON key.lease_id=held.lease_id
  LEFT JOIN background_jobs job ON held.owner_kind='job' AND job.id=held.work_id
  LEFT JOIN background_job_attempts attempt ON held.owner_kind='job' AND attempt.job_id=held.work_id AND attempt.fence=held.fence
  LEFT JOIN analysis_attempts analysis_attempt ON held.owner_kind='analysis' AND analysis_attempt.request_id=held.work_id
    AND analysis_attempt.claim_epoch=held.fence
  -- Every lease, including one past its grace that the sweep has not yet
  -- collected: it holds nothing, and its growing age shows the sweep stopped.
  WHERE key.value LIKE 'source_io%'
) ORDER BY origin_node_id,(active_occupancy=1 AND phase='cleanup_pending') DESC,(priority='foreground') DESC,started_at_ms,work_id LIMIT 128
"#;

pub(super) const COUNTS_SQL: &str = r#"
SELECT json_object('kind', kind, 'state', state, 'count', COUNT(*),
    'oldest_age_ms', MAX(0, json_extract($1, '$.now_ms') - MIN(created_at_ms)),
    'overdue_manual', COUNT(CASE WHEN state = 'queued' AND kind IN ('library_scan', 'metadata_refresh')
        AND not_before_ms <= json_extract($1, '$.manual_due_before_ms')
        AND EXISTS (
            SELECT 1 FROM background_job_waiters waiter
            JOIN background_library_requests intent ON intent.request_id = waiter.request_id
            WHERE waiter.job_id = job.id AND waiter.request_scope = 'library' AND waiter.state = 'pending'
              AND waiter.created_at_ms <= json_extract($1, '$.manual_due_before_ms')
              AND (json_extract(intent.input_json, '$.kind') = 'targeted'
                   OR json_extract(intent.input_json, '$.trigger') = 'manual'))
        THEN 1 END)) AS result_json
FROM background_jobs job GROUP BY kind, state ORDER BY kind, state LIMIT 128
"#;

pub(super) const NODE_COMPLETIONS_SQL: &str = r#"
SELECT json_object('node_id', owner_node_id, 'succeeded', SUM(outcome = 'succeeded'),
    'failed', SUM(outcome = 'failed')) AS result_json
FROM background_job_attempts
WHERE finished_at_ms > json_extract($1, '$.after_ms')
  AND finished_at_ms <= json_extract($1, '$.now_ms')
  AND outcome IN ('succeeded', 'failed')
GROUP BY owner_node_id ORDER BY owner_node_id
"#;

pub(super) const ATTEMPTS_SQL: &str = r#"
SELECT json_object('fence', fence, 'node_id', owner_node_id, 'started_at_ms', started_at_ms,
    'finished_at_ms', finished_at_ms, 'outcome', outcome, 'error_code', error_code) AS result_json
FROM background_job_attempts WHERE job_id = json_extract($1, '$.job_id') ORDER BY fence DESC LIMIT 16
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLabel {
    pub id: String,
    pub title: Option<String>,
    pub library: Option<String>,
}

pub(super) const LABELS_SQL: &str = r#"
SELECT json_object('id', job.id, 'title', item.title, 'library', library.name) AS result_json
FROM json_each($1, '$.ids') requested JOIN background_jobs job ON job.id = requested.value
LEFT JOIN cluster_fragment_index_artifacts artifact ON job.kind = 'artifact_hydrate'
    AND 'fragment:' || artifact.cache_key = json_extract(job.payload_json, '$.artifact_key')
LEFT JOIN files file ON file.id = COALESCE(json_extract(job.payload_json, '$.file_id'), artifact.file_id)
LEFT JOIN items item ON item.id = file.item_id
LEFT JOIN libraries library ON library.id = COALESCE(item.library_id, json_extract(job.payload_json, '$.library_id'))
ORDER BY job.id
"#;

/// Closed metric labels: persisted unknown kinds never become labels.
pub const JOB_METRIC_KINDS: [&str; 22] = [
    "transcode_prepare",
    "fragment_index_build",
    "artifact_hydrate",
    "subtitle_extract",
    "library_scan",
    "metadata_refresh",
    "artifact_verify",
    "artwork_derivative",
    "semantic_embedding",
    "media_probe",
    "copy_output_prepare",
    "encoded_output_prepare",
    "subtitle_transcribe",
    "subtitle_transcribe_managed",
    "subtitle_model_operation",
    "subtitle_translation_prepare",
    "subtitle_translation_range",
    "dv_title_scan",
    "subtitle_text_translate",
    "subtitle_text_model_operation",
    "subtitle_text_translate_artifact",
    "fragment_index_publish",
];
pub const JOB_METRIC_STATES: [&str; 6] = [
    "queued",
    "running",
    "cancelling",
    "succeeded",
    "failed",
    "cancelled",
];
pub const JOB_METRIC_SLOTS: usize = JOB_METRIC_KINDS.len() * JOB_METRIC_STATES.len();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundJobMetrics {
    pub counts: [i64; JOB_METRIC_SLOTS],
    pub oldest_age_seconds: [i64; JOB_METRIC_SLOTS],
    pub source_io_reservations: i64,
    pub source_recovery_pending: i64,
    pub legacy_pending: i64,
    /// This node's source leases by [`SOURCE_LEASE_KINDS`].
    pub source_leases: [i64; SOURCE_LEASE_KINDS.len()],
    /// Background source readers this node may run (its override or the default).
    pub source_readers_per_root: i64,
    /// 2 expanded (v1 objects still present for older binaries), 3 contracted.
    pub source_lease_schema_version: i64,
}
/// Lease holder kinds, in metric order.
pub const SOURCE_LEASE_KINDS: [&str; 5] = ["playback", "job", "analysis", "lease", "offline"];
impl Default for BackgroundJobMetrics {
    fn default() -> Self {
        Self {
            counts: [0; JOB_METRIC_SLOTS],
            oldest_age_seconds: [0; JOB_METRIC_SLOTS],
            source_io_reservations: 0,
            source_recovery_pending: 0,
            legacy_pending: 0,
            source_leases: [0; SOURCE_LEASE_KINDS.len()],
            source_readers_per_root: 0,
            source_lease_schema_version: 0,
        }
    }
}

/// Parse the bounded aggregate carried in the existing Store metrics read.
/// Reject malformed snapshots instead of publishing a misleading empty queue.
pub(crate) fn background_job_metrics(
    json: &str,
) -> Result<BackgroundJobMetrics, crate::error::StoreError> {
    #[derive(Deserialize)]
    struct Aggregate {
        jobs: Vec<JobCount>,
        source_io_reservations: i64,
        source_recovery_pending: i64,
        legacy_pending: i64,
        #[serde(default)]
        source_leases: std::collections::BTreeMap<String, i64>,
        #[serde(default)]
        source_readers_per_root: i64,
        #[serde(default)]
        source_lease_schema_version: i64,
    }
    let rows: Aggregate = serde_json::from_str(json).map_err(|error| {
        crate::error::StoreError::Database(format!("background metrics: {error}"))
    })?;
    let mut metrics = BackgroundJobMetrics {
        source_io_reservations: rows.source_io_reservations,
        source_recovery_pending: rows.source_recovery_pending,
        legacy_pending: rows.legacy_pending,
        source_leases: SOURCE_LEASE_KINDS
            .map(|kind| rows.source_leases.get(kind).copied().unwrap_or(0).max(0)),
        source_readers_per_root: rows.source_readers_per_root,
        source_lease_schema_version: rows.source_lease_schema_version,
        ..Default::default()
    };
    for row in rows.jobs {
        if let (Some(kind), Some(state)) = (
            JOB_METRIC_KINDS.iter().position(|name| *name == row.kind),
            JOB_METRIC_STATES.iter().position(|name| *name == row.state),
        ) {
            let slot = kind * JOB_METRIC_STATES.len() + state;
            metrics.counts[slot] = row.count.max(0);
            metrics.oldest_age_seconds[slot] = row.oldest_age_ms.max(0) / 1000;
        }
    }
    Ok(metrics)
}
