//! Short-lived, node-verified library demand. The triggers arbitrate on the
//! existing source grant, including grants from older nodes during rollout.
use super::background_jobs::{encode, identifier, QueueSql};
use crate::error::StoreError;

pub(super) const SCHEMA: &str = include_str!("background_job_turns.sql");

pub(super) async fn advertise<T: QueueSql>(
    store: &T,
    job_id: &str,
    revision: i64,
    node: &str,
    roots: &[std::path::PathBuf],
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !identifier(node)
        || uuid::Uuid::parse_str(job_id).is_err()
        || revision < 0
        || !(0..=i64::MAX - 7000).contains(&now_ms)
    {
        return Err(StoreError::Task(
            "invalid library dispatch advertisement".into(),
        ));
    }
    let rows = store.queue_sql(r#"INSERT INTO dispatch_turn_ready(job_id,node_id,revision,expires_at_ms,roots)
        SELECT j.id,json_extract($1,'$.node'),j.revision,json_extract($1,'$.now')+7000,l.paths
        FROM background_jobs j JOIN libraries l ON l.id=json_extract(j.payload_json,'$.library_id')
        WHERE j.id=json_extract($1,'$.id') AND j.revision=json_extract($1,'$.revision')
        AND json(l.paths)=json_extract($1,'$.roots') AND json_array_length(l.paths)>0
        AND j.state='queued' AND j.kind IN ('library_scan','metadata_refresh')
        AND (j.target_node_id IS NULL OR j.target_node_id=json_extract($1,'$.node'))
        AND j.not_before_ms<=json_extract($1,'$.now') AND j.failed_attempts<j.attempt_limit
        AND (j.retry_deadline_ms=0 OR j.retry_deadline_ms>json_extract($1,'$.now'))
        AND NOT EXISTS (SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||json_extract($1,'$.node'))
        AND EXISTS(SELECT 1 FROM dispatch_turn_interest i WHERE i.job_id=j.id
          AND (i.deadline_ms IS NULL OR i.deadline_ms>json_extract($1,'$.now')))
        ON CONFLICT(job_id,node_id) DO UPDATE SET revision=excluded.revision,expires_at_ms=excluded.expires_at_ms,roots=excluded.roots
        RETURNING 'true' AS result_json"#.into(), encode(&serde_json::json!({"id":job_id,"revision":revision,"node":node,"now":now_ms,"roots":roots}))?, true, true).await?;
    Ok(!rows.is_empty())
}
