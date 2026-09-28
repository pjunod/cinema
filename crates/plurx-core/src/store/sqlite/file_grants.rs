use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::{FileGrant, FileGrantStore, NewFileGrant};

#[async_trait]
impl FileGrantStore for SqliteStore {
    async fn create_file_grant(&self, grant: NewFileGrant) -> Result<(), StoreError> {
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO file_grants
                 (id, token_hash, file_id, user_id, source_token_hash, purpose, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'open_in', ?6, ?7)",
                params![grant.id, grant.token_hash, grant.file_id, grant.user_id, grant.source_token_hash, grant.created_at, grant.expires_at],
            )?;
            Ok(())
        })
        .await
    }

    async fn file_grant_by_hash(&self, token_hash: &str) -> Result<Option<FileGrant>, StoreError> {
        let token_hash = token_hash.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT g.id, g.file_id, g.user_id, g.expires_at, g.revoked_at,
                            EXISTS (SELECT 1 FROM tokens t WHERE t.token_hash = g.source_token_hash
                                      AND t.user_id = g.user_id)
                     FROM file_grants g WHERE g.token_hash = ?1 AND g.purpose = 'open_in'",
                    params![token_hash],
                    |row| {
                        Ok(FileGrant {
                            id: row.get(0)?,
                            file_id: row.get(1)?,
                            user_id: row.get(2)?,
                            expires_at: row.get(3)?,
                            revoked_at: row.get(4)?,
                            source_active: row.get::<_, i64>(5)? != 0,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    async fn revoke_file_grant(
        &self,
        id: &str,
        user_id: i64,
        now: i64,
    ) -> Result<bool, StoreError> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE file_grants SET revoked_at = ?1
                 WHERE id = ?2 AND user_id = ?3 AND revoked_at IS NULL",
                params![now, id, user_id],
            )? > 0)
        })
        .await
    }

    async fn revoke_file_grants_for_user(&self, user_id: i64, now: i64) -> Result<(), StoreError> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE file_grants SET revoked_at = ?1
                 WHERE user_id = ?2 AND revoked_at IS NULL",
                params![now, user_id],
            )?;
            Ok(())
        })
        .await
    }

    async fn prune_file_grants(&self, before: i64) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM file_grants WHERE expires_at < ?1",
                params![before],
            )? as u64)
        })
        .await
    }
}
