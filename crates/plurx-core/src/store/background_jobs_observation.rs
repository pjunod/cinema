//! Bounded operator observations. Ownership tokens and payloads remain private
//! to execution paths; these records can be shown without exposing source paths.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCount {
    pub kind: String,
    pub state: String,
    pub count: i64,
    pub oldest_age_ms: i64,
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

pub(super) const COUNTS_SQL: &str = r#"
SELECT json_object('kind', kind, 'state', state, 'count', COUNT(*),
    'oldest_age_ms', MAX(0, json_extract($1, '$.now_ms') - MIN(created_at_ms))) AS result_json
FROM background_jobs GROUP BY kind, state ORDER BY kind, state LIMIT 128
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
pub const JOB_METRIC_KINDS: [&str; 10] = [
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
    pub legacy_pending: i64,
}
impl Default for BackgroundJobMetrics {
    fn default() -> Self {
        Self {
            counts: [0; JOB_METRIC_SLOTS],
            oldest_age_seconds: [0; JOB_METRIC_SLOTS],
            source_io_reservations: 0,
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
        legacy_pending: i64,
    }
    let rows: Aggregate = serde_json::from_str(json).map_err(|error| {
        crate::error::StoreError::Database(format!("background metrics: {error}"))
    })?;
    let mut metrics = BackgroundJobMetrics {
        source_io_reservations: rows.source_io_reservations,
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
