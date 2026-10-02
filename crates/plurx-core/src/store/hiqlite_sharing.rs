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
