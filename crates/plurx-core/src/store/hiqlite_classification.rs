use super::{
    classification::*,
    hiqlite::{database_error, HiqliteAuthStore},
};
use crate::error::StoreError;
use async_trait::async_trait;
use hiqlite::macros::params;
struct Payload(String);
struct Due(i64);
impl From<&mut hiqlite::Row<'_>> for Due {
    fn from(r: &mut hiqlite::Row<'_>) -> Self {
        Self(r.get("due"))
    }
}
impl From<&mut hiqlite::Row<'_>> for Payload {
    fn from(r: &mut hiqlite::Row<'_>) -> Self {
        Self(r.get("payload"))
    }
}
#[async_trait]
impl ClassificationStore for HiqliteAuthStore {
    async fn classification_page(&self, after: i64, limit: i64) -> Result<Vec<Entry>, StoreError> {
        self.client()
            .query_consistent_map::<Payload, _>(
                inventory_sql(),
                params!(after, limit.clamp(1, 256)),
            )
            .await?
            .into_iter()
            .map(|r| decode(&r.0))
            .collect()
    }
    async fn write_classification(&self, id: i64, r: &Record) -> Result<bool, StoreError> {
        Ok(self
            .client()
            .execute(
                write_sql(),
                params!(
                    id,
                    r.source_json.clone(),
                    serde_json::to_string(&r.classification).map_err(database_error)?,
                    serde_json::to_string(&r.overrides).map_err(database_error)?,
                    terms(r),
                    r.revision
                ),
            )
            .await?
            == 1)
    }
    async fn classification_hint(&self, now_unix: i64) -> Result<bool, StoreError> {
        // `query_map` reads this node's replica: no leader round trip and no
        // proposal. See the trait method for why a stale answer is safe.
        let rows = self
            .client()
            .query_map::<Due, _>(
                hint_sql(),
                params!(crate::metadata::classification::VERSION, now_unix),
            )
            .await?;
        Ok(rows.first().is_some_and(|due| due.0 != 0))
    }
}
pub(super) fn migration_statements() -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    super::hiqlite_library_channels::split_schema_statements(SCHEMA)
        .into_iter()
        .map(|s| {
            super::hiqlite::validate_sql(&s)?;
            Ok((s, params!()))
        })
        .collect()
}
