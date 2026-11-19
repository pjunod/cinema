//! Short-lived, node-verified library demand. The triggers arbitrate on the
//! existing source grant, including grants from older nodes during rollout.
use super::background_jobs::{encode, identifier, LibraryTurnCapacity, QueueSql};
use crate::error::StoreError;

pub(super) const SCHEMA: &str = include_str!("background_job_turns.sql");

pub(super) async fn advertise<T: QueueSql>(
    store: &T,
    job_id: &str,
    revision: i64,
    node: &str,
    roots: &[std::path::PathBuf],
    capacity: LibraryTurnCapacity,
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
        SELECT j.id,json_extract($1,'$.node'),j.revision,json_extract($1,'$.now') + CASE
          WHEN json_extract($1,'$.capacity')='available' THEN 7000
          -- The busy node may keep the next turn warm only while it owns a
          -- current ordinary source fence on an overlapping disk. The grant
          -- still checks physical occupancy. At release this gives the worker
          -- four seconds (one three-second discovery tick plus a poll) to
          -- acquire the local lane, without pinning other workers indefinitely.
          WHEN json_extract($1,'$.capacity')='heavy_occupied' AND EXISTS (
            SELECT 1 FROM json_each(l.paths) root
            LEFT JOIN background_storage_domains m ON m.library_id=l.id AND m.root_path=root.value
            JOIN (
                SELECT h.resource_key FROM background_job_reservations h
                JOIN background_jobs owner ON owner.id=h.job_id AND owner.fence=h.fence
                WHERE owner.owner_node_id=json_extract($1,'$.node') AND owner.state='running'
                  AND owner.lease_expires_ms>json_extract($1,'$.now') AND h.expires_at_ms>json_extract($1,'$.now')
                  AND h.resource_key LIKE 'source_io%'
                  AND NOT EXISTS(SELECT 1 FROM background_source_pauses p
                      WHERE p.job_id=owner.id AND p.fence=h.fence AND p.phase='paused')
                  -- Using the foreground slot does not promise a heavy-lane
                  -- handoff. When uncertain, wait for local capacity instead.
                  AND NOT EXISTS(SELECT 1 FROM background_job_waiters v WHERE v.job_id=owner.id
                    AND v.state IN ('pending','awaiting_hydration') AND (v.deadline_ms IS NULL OR v.deadline_ms>json_extract($1,'$.now'))
                    AND (v.consumer_kind IN ('playback_fragment','background_delivery','subtitle_translation','subtitle_translation_range')
                      OR (v.consumer_kind='subtitle_source' AND EXISTS(SELECT 1 FROM analysis_requests sub
                        WHERE sub.request_id=v.consumer_ref AND sub.trigger='playback'))))
                UNION ALL
                SELECT h.resource_key FROM analysis_source_reservations h
                JOIN analysis_requests owner ON owner.request_id=h.request_id AND owner.fence=h.fence
                WHERE owner.owner_node_id=json_extract($1,'$.node') AND owner.state='running'
                  AND owner.cancel_requested=0 AND owner.lease_expires_ms>json_extract($1,'$.now')
                  AND h.expires_at_ms>json_extract($1,'$.now') AND h.resource_key LIKE 'source_io%'
                  AND NOT EXISTS(SELECT 1 FROM background_job_waiters v WHERE v.job_id=owner.request_id
                    AND v.request_scope='playback-analysis' AND v.state='pending' AND v.deadline_ms>json_extract($1,'$.now'))
            ) held ON m.domain_id IS NULL OR held.resource_key='source_io' OR held.resource_key='source_io:'||m.domain_id
          ) THEN 4000 ELSE 0 END,l.paths
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
        RETURNING 'true' AS result_json"#.into(), encode(&serde_json::json!({"id":job_id,"revision":revision,"node":node,"now":now_ms,"roots":roots,"capacity":capacity}))?, true, true).await?;
    Ok(!rows.is_empty())
}
