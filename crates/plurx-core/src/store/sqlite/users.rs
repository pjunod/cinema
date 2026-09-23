//! Users and login tokens.

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::{user_from_row, SqliteStore, USER_COLS};
use crate::domain::User;
use crate::error::StoreError;
use crate::store::{
    bounded_device_label, CacheAdminMutationClaim, DeleteTokenByPrefixOutcome, TokenSummary,
    UserStore, MAX_DEVICE_LABEL_BYTES, TOKEN_SUMMARY_MAX,
};

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
        // Defence in depth behind the login admission check: whatever caller
        // reaches the Store, no row is written above the documented byte
        // bound, so the inventory projection's cap only ever has to deal with
        // rows written before this bound existed.
        let device = bounded_device_label(device.map(str::to_owned));
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
        let device = bounded_device_label(device.map(str::to_owned));
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

    async fn list_tokens_for_user(&self, user_id: i64) -> Result<Vec<TokenSummary>, StoreError> {
        self.with_conn(move |conn| {
            // `substr` counts characters, so `?2` characters is at most four
            // times that many bytes: the projection never materializes a whole
            // legacy label, and `bounded_device_label` then trims what is left
            // to the exact byte bound on a character boundary.
            let mut statement = conn.prepare(
                "SELECT substr(token_hash, 1, 8), substr(device, 1, ?2), \
                        created_at, last_seen_at \
                 FROM tokens WHERE user_id = ?1 \
                 ORDER BY created_at, token_hash LIMIT ?3",
            )?;
            let rows = statement.query_map(
                params![
                    user_id,
                    MAX_DEVICE_LABEL_BYTES as i64,
                    TOKEN_SUMMARY_MAX as i64
                ],
                |row| {
                    Ok(TokenSummary {
                        token_hash_prefix: row.get(0)?,
                        device: bounded_device_label(row.get(1)?),
                        created_at: row.get(2)?,
                        last_seen_at: row.get(3)?,
                    })
                },
            )?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn delete_token_by_prefix_for_user(
        &self,
        user_id: i64,
        prefix: &str,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<DeleteTokenByPrefixOutcome, StoreError> {
        require_standalone_claim(claim)?;
        let prefix = prefix.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let count: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM tokens \
                 WHERE user_id = ?1 AND substr(token_hash, 1, 8) = ?2",
                params![user_id, prefix],
                |row| row.get(0),
            )?;
            let outcome = match count {
                0 => DeleteTokenByPrefixOutcome::NotFound,
                1 => {
                    transaction.execute(
                        "DELETE FROM tokens \
                         WHERE user_id = ?1 AND substr(token_hash, 1, 8) = ?2",
                        params![user_id, prefix],
                    )?;
                    DeleteTokenByPrefixOutcome::Deleted
                }
                _ => DeleteTokenByPrefixOutcome::Ambiguous,
            };
            transaction.commit()?;
            Ok(outcome)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{DeleteTokenByPrefixOutcome, SqliteStore, UserStore};
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

    /// A device label is caller-chosen and caller-repeatable, so it is the one
    /// field of `tokens` that can turn a bounded 256-row inventory into an
    /// unbounded response. This pins both halves of the bound: new writes are
    /// capped at the Store boundary, and rows an older build already stored
    /// are capped where the inventory is projected rather than dropped.
    #[tokio::test]
    async fn device_inventory_bounds_label_bytes_at_the_write_and_at_the_projection() {
        use crate::store::MAX_DEVICE_LABEL_BYTES;

        let store = SqliteStore::open_in_memory().expect("open");
        let user = store
            .create_user("paul", "hash", true)
            .await
            .expect("create");

        // Rows an older build accepted: `/auth/login` inherited axum's 2 MiB
        // default body limit and nothing bounded this column.
        let ascii = "A".repeat(64 * 1024);
        let multibyte = "\u{e9}".repeat(64 * 1024);
        let (legacy_ascii, legacy_multibyte, owner) = (ascii.clone(), multibyte.clone(), user.id);
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO tokens (token_hash, user_id, device) VALUES (?1, ?2, ?3)",
                    rusqlite::params![format!("aaaaaaaa{}", "1".repeat(56)), owner, legacy_ascii],
                )?;
                conn.execute(
                    "INSERT INTO tokens (token_hash, user_id, device) VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        format!("bbbbbbbb{}", "2".repeat(56)),
                        owner,
                        legacy_multibyte
                    ],
                )?;
                Ok(())
            })
            .await
            .expect("legacy rows");

        // A new write over the bound is capped before it reaches the column,
        // so the projection only ever has to repair pre-bound history.
        store
            .create_token(
                &format!("cccccccc{}", "3".repeat(56)),
                user.id,
                Some(&"Z".repeat(4 * 1024)),
            )
            .await
            .expect("oversized write");
        let stored: usize = store
            .with_conn(move |conn| {
                Ok(conn.query_row(
                    "SELECT length(CAST(device AS BLOB)) FROM tokens \
                     WHERE substr(token_hash, 1, 8) = ?1",
                    rusqlite::params!["cccccccc"],
                    |row| row.get::<_, i64>(0),
                )? as usize)
            })
            .await
            .expect("stored width");
        assert_eq!(
            stored, MAX_DEVICE_LABEL_BYTES,
            "the Store boundary must cap a new label at the write"
        );

        let listed = store.list_tokens_for_user(user.id).await.expect("list");
        assert_eq!(listed.len(), 3);
        let label = |prefix: &str| {
            listed
                .iter()
                .find(|row| row.token_hash_prefix == prefix)
                .expect("row")
                .device
                .clone()
                .expect("label")
        };
        for row in &listed {
            let device = row.device.as_deref().expect("label");
            assert!(
                device.len() <= MAX_DEVICE_LABEL_BYTES,
                "projected {} bytes for prefix {}",
                device.len(),
                row.token_hash_prefix
            );
        }
        // Truncated, never dropped: a device the user cannot see is a device
        // the user cannot revoke.
        let projected_ascii = label("aaaaaaaa");
        assert_eq!(projected_ascii.len(), MAX_DEVICE_LABEL_BYTES);
        assert!(ascii.starts_with(&projected_ascii));
        // Truncation lands on a character boundary, so a two-byte-per-char
        // label still comes back as valid UTF-8 at exactly the bound.
        let projected_multibyte = label("bbbbbbbb");
        assert_eq!(projected_multibyte.len(), MAX_DEVICE_LABEL_BYTES);
        assert!(multibyte.starts_with(&projected_multibyte));

        // The whole inventory body is now rows x bytes, not rows x whatever
        // the request body limit allowed.
        let serialized = serde_json::to_string(&listed).expect("json");
        assert!(
            serialized.len() <= listed.len() * (MAX_DEVICE_LABEL_BYTES + 256),
            "inventory serialized to {} bytes",
            serialized.len()
        );
    }

    #[tokio::test]
    async fn device_inventory_never_exposes_a_full_digest_and_delete_requires_a_unique_prefix() {
        let store = SqliteStore::open_in_memory().expect("open");
        let user = store
            .create_user("paul", "hash", true)
            .await
            .expect("create");
        let first = format!("deadbeef{}", "1".repeat(56));
        let collision = format!("deadbeef{}", "2".repeat(56));
        let unique = format!("cafebabe{}", "3".repeat(56));
        store
            .create_token(&first, user.id, Some("Living room"))
            .await
            .expect("first token");
        store
            .create_token(&collision, user.id, Some("Tablet"))
            .await
            .expect("collision token");
        store
            .create_token(&unique, user.id, None)
            .await
            .expect("unique token");

        let listed = store.list_tokens_for_user(user.id).await.expect("list");
        assert_eq!(listed.len(), 3);
        assert!(listed
            .iter()
            .all(|token| token.token_hash_prefix.len() == 8));
        assert!(!format!("{listed:?}").contains(&"1".repeat(56)));
        assert_eq!(
            store
                .delete_token_by_prefix_for_user(user.id, "deadbeef", None)
                .await
                .expect("ambiguous delete"),
            DeleteTokenByPrefixOutcome::Ambiguous
        );
        assert_eq!(
            store
                .delete_token_by_prefix_for_user(user.id, "cafebabe", None)
                .await
                .expect("unique delete"),
            DeleteTokenByPrefixOutcome::Deleted
        );
        assert!(store
            .user_for_token(&unique)
            .await
            .expect("lookup")
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
