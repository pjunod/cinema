//! SQLite bridge for the common queue's authoritative SQL transitions.

use async_trait::async_trait;

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::background_jobs::QueueSql;

#[async_trait]
impl QueueSql for SqliteStore {
    async fn queue_sql(
        &self,
        sql: String,
        request: String,
        _mutation: bool,
        _authoritative: bool,
    ) -> Result<Vec<String>, StoreError> {
        self.with_conn(move |connection| {
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map([request], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::from)
        })
        .await
    }
}
