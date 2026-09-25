//! Fixed-lifetime capabilities for opening one book in an external reader.

use async_trait::async_trait;

use crate::error::StoreError;

pub const FILE_GRANTS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS file_grants (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose = 'open_in'),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER
) STRICT;
CREATE INDEX IF NOT EXISTS file_grants_expiry ON file_grants(expires_at);
CREATE INDEX IF NOT EXISTS file_grants_user_live ON file_grants(user_id, revoked_at);";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileGrant {
    pub id: String,
    pub file_id: i64,
    pub user_id: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

#[async_trait]
pub trait FileGrantStore: Send + Sync {
    async fn create_file_grant(
        &self,
        id: &str,
        token_hash: &str,
        file_id: i64,
        user_id: i64,
        created_at: i64,
        expires_at: i64,
    ) -> Result<(), StoreError>;
    async fn file_grant_by_hash(&self, token_hash: &str) -> Result<Option<FileGrant>, StoreError>;
    async fn revoke_file_grant(&self, id: &str, user_id: i64, now: i64)
        -> Result<bool, StoreError>;
    async fn revoke_file_grants_for_user(&self, user_id: i64, now: i64) -> Result<(), StoreError>;
    async fn prune_file_grants(&self, before: i64) -> Result<u64, StoreError>;
}
