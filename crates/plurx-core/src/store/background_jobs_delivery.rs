//! A build's waiting target receipts are its durable delivery outbox. These
//! queries only expose bounded scheduling intent; publication settles receipts.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobWaiter {
    pub scope: String,
    pub request_id: String,
    pub job_id: String,
    pub consumer_kind: String,
    pub consumer_ref: String,
    pub state: String,
    pub priority: u8,
    pub target_node_id: Option<String>,
    pub deadline_ms: Option<i64>,
    pub result_ref: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaiterCursor {
    pub scope: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaiterQuery {
    pub job_id: String,
    pub after: Option<WaiterCursor>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaiterPage {
    pub waiters: Vec<JobWaiter>,
    pub next: Option<WaiterCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryIntent {
    pub job_id: String,
    pub target_node_id: String,
    pub artifact_key: String,
    pub priority: u8,
}

pub(super) const WAITERS_SQL: &str = r#"
SELECT json_object('scope', request_scope, 'request_id', request_id, 'job_id', job_id,
    'consumer_kind', consumer_kind, 'consumer_ref', consumer_ref, 'state', state,
    'priority', priority, 'target_node_id', target_node_id, 'deadline_ms', deadline_ms,
    'result_ref', result_ref, 'updated_at_ms', updated_at_ms) AS result_json
FROM background_job_waiters WHERE job_id = json_extract($1, '$.job_id')
    AND (json_extract($1, '$.after') IS NULL OR (request_scope, request_id) >
        (json_extract($1, '$.after.scope'), json_extract($1, '$.after.request_id')))
ORDER BY request_scope, request_id LIMIT json_extract($1, '$.limit') + 1
"#;

pub(super) const DELIVERIES_SQL: &str = r#"
SELECT json_object('job_id', job_id, 'target_node_id', target_node_id,
    'artifact_key', 'fragment:' || json_extract(result_ref, '$.artifact_key'),
    'priority', MAX(priority)) AS result_json
FROM background_job_waiters WHERE state = 'awaiting_hydration' AND target_node_id IS NOT NULL
    AND (deadline_ms IS NULL OR deadline_ms > json_extract($1, '$.now_ms'))
    AND CASE WHEN json_valid(result_ref) THEN json_extract(result_ref, '$.artifact_kind') END = 'fragment_index'
    AND NOT EXISTS (SELECT 1 FROM background_job_waiters delivery
        WHERE delivery.request_scope = 'delivery:' || background_job_waiters.job_id
          AND delivery.request_id = background_job_waiters.target_node_id)
GROUP BY job_id, target_node_id, result_ref ORDER BY MIN(updated_at_ms), job_id, target_node_id LIMIT 128
"#;
