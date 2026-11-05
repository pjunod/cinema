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

/// A live shared source reader. Paths and ownership tokens stay private; a
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
}

fn source_occupancy_default() -> bool {
    true
}
pub(super) const SOURCE_IO_HOLDERS_SQL: &str = r#"
SELECT json_object('resource_key', resource_key, 'work_id', work_id,
    'kind', kind, 'priority', priority, 'started_at_ms', started_at_ms,
    'origin_node_id',origin_node_id,'origin_job_kind',origin_job_kind,'origin_job_state',origin_job_state,
    'lease_expires_at_ms', lease_expires_at_ms, 'phase', phase,
    'last_phase_at_ms', last_phase_at_ms, 'active_occupancy',json(CASE WHEN active_occupancy=1 THEN 'true' ELSE 'false' END)) AS result_json
FROM (
  SELECT held.resource_key, held.job_id AS work_id, job.kind,
    CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters viewer
        WHERE viewer.job_id = job.id AND viewer.state IN ('pending','awaiting_hydration')
          AND viewer.deadline_ms > json_extract($1, '$.now_ms')
          AND viewer.request_scope = 'playback-analysis')
      THEN 'viewer' ELSE 'maintenance' END AS priority,
    COALESCE(attempt.started_at_ms, job.updated_at_ms) AS started_at_ms,
    held.expires_at_ms AS lease_expires_at_ms,
    NULL AS phase, NULL AS last_phase_at_ms, 1 AS active_occupancy, job.owner_node_id AS origin_node_id, job.kind AS origin_job_kind, job.state AS origin_job_state
  FROM background_job_reservations held JOIN background_jobs job ON job.id = held.job_id
  LEFT JOIN background_job_attempts attempt ON attempt.job_id = held.job_id
    AND attempt.fence = held.fence
  WHERE (held.resource_key = 'source_io' OR held.resource_key GLOB 'source_io:*')
    AND held.expires_at_ms > json_extract($1, '$.now_ms')
  UNION ALL
  SELECT held.resource_key, held.request_id AS work_id, 'analysis_source' AS kind,
    CASE WHEN request.trigger = 'playback' AND EXISTS (SELECT 1 FROM background_job_waiters viewer
        WHERE viewer.request_scope = 'playback-analysis'
          AND viewer.job_id = held.request_id AND viewer.state = 'pending'
          AND viewer.deadline_ms > json_extract($1, '$.now_ms'))
      THEN 'viewer' ELSE 'maintenance' END AS priority,
    COALESCE(attempt.started_at_ms, request.updated_at_ms) AS started_at_ms,
    held.expires_at_ms AS lease_expires_at_ms,
    attempt.phase AS phase, attempt.phase_updated_at_ms AS last_phase_at_ms, 1 AS active_occupancy,request.owner_node_id,NULL,NULL
  FROM analysis_source_reservations held
  JOIN analysis_requests request ON request.request_id = held.request_id
  LEFT JOIN analysis_attempts attempt ON attempt.request_id = held.request_id
    AND attempt.claim_epoch = held.fence
  WHERE (held.resource_key = 'source_io' OR held.resource_key GLOB 'source_io:*')
    AND held.expires_at_ms > json_extract($1, '$.now_ms')
  UNION ALL
  SELECT resource.resource_key,owner.owner_id,'foreground_source','foreground',owner.started_at_ms,
    owner.lease_expires_at_ms,CASE WHEN owner.lease_expires_at_ms<=json_extract($1,'$.now_ms')
      AND json_extract(owner.projection_json,'$.source_quiescent')!=1 THEN 'cleanup_pending' ELSE owner.phase END,owner.updated_at_ms,
    CASE WHEN owner.phase IN ('source_idle','sharing') AND json_extract(owner.projection_json,'$.source_quiescent')=1 THEN 0 ELSE 1 END
    ,json_extract(owner.identity_json,'$.node_id'),NULL,NULL
  FROM foreground_source_owners owner JOIN foreground_source_required_resources resource ON resource.owner_id=owner.owner_id
  WHERE owner.lease_expires_at_ms>json_extract($1,'$.now_ms') OR json_extract(owner.projection_json,'$.source_quiescent')!=1
  UNION ALL
  SELECT resource.value,custody.work_id,'background_source','maintenance',custody.started_at_ms,custody.authority_expires_at_ms,
    CASE WHEN custody.phase='paused' THEN 'source_paused' WHEN custody.authority_expires_at_ms>json_extract($1,'$.now_ms')
      THEN CASE custody.owner_kind WHEN 'lease' THEN 'read_attestation' WHEN 'offline' THEN 'offline_source' ELSE custody.phase END ELSE 'cleanup_pending' END,custody.updated_at_ms,
    CASE WHEN custody.phase='paused' THEN 0 ELSE 1 END
    ,COALESCE(json_extract(custody.owner_json,'$.token.node_id'),json_extract(custody.owner_json,'$.node_id'),json_extract(custody.owner_json,'$.lease.owner_node_id')),origin.kind,origin.state
  FROM background_source_current_owners custody LEFT JOIN background_jobs origin
    ON custody.owner_kind='job' AND origin.id=custody.work_id,json_each(custody.resource_keys_json) resource
  WHERE NOT EXISTS(SELECT 1 FROM background_job_reservations held WHERE custody.owner_kind='job' AND held.job_id=custody.work_id
      AND held.fence=custody.fence AND held.resource_key=resource.value AND held.expires_at_ms>json_extract($1,'$.now_ms'))
    AND NOT EXISTS(SELECT 1 FROM analysis_source_reservations held WHERE custody.owner_kind='analysis' AND held.request_id=custody.work_id
      AND held.fence=custody.fence AND held.resource_key=resource.value AND held.expires_at_ms>json_extract($1,'$.now_ms'))
    AND NOT EXISTS(SELECT 1 FROM background_source_pauses paused WHERE custody.owner_kind='job' AND paused.job_id=custody.work_id
      AND paused.fence=custody.fence AND paused.phase='paused')
  UNION ALL
  SELECT resource.value,job.id,job.kind,'maintenance',paused.paused_at_ms,job.lease_expires_ms,'source_paused',paused.paused_at_ms,0,paused.node_id,job.kind,job.state
  FROM background_source_pauses paused JOIN background_jobs job ON job.id=paused.job_id AND job.fence=paused.fence,
    json_each(paused.resource_keys_json) resource
  WHERE paused.phase='paused' AND job.state IN ('running','cancelling') AND job.lease_expires_ms>json_extract($1,'$.now_ms')
) ORDER BY (active_occupancy=1 AND phase='cleanup_pending') DESC,(priority='foreground') DESC,started_at_ms, work_id LIMIT 128
"#;

