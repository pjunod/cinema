//! Hiqlite bridge. All mutations share the SQLite backend's exact SQL.

use async_trait::async_trait;
use hiqlite::macros::params;

use super::background_jobs::{QueueSql, SCHEMA};
use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use crate::error::StoreError;

struct JsonRow(String);

impl From<&mut hiqlite::Row<'_>> for JsonRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(row.get("result_json"))
    }
}

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    validate_sql(SCHEMA)?;
    for result in timeout_store(client.batch(SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    Ok(())
}

pub(super) async fn seal_legacy(client: &super::hiqlite::TimedClient) -> Result<(), StoreError> {
    let statements: Vec<(String, hiqlite::Params)> = SCHEMA
        .split("-- next statement\n")
        .filter(|sql| {
            sql.trim_start()
                .starts_with("INSERT INTO background_job_legacy")
                || sql
                    .trim_start()
                    .starts_with("INSERT INTO background_job_migration")
                || sql
                    .trim_start()
                    .starts_with("DELETE FROM background_job_legacy WHERE kind")
        })
        .map(|sql| (sql.to_owned(), params!()))
        .collect();
    for result in client.txn(statements).await? {
        result.map_err(database_error)?;
    }
    Ok(())
}

#[async_trait]
impl QueueSql for HiqliteAuthStore {
    async fn queue_transaction(&self, statements: Vec<(String, String)>) -> Result<(), StoreError> {
        let statements: Vec<(String, hiqlite::Params)> = statements
            .into_iter()
            .map(|(sql, request)| (sql, params!(request)))
            .collect();
        for result in self.client().txn(statements).await? {
            result.map_err(database_error)?;
        }
        Ok(())
    }

    async fn queue_sql(
        &self,
        sql: String,
        request: String,
        mutation: bool,
        authoritative: bool,
    ) -> Result<Vec<String>, StoreError> {
        if mutation {
            self.client()
                .execute_returning_map::<_, JsonRow>(sql, params!(request))
                .await?
                .into_iter()
                .map(|row| row.map(|row| row.0).map_err(database_error))
                .collect()
        } else {
            let rows = if authoritative {
                self.client()
                    // authority: claim reconciliation must observe committed ownership.
                    .query_consistent_map::<JsonRow, _>(sql, params!(request))
                    .await?
            } else {
                self.client()
                    .query_map::<JsonRow, _>(sql, params!(request))
                    .await?
            };
            Ok(rows.into_iter().map(|row| row.0).collect())
        }
    }
}
