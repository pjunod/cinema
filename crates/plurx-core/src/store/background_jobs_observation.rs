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
