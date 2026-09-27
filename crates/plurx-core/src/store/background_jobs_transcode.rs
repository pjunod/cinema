//! Portable transcode copies retain their producer's source snapshot and manifest.
//! A holder row is only a candidate: every transfer verifies the manifest and bytes.
use super::background_jobs::{decode, digest, encode, QueueSql};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = include_str!("background_jobs_transcode.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 78;

pub fn artifact_key(recipe: &str, manifest: &str) -> String {
    format!("transcode:{recipe}:{manifest}")
}

pub fn parse_key(key: &str) -> Option<(&str, &str)> {
    let (recipe, manifest) = key.strip_prefix("transcode:")?.split_once(':')?;
    (digest(recipe) && digest(manifest)).then_some((recipe, manifest))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscodeCopySource {
    pub recipe_hash: String,
    pub manifest_digest: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub recipe_version: i64,
    pub built_by_node_id: String,
    pub built_at_ms: i64,
    pub node_id: String,
    pub relative_dir: String,
    pub bytes: i64,
}

pub(super) async fn sources<T: QueueSql>(
    store: &T,
    key: &str,
) -> Result<Vec<TranscodeCopySource>, StoreError> {
    let (recipe, manifest) =
        parse_key(key).ok_or_else(|| StoreError::Task("invalid transcode copy identity".into()))?;
    let rows = store.queue_sql(r#"
SELECT json_object('recipe_hash', artifact.recipe_hash, 'manifest_digest', artifact.manifest_digest,
    'file_id', artifact.file_id, 'source_size', artifact.source_size, 'source_mtime', artifact.source_mtime,
    'recipe_version', artifact.recipe_version, 'built_by_node_id', artifact.built_by_node_id,
    'built_at_ms', artifact.built_at_ms, 'node_id', location.node_id,
    'relative_dir', location.relative_dir, 'bytes', location.bytes) AS result_json
FROM background_transcode_artifacts artifact JOIN files file ON file.id = artifact.file_id
JOIN transcode_cache_locations location ON location.recipe_hash = artifact.recipe_hash
    AND location.manifest_digest = artifact.manifest_digest
WHERE artifact.recipe_hash = json_extract($1, '$.recipe') AND artifact.manifest_digest = json_extract($1, '$.manifest')
    AND file.size = artifact.source_size AND file.mtime = artifact.source_mtime
    AND location.complete = 1 AND location.storage_class = 'local' AND location.bytes > 0
    AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || location.node_id)
ORDER BY location.last_seen_at DESC, location.node_id LIMIT 64
"#.into(), encode(&serde_json::json!({"recipe":recipe,"manifest":manifest}))?, false, true).await?;
    rows.iter().map(|row| decode(row)).collect()
}