pub(super) const COUNTS_SQL: &str = r#"
SELECT json_object('kind', kind, 'state', state, 'count', COUNT(*),
    'oldest_age_ms', MAX(0, json_extract($1, '$.now_ms') - MIN(created_at_ms)),
    'overdue_manual', COUNT(CASE WHEN state = 'queued' AND kind IN ('library_scan', 'metadata_refresh')
        AND priority >= 2 AND MAX(created_at_ms, not_before_ms) <= json_extract($1, '$.manual_due_before_ms')
        THEN 1 END)) AS result_json
FROM background_jobs GROUP BY kind, state ORDER BY kind, state LIMIT 128
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
pub const JOB_METRIC_KINDS: [&str; 21] = [
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
}
impl Default for BackgroundJobMetrics {
    fn default() -> Self {
        Self {
            counts: [0; JOB_METRIC_SLOTS],
            oldest_age_seconds: [0; JOB_METRIC_SLOTS],
            source_io_reservations: 0,
            source_recovery_pending: 0,
            legacy_pending: 0,
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
    }
    let rows: Aggregate = serde_json::from_str(json).map_err(|error| {
        crate::error::StoreError::Database(format!("background metrics: {error}"))
    })?;
    let mut metrics = BackgroundJobMetrics {
        source_io_reservations: rows.source_io_reservations,
        source_recovery_pending: rows.source_recovery_pending,
        legacy_pending: rows.legacy_pending,
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
