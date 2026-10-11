use super::{
    hiqlite::HiqliteAuthStore,
    sharing::{Backend, Statement, Value},
};
use crate::error::StoreError;
use async_trait::async_trait;
fn values(values: Vec<Value>) -> hiqlite::Params {
    values
        .into_iter()
        .map(|v| match v {
            Value::Text(v) => hiqlite::Param::Text(v),
            Value::Integer(v) => hiqlite::Param::Integer(v),
        })
        .collect()
}
struct Payload(String);
impl From<&mut hiqlite::Row<'_>> for Payload {
    fn from(r: &mut hiqlite::Row<'_>) -> Self {
        Self(r.get("payload"))
    }
}
#[async_trait]
impl Backend for HiqliteAuthStore {
    fn sharing_is_replicated(&self) -> bool {
        true
    }
    async fn sharing_purpose_archive_rows(&self) -> Result<Vec<String>, StoreError> {
        // These are individual consistent reads, not one schema/data snapshot.
        // Candidate activation/rewrap must hold coordinated quiescence around
        // startup census and key selection. No runtime initializer exists here.
        let present=self.sharing_read("SELECT json_quote(CASE WHEN count(*)=0 THEN 0 WHEN count(*)=1 AND max(type)='table' THEN 1 ELSE 2 END) AS payload FROM sqlite_master WHERE name='sharing_purpose_key_archive'",vec![]).await?;
        match present.first().map(String::as_str) {
            Some("0") => return Ok(Vec::new()),
            Some("1") => {}
            _ => return Err(crate::sharing::invalid()),
        }
        super::sharing_purpose_keys::archive_columns(
            self.sharing_read(super::sharing_purpose_keys::ARCHIVE_COLUMNS_SQL, vec![])
                .await?,
        )?;
        self.sharing_read(super::sharing_purpose_keys::ARCHIVE_ROWS_SQL, vec![])
            .await
    }
    async fn sharing_revision_key_rows(&self) -> Result<Vec<String>, StoreError> {
        // These are individual consistent reads, not one schema/data snapshot.
        // Candidate activation/rewrap must hold coordinated quiescence around
        // startup census and key selection. No runtime initializer exists here.
        let present=self.sharing_read("SELECT json_quote(CASE WHEN count(*)=0 THEN 0 WHEN count(*)=1 AND max(type)='table' THEN 1 ELSE 2 END) AS payload FROM sqlite_master WHERE name='sharing_catalogue_keys'",vec![]).await?;
        match present.first().map(String::as_str) {
            Some("0") => return Ok(Vec::new()),
            Some("1") => {}
            _ => return Err(crate::sharing::invalid()),
        }
        super::sharing::revision_key_columns(
            self.sharing_read(super::sharing::REVISION_KEY_COLUMNS_SQL, vec![])
                .await?,
        )?;
        self.sharing_read(super::sharing::REVISION_KEY_ROWS_SQL, vec![])
            .await
    }
    async fn sharing_file_locator_key_rows(&self) -> Result<Vec<String>, StoreError> {
        // These are individual consistent reads, not one schema/data snapshot.
        // Candidate activation/rewrap must hold coordinated quiescence around
        // startup census and key selection. No runtime initializer exists here.
        let present=self.sharing_read("SELECT json_quote(CASE WHEN count(*)=0 THEN 0 WHEN count(*)=1 AND max(type)='table' THEN 1 ELSE 2 END) AS payload FROM sqlite_master WHERE name='sharing_file_locator_keys'",vec![]).await?;
        match present.first().map(String::as_str) {
            Some("0") => return Ok(Vec::new()),
            Some("1") => {}
            _ => return Err(crate::sharing::invalid()),
        }
        super::sharing_file_locators::columns(
            self.sharing_read(super::sharing_file_locators::COLUMNS_SQL, vec![])
                .await?,
        )?;
        self.sharing_read(super::sharing_file_locators::ROWS_SQL, vec![])
            .await
    }
    async fn sharing_read(&self, sql: &str, params: Vec<Value>) -> Result<Vec<String>, StoreError> {
        let (sql, params) = super::sharing::ordered(sql, params)?;
        super::hiqlite::validate_sql(&sql)?;
        Ok(self
            .client()
            // authority: grant and assignment revocations must be committed before access.
            .query_consistent_map::<Payload, _>(sql, values(params))
            .await?
            .into_iter()
            .map(|r| r.0)
            .collect())
    }
    async fn sharing_txn(&self, statements: Vec<Statement>) -> Result<Vec<usize>, StoreError> {
        let mut stmts = Vec::new();
        for (sql, params) in statements {
            let (sql, params) = super::sharing::ordered(&sql, params)?;
            super::hiqlite::validate_sql(&sql)?;
            stmts.push((sql, values(params)));
        }
        self.client()
            .txn(stmts)
            .await?
            .into_iter()
            .map(|r| r.map_err(super::hiqlite::database_error))
            .collect()
    }
}
