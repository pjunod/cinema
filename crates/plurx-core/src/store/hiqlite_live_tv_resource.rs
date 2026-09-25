use super::hiqlite::{database_error, HiqliteAuthStore};
use crate::error::StoreError;
use crate::live_tv_resource::{Backend, Snapshot, Statement, Value, SNAPSHOT_SQL};
use async_trait::async_trait;
use hiqlite::{macros::params, Row};

struct Payload {
    value: String,
}
impl From<&mut Row<'_>> for Payload {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            value: row.get("payload"),
        }
    }
}
#[async_trait]
impl Backend for HiqliteAuthStore {
    async fn read_ledger(&self, user_id: i64, key: &str, now: i64) -> Result<Snapshot, StoreError> {
        let rows = self
            .client()
            .query_consistent_map::<Payload, _>(SNAPSHOT_SQL, params!(user_id, key, now))
            .await?;
        let payload = rows
            .first()
            .ok_or_else(|| StoreError::Database("Live TV revision row is missing".into()))?;
        serde_json::from_str(&payload.value)
            .map_err(|e| StoreError::Database(format!("reading Live TV authority: {e}")))
    }
    async fn commit_ledger(&self, statements: Vec<Statement>) -> Result<bool, StoreError> {
        let statements = statements.into_iter().map(|s| {
            (
                s.sql,
                s.values
                    .into_iter()
                    .map(|v| match v {
                        Value::Text(s) => hiqlite::Param::from(s),
                        Value::Integer(i) => hiqlite::Param::from(i),
                    })
                    .collect::<Vec<_>>(),
            )
        });
        let counts = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(counts.first() == Some(&1))
    }
}

pub(super) fn schema_statements() -> Vec<(String, hiqlite::Params)> {
    crate::live_tv_resource::SCHEMA
        .split(";\n")
        .map(|s| s.trim().trim_end_matches(';'))
        .filter(|s| !s.is_empty())
        .map(|s| (s.to_owned(), params!()))
        .collect()
}
