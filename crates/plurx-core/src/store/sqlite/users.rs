//! Users and login tokens.

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::{user_from_row, SqliteStore, USER_COLS};
use crate::domain::User;
use crate::error::StoreError;
use crate::store::{CacheAdminMutationClaim, UserStore};

fn require_standalone_claim(claim: Option<&CacheAdminMutationClaim>) -> Result<(), StoreError> {
    if claim.is_some() {
        return Err(StoreError::Database(
            "cluster cache-admin mutation claim cannot be used by standalone SQLite".to_owned(),
        ));
    }
    Ok(())
}

#[async_trait]
impl UserStore for SqliteStore {
    async fn count_users(&self) -> Result<i64, StoreError> {
        self.with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?)
        })
        .await
    }

    async fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_admin: bool,
    ) -> Result<User, StoreError> {
        let username = username.to_owned();
        let password_hash = password_hash.to_owned();
        self.with_conn(move |conn| {
            let user = conn.query_row(
                &format!(
                    "INSERT INTO users (username, password_hash, is_admin)
                     VALUES (?1, ?2, ?3) RETURNING {USER_COLS}"
                ),
                params![username, password_hash, is_admin as i64],
                user_from_row,
            )?;
            Ok(user)
        })
        .await
    }

    async fn get_user(&self, id: i64) -> Result<Option<User>, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {USER_COLS} FROM users WHERE id = ?1"),
                    params![id],
                    user_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn get_user_by_username(&self, username: &str) -> Result<Option<User>, StoreError> {
        let username = username.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {USER_COLS} FROM users WHERE username = ?1"),
                    params![username],
                    user_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare(&format!("SELECT {USER_COLS} FROM users ORDER BY username"))?;
            let users = stmt
                .query_map([], user_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(users)
        })
        .await
    }

    async fn list_users_page(&self, after_id: i64, limit: i64) -> Result<Vec<User>, StoreError> {
        if after_id < 0 || !(1..=256).contains(&limit) {
            return Err(StoreError::Task("invalid bounded user page".to_owned()));
        }
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {USER_COLS} FROM users WHERE id > ?1 ORDER BY id LIMIT ?2"
            ))?;
            let users = stmt
                .query_map(params![after_id, limit], user_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(users)
        })
        .await
    }

    async fn delete_user(&self, id: i64) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute("DELETE FROM users WHERE id = ?1", params![id])? > 0)
        })
        .await
    }

    async fn delete_user_preserving_admin(
        &self,
        id: i64,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError> {
        require_standalone_claim(claim)?;
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM users
                 WHERE id = ?1
                   AND (is_admin = 0 OR EXISTS (
                     SELECT 1 FROM users AS other
                     WHERE other.is_admin = 1 AND other.id != ?1
                   ))",
                params![id],
            )? > 0)
        })
        .await
    }

    async fn count_admins(&self) -> Result<i64, StoreError> {
        self.with_conn(|conn| {
            Ok(
                conn.query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
    }

    async fn set_password(&self, id: i64, password_hash: &str) -> Result<bool, StoreError> {
        let password_hash = password_hash.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE users SET password_hash = ?2 WHERE id = ?1",
                params![id, password_hash],
            )? > 0)
        })
        .await
    }

    async fn reset_password_and_revoke_tokens(
        &self,
        id: i64,
        password_hash: &str,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError> {
        require_standalone_claim(claim)?;
        let password_hash = password_hash.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE users SET password_hash = ?2 WHERE id = ?1",
                params![id, password_hash],
            )?;
            transaction.execute("DELETE FROM tokens WHERE user_id = ?1", params![id])?;
            transaction.commit()?;
            Ok(changed > 0)
        })
        .await
    }

    async fn promote_user_and_reset_password(
        &self,
        id: i64,
        password_hash: &str,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError> {
        require_standalone_claim(claim)?;
        let password_hash = password_hash.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE users SET password_hash = ?2, is_admin = 1 WHERE id = ?1",
                params![id, password_hash],
            )?;
            transaction.execute("DELETE FROM tokens WHERE user_id = ?1", params![id])?;
            transaction.commit()?;
            Ok(changed > 0)
        })
        .await
    }

    async fn set_admin(&self, id: i64, is_admin: bool) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE users SET is_admin = ?2 WHERE id = ?1",
                params![id, is_admin as i64],
            )? > 0)
        })
        .await
    }

    async fn demote_user_preserving_admin(
        &self,
        id: i64,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError> {
        require_standalone_claim(claim)?;
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE users SET is_admin = 0
                 WHERE id = ?1
                   AND (is_admin = 0 OR EXISTS (
                     SELECT 1 FROM users AS other
                     WHERE other.is_admin = 1 AND other.id != ?1
                   ))",
                params![id],
            )? > 0)
        })
        .await
    }

    async fn delete_tokens_for_user(&self, user_id: i64) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute("DELETE FROM tokens WHERE user_id = ?1", params![user_id])? as u64)
        })
        .await
    }

    async fn create_token(
        &self,
        token_hash: &str,
        user_id: i64,
        device: Option<&str>,
    ) -> Result<(), StoreError> {
        let token_hash = token_hash.to_owned();
        let device = device.map(str::to_owned);
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO tokens (token_hash, user_id, device) VALUES (?1, ?2, ?3)",
                params![token_hash, user_id, device],
            )?;
            Ok(())
        })
        .await
    }

    async fn create_token_if_password_matches(
        &self,
        token_hash: &str,
        user_id: i64,
        device: Option<&str>,
        expected_password_hash: &str,
    ) -> Result<bool, StoreError> {
        let token_hash = token_hash.to_owned();
        let device = device.map(str::to_owned);
        let expected_password_hash = expected_password_hash.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "INSERT INTO tokens (token_hash, user_id, device) \
                 SELECT ?1, id, ?2 FROM users \
                 WHERE id = ?3 AND password_hash = ?4",
                params![token_hash, device, user_id, expected_password_hash],
            )? > 0)
        })
        .await
    }

    async fn user_for_token(&self, token_hash: &str) -> Result<Option<User>, StoreError> {
        let token_hash = token_hash.to_owned();
        self.with_conn(move |conn| {
            let user = conn
                .query_row(
                    &format!(
                        "SELECT {cols} FROM users u
                         JOIN tokens t ON t.user_id = u.id
                         WHERE t.token_hash = ?1",
                        cols = "u.id, u.username, u.password_hash, u.is_admin, u.created_at"
                    ),
                    params![token_hash],
                    user_from_row,
                )
                .optional()?;
            if user.is_some() {
                // Touch at most once a minute to keep write volume trivial.
                conn.execute(
                    "UPDATE tokens SET last_seen_at = unixepoch()
                     WHERE token_hash = ?1 AND last_seen_at < unixepoch() - 60",
                    params![token_hash],
                )?;
            }
            Ok(user)
        })
        .await
    }

    async fn delete_token(&self, token_hash: &str) -> Result<bool, StoreError> {
        let token_hash = token_hash.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM tokens WHERE token_hash = ?1",
                params![token_hash],
            )? > 0)
        })
        .await
    }

    async fn delete_token_with_cache_admin_claim(
        &self,
        token_hash: &str,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError> {
        require_standalone_claim(claim)?;
        self.delete_token(token_hash).await
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{SqliteStore, UserStore};
    use std::sync::Arc;

    #[tokio::test]
    async fn user_and_token_lifecycle() {
        let store = SqliteStore::open_in_memory().expect("open");
        assert_eq!(store.count_users().await.expect("count"), 0);

        let user = store
            .create_user("paul", "hash", true)
            .await
            .expect("create");
        assert!(user.is_admin);
        assert_eq!(store.count_users().await.expect("count"), 1);

        // Username lookup is case-insensitive.
        let found = store
            .get_user_by_username("PAUL")
            .await
            .expect("lookup")
            .expect("present");
        assert_eq!(found.id, user.id);

        store
            .create_token("th_abc", user.id, Some("test"))
            .await
            .expect("token");
        let via_token = store
            .user_for_token("th_abc")
            .await
            .expect("resolve")
            .expect("present");
        assert_eq!(via_token.id, user.id);
        assert!(store.delete_token("th_abc").await.expect("del"));
        assert!(store
            .user_for_token("th_abc")
            .await
            .expect("resolve")
            .is_none());

        // Deleting the user cascades to tokens.
        store
            .create_token("th_2", user.id, None)
            .await
            .expect("token");
        assert!(store.delete_user(user.id).await.expect("del user"));
        assert!(store
            .user_for_token("th_2")
            .await
            .expect("resolve")
            .is_none());
    }

    #[tokio::test]
    async fn password_and_session_revocation_roll_back_together_on_delete_failure() {
        let store = SqliteStore::open_in_memory().expect("open");
        let user = store
            .create_user("paul", "old-hash", true)
            .await
            .expect("create");
        store
            .create_token("old-session", user.id, None)
            .await
            .expect("token");
        store
            .with_conn(|conn| {
                conn.execute_batch(
                    "CREATE TEMP TRIGGER reject_token_revocation
                     BEFORE DELETE ON tokens
                     BEGIN
                       SELECT RAISE(ABORT, 'injected token revocation failure');
                     END;",
                )?;
                Ok(())
            })
            .await
            .expect("install fault");

        store
            .reset_password_and_revoke_tokens(user.id, "new-hash", None)
            .await
            .expect_err("token failure must abort the password transaction");
        assert_eq!(
            store
                .get_user(user.id)
                .await
                .expect("user query")
                .expect("user")
                .password_hash,
            "old-hash"
        );
        assert!(store
            .user_for_token("old-session")
            .await
            .expect("session query")
            .is_some());
    }

    #[tokio::test]
    async fn failed_combined_promotion_never_authorizes_the_old_session() {
        let store = SqliteStore::open_in_memory().expect("open");
        let user = store
            .create_user("viewer", "old-hash", false)
            .await
            .expect("create");
        store
            .create_token("viewer-session", user.id, None)
            .await
            .expect("token");
        store
            .with_conn(|conn| {
                conn.execute_batch(
                    "CREATE TEMP TRIGGER reject_combined_token_revocation
                     BEFORE DELETE ON tokens
                     BEGIN
                       SELECT RAISE(ABORT, 'injected combined update failure');
                     END;",
                )?;
                Ok(())
            })
            .await
            .expect("install fault");

        store
            .promote_user_and_reset_password(user.id, "new-hash", None)
            .await
            .expect_err("token failure must abort promotion and password reset");
        let session_user = store
            .user_for_token("viewer-session")
            .await
            .expect("session query")
            .expect("old session remains a viewer");
        assert!(!session_user.is_admin);
        assert_eq!(session_user.password_hash, "old-hash");
    }

    #[tokio::test]
    async fn concurrent_admin_demote_and_delete_preserve_one_administrator() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let first = store
            .create_user("first", "hash", true)
            .await
            .expect("first admin");
        let second = store
            .create_user("second", "hash", true)
            .await
            .expect("second admin");

        let (first_demote, second_demote) = tokio::join!(
            store.demote_user_preserving_admin(first.id, None),
            store.demote_user_preserving_admin(second.id, None),
        );
        assert_ne!(
            first_demote.expect("first demotion"),
            second_demote.expect("second demotion"),
            "exactly one concurrent demotion must commit"
        );
        assert_eq!(store.count_admins().await.expect("admin count"), 1);

        store
            .set_admin(first.id, true)
            .await
            .expect("restore first admin");
        store
            .set_admin(second.id, true)
            .await
            .expect("restore second admin");
        assert_eq!(store.count_admins().await.expect("admin count"), 2);

        let (first_delete, second_delete) = tokio::join!(
            store.delete_user_preserving_admin(first.id, None),
            store.delete_user_preserving_admin(second.id, None),
        );
        assert_ne!(
            first_delete.expect("first delete"),
            second_delete.expect("second delete"),
            "exactly one concurrent delete must commit"
        );
        assert_eq!(store.count_users().await.expect("user count"), 1);
        assert_eq!(store.count_admins().await.expect("admin count"), 1);
    }
}
