//! Shared storage identities and all-or-none claim reservations.
use super::background_jobs::{decode, encode, identifier, QueueSql};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = include_str!("background_jobs_resources.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 74;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageDomainMapping {
    pub library_id: i64,
    pub root_path: String,
    pub domain_id: String,
}

pub fn validate(mappings: &[StorageDomainMapping]) -> Result<(), StoreError> {
    let mut roots = std::collections::BTreeSet::new();
    if mappings.len() > 256
        || mappings.iter().any(|mapping| {
            mapping.library_id <= 0
                || mapping.root_path.is_empty()
                || mapping.root_path.len() > 4096
                || mapping.root_path.contains('\0')
                || !identifier(&mapping.domain_id)
                || mapping.domain_id.len() > 64
                || !roots.insert((mapping.library_id, &mapping.root_path))
        })
    {
        return Err(StoreError::Task("invalid storage domains: at most 256 unique library roots and 64-byte domain identifiers".into()));
    }
    Ok(())
}

pub(super) async fn list<T: QueueSql>(store: &T) -> Result<Vec<StorageDomainMapping>, StoreError> {
    store.queue_sql("SELECT json_object('library_id', library_id, 'root_path', root_path, 'domain_id', domain_id) AS result_json FROM background_storage_domains ORDER BY library_id, root_path LIMIT json_extract($1, '$.limit')".into(), "{\"limit\":256}".into(), false, true).await?
        .iter().map(|row| decode(row)).collect()
}

pub(super) async fn replace<T: QueueSql>(
    store: &T,
    mappings: Vec<StorageDomainMapping>,
    now_ms: i64,
) -> Result<bool, StoreError> {
    validate(&mappings)?;
    if now_ms < 0 {
        return Err(StoreError::Task("invalid storage domain clock".into()));
    }
    let body = encode(
        &serde_json::json!({"mappings":mappings,"now_ms":now_ms,"id":uuid::Uuid::new_v4().to_string()}),
    )?;
    if body.len() > 64 * 1024 {
        return Err(StoreError::Task(
            "storage domain configuration exceeds 64 KiB".into(),
        ));
    }
    let rows = store
        .queue_sql(
            r#"
INSERT INTO background_job_commands(id, operation, request_json, result_json)
SELECT json_extract($1, '$.id'), 'storage_domains', $1,
    CASE WHEN NOT EXISTS (SELECT 1 FROM background_jobs
        WHERE state IN ('running','cancelling') AND lease_expires_ms > json_extract($1, '$.now_ms'))
      AND NOT EXISTS (SELECT 1 FROM analysis_source_reservations
        WHERE expires_at_ms > json_extract($1, '$.now_ms'))
      AND NOT EXISTS (SELECT 1 FROM json_each($1, '$.mappings') mapping
        WHERE NOT EXISTS (SELECT 1 FROM libraries library, json_each(library.paths) root
            WHERE library.id = json_extract(mapping.value, '$.library_id')
            AND root.value = json_extract(mapping.value, '$.root_path')))
      THEN 'true' ELSE 'false' END
RETURNING result_json
"#
            .into(),
            body,
            true,
            true,
        )
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing domain replacement verdict".into()))?,
    )
}
