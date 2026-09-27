//! Bounded preparation inputs derived from durable demand, not a second queue.
use serde::{Deserialize, Serialize};

use super::background_jobs::{decode, encode, QueueSql};
use crate::error::StoreError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreparationDemand {
    Item {
        item_id: i64,
        title: String,
    },
    Viewer {
        user_id: i64,
    },
    Channel {
        channel_id: String,
        generation_id: String,
        file_id: i64,
        item_id: i64,
        title: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotArtifact {
    pub artifact_key: String,
    pub bytes: i64,
    pub holders: Vec<String>,
    pub demanded_at_ms: i64,
    pub pending_copy: bool,
}

// Watch progress is already replicated. Count distinct viewers of an item in
// the last day, rather than replicating every local playback telemetry event.
const HOT_ITEMS: &str = r#"
SELECT item_id, COUNT(*) AS viewers, MAX(updated_at) * 1000 AS demanded_at_ms
FROM watch_state WHERE updated_at > json_extract($1, '$.now_ms') / 1000 - 86400
    AND updated_at <= json_extract($1, '$.now_ms') / 1000
GROUP BY item_id ORDER BY viewers DESC, demanded_at_ms DESC, item_id LIMIT 128
"#;

pub(super) async fn demands<T: QueueSql>(
    store: &T,
    now_ms: i64,
) -> Result<Vec<PreparationDemand>, StoreError> {
    if now_ms < 0 {
        return Err(StoreError::Task("invalid preparation time".into()));
    }
    let sql = format!(
        r#"
WITH hot AS ({HOT_ITEMS}), viewers AS (
 SELECT user_id, MAX(updated_at_ms) AS last_active FROM media_sessions
 WHERE state = 'active' AND lease_expires_at_ms > json_extract($1, '$.now_ms')
 GROUP BY user_id ORDER BY last_active DESC, user_id LIMIT 64
), channels AS (
 SELECT id,
 CASE WHEN pending_epoch_ms <= json_extract($1, '$.now_ms') THEN pending_generation_id ELSE active_generation_id END AS generation_id,
 CASE WHEN pending_epoch_ms <= json_extract($1, '$.now_ms') THEN pending_epoch_ms ELSE active_epoch_ms END AS epoch_ms
 FROM library_channels WHERE enabled = 1
 ORDER BY updated_at_ms DESC, id LIMIT 64
), positions AS (
 SELECT channels.*, generation.entry_count,
 (json_extract($1, '$.now_ms') - epoch_ms) % generation.loop_duration_ms AS offset_ms
 FROM channels JOIN library_channel_generations generation ON generation.id = channels.generation_id
 WHERE generation.state = 'ready' AND generation.loop_duration_ms > 0
   AND epoch_ms <= json_extract($1, '$.now_ms')
), next_entries AS (
 SELECT positions.id AS channel_id, positions.generation_id, next.file_id, next.item_id
 FROM positions JOIN library_channel_entries current ON current.generation_id = positions.generation_id
   AND current.cumulative_start_ms <= positions.offset_ms
   AND current.cumulative_start_ms + current.duration_ms > positions.offset_ms
 JOIN library_channel_entries next ON next.generation_id = positions.generation_id
   AND next.ordinal = (current.ordinal + 1) % positions.entry_count
 JOIN files file ON file.id = next.file_id AND file.size = next.file_size AND file.mtime = next.file_mtime
)
SELECT json_object('kind','item','item_id',item.id,'title',item.title) AS result_json
 FROM hot JOIN items item ON item.id = hot.item_id
UNION ALL SELECT json_object('kind','viewer','user_id',user_id) FROM viewers
UNION ALL SELECT json_object('kind','channel','channel_id',channel_id,'generation_id',generation_id,
 'file_id',file_id,'item_id',item_id,'title',item.title)
 FROM next_entries JOIN items item ON item.id = next_entries.item_id
"#
    );
    let rows = store
        .queue_sql(
            sql,
            encode(&serde_json::json!({"now_ms":now_ms}))?,
            false,
            true,
        )
        .await?;
    rows.iter().map(|row| decode(row)).collect()
}

pub(super) async fn hot_artifacts<T: QueueSql>(
    store: &T,
    now_ms: i64,
) -> Result<Vec<HotArtifact>, StoreError> {
    if now_ms < 0 {
        return Err(StoreError::Task("invalid artifact demand time".into()));
    }
    let sql = format!(
        r#"
WITH hot AS ({HOT_ITEMS}), copies AS (
 SELECT 'transcode:' || artifact.recipe_hash || ':' || artifact.manifest_digest AS artifact_key,
 location.node_id, location.bytes, hot.demanded_at_ms
 FROM hot JOIN files file ON file.item_id = hot.item_id
 JOIN background_transcode_artifacts artifact ON artifact.file_id = file.id
   AND artifact.source_size = file.size AND artifact.source_mtime = file.mtime
 JOIN transcode_cache_locations location ON location.recipe_hash = artifact.recipe_hash
   AND location.manifest_digest = artifact.manifest_digest
 WHERE location.complete = 1 AND location.storage_class = 'local' AND location.bytes > 0
 UNION ALL
 SELECT 'fragment:' || artifact.cache_key, location.node_id, artifact.bytes, hot.demanded_at_ms
 FROM hot JOIN files file ON file.item_id = hot.item_id
 JOIN cluster_fragment_index_artifacts artifact ON artifact.file_id = file.id
   AND artifact.source_size = file.size AND artifact.source_mtime = file.mtime
 JOIN cluster_fragment_index_locations location ON location.cache_key = artifact.cache_key
 WHERE location.verified_at_ms > json_extract($1, '$.now_ms') - 604800000
 UNION ALL
 SELECT 'artwork:' || location.artifact_key, location.node_id, location.bytes,
 (SELECT MAX(waiter.updated_at_ms) FROM background_job_waiters waiter
  WHERE waiter.consumer_kind = 'artwork' AND waiter.consumer_ref = location.artifact_key)
 FROM background_artwork_locations location
 WHERE location.verified_at_ms > json_extract($1, '$.now_ms') - 604800000
), eligible AS (
 SELECT * FROM copies WHERE demanded_at_ms > json_extract($1, '$.now_ms') - 86400000
 AND demanded_at_ms <= json_extract($1, '$.now_ms')
 AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || copies.node_id)
)
SELECT json_object('artifact_key', artifact_key, 'bytes', MAX(bytes), 'holders', json_group_array(DISTINCT node_id),
 'demanded_at_ms', MAX(demanded_at_ms), 'pending_copy', json(CASE WHEN EXISTS (
 SELECT 1 FROM background_jobs job JOIN background_job_waiters waiter ON waiter.job_id = job.id
 WHERE job.kind = 'artifact_hydrate' AND json_extract(job.payload_json, '$.artifact_key') = eligible.artifact_key
   AND job.state IN ('queued','running','cancelling') AND waiter.request_scope = 'automatic:hot-copy'
   AND waiter.state = 'pending' AND waiter.deadline_ms > json_extract($1, '$.now_ms')
) THEN 'true' ELSE 'false' END)) AS result_json
FROM eligible GROUP BY artifact_key ORDER BY MAX(demanded_at_ms) DESC, artifact_key LIMIT 128
"#
    );
    let rows = store
        .queue_sql(
            sql,
            encode(&serde_json::json!({"now_ms":now_ms}))?,
            false,
            true,
        )
        .await?;
    rows.iter().map(|row| decode(row)).collect()
}
