//! Users and login tokens.

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::{user_from_row, SqliteStore, USER_COLS};
use crate::auth::TokenIdlePolicy;
use crate::domain::User;
use crate::error::StoreError;
use crate::store::{
    bounded_device_label, keys, CacheAdminMutationClaim, DeleteTokenByPrefixOutcome,
    TokenAuthentication, TokenSummary, UserStore, MAX_DEVICE_LABEL_BYTES, TOKEN_SUMMARY_MAX,
};

fn require_standalone_claim(claim: Option<&CacheAdminMutationClaim>) -> Result<(), StoreError> {
    if claim.is_some() {
        return Err(StoreError::Database(
            "cluster cache-admin mutation claim cannot be used by standalone SQLite".to_owned(),
        ));
    }
    Ok(())
}

/// The standalone twin of the replicated store's activity gate: one
/// `last_seen_at` refresh per token per `ACTIVITY_REFRESH_SECS` in this
/// process. A reservation is kept only when its write committed; a failed
/// write releases it so the next request retries.
#[derive(Default)]
pub(super) struct TokenActivityGate {
    reservations: std::sync::Mutex<std::collections::HashMap<String, i64>>,
}

pub(super) struct TokenActivityReservation<'a> {
    gate: &'a TokenActivityGate,
    token_hash: String,
    reserved_at: i64,
    retained: bool,
}

impl TokenActivityGate {
    fn try_reserve(&self, token_hash: &str, now: i64) -> Option<TokenActivityReservation<'_>> {
        let mut reservations = self
            .reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Expired reservations go on every admission, so the map holds at
        // most the tokens refreshed within the last window.
        reservations
            .retain(|_, reserved_at| !crate::auth::activity_refresh_due(Some(*reserved_at), now));
        if reservations.contains_key(token_hash) {
            return None;
        }
        reservations.insert(token_hash.to_owned(), now);
        Some(TokenActivityReservation {
            gate: self,
            token_hash: token_hash.to_owned(),
            reserved_at: now,
            retained: false,
        })
    }

    fn forget(&self, token_hash: &str) {
        self.reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(token_hash);
    }

    #[cfg(test)]
    fn is_reserved(&self, token_hash: &str) -> bool {
        self.reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(token_hash)
    }
}

impl TokenActivityReservation<'_> {
    fn retain(mut self) {
        self.retained = true;
    }
}

impl Drop for TokenActivityReservation<'_> {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        let mut reservations = self
            .gate
            .reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if reservations.get(&self.token_hash) == Some(&self.reserved_at) {
            reservations.remove(&self.token_hash);
        }
    }
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

    async fn authenticate_token(
        &self,
        token_hash: &str,
    ) -> Result<TokenAuthentication, StoreError> {
        let token_hash = token_hash.to_owned();
        let read_hash = token_hash.clone();
        // The read is on the read pool: an authenticated request no longer
        // takes the writer mutex, and inside the activity window it takes
        // nothing else either (K-05 section 3.2).
        let found = self
            .with_read(move |conn| {
                // The expiry policy is read in the same statement as the
                // token, so a request is judged against one snapshot of both.
                const SQL: &str =
                    "SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at,
                            t.last_seen_at,
                            (SELECT value FROM settings WHERE key = ?2),
                            (SELECT value FROM settings WHERE key = ?3),
                            (SELECT value FROM settings WHERE key = ?4),
                            unixepoch()
                     FROM users u
                     JOIN tokens t ON t.user_id = u.id
                     WHERE t.token_hash = ?1";
                super::trace_statement("authenticate_token", SQL);
                Ok(conn
                    .query_row(
                        SQL,
                        params![
                            read_hash,
                            keys::AUTH_TOKEN_EXPIRY_ENABLED,
                            keys::AUTH_TOKEN_IDLE_DAYS,
                            keys::AUTH_TOKEN_EXPIRY_SINCE
                        ],
                        |row| {
                            Ok((
                                user_from_row(row)?,
                                row.get::<_, i64>(5)?,
                                TokenIdlePolicy::from_settings(
                                    row.get::<_, Option<String>>(6)?.as_deref(),
                                    row.get::<_, Option<String>>(7)?.as_deref(),
                                    row.get::<_, Option<String>>(8)?.as_deref(),
                                ),
                                row.get::<_, i64>(9)?,
                            ))
                        },
                    )
                    .optional()?)
            })
            .await?;
        let Some((user, last_seen_at, policy, now)) = found else {
            return Ok(TokenAuthentication::Unknown);
        };
        if let Some(policy) = policy.filter(|policy| policy.is_expired(last_seen_at, now)) {
            // No touch: refreshing an expired token would revive it.
            return Ok(TokenAuthentication::Expired {
                idle_days: policy.idle_days,
            });
        }
        // Touch at most once a minute to keep write volume trivial, and at
        // most once per process: the gate admits one refresh per token per
        // window, and the predicate is the final guard across processes and
        // against a delete that landed after the read. An UPDATE cannot
        // resurrect a deleted row, so a token revoked between the read and
        // this write stays revoked; this request, which read it live, is
        // served, exactly as when the delete landed just after the read did.
        if crate::auth::activity_refresh_due(Some(last_seen_at), now) {
            if let Some(reservation) = self.token_activity.try_reserve(&token_hash, now) {
                self.with_conn(move |conn| {
                    conn.execute(
                        "UPDATE tokens SET last_seen_at = ?2
                         WHERE token_hash = ?1 AND last_seen_at < ?3",
                        params![
                            token_hash,
                            now,
                            now.saturating_sub(crate::auth::ACTIVITY_REFRESH_SECS)
                        ],
                    )?;
                    Ok(())
                })
                .await?;
                reservation.retain();
            }
        }
        Ok(TokenAuthentication::Authenticated(user))
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

/// Test fixture: move a login token's activity timestamp, so sign-in expiry
/// can be exercised without waiting out a 90-day window. Compiled only for
/// tests and the `fixtures` feature the plurxd test suite enables.
#[cfg(any(test, feature = "fixtures"))]
impl SqliteStore {
    pub async fn fixture_set_token_last_seen(
        &self,
        token_hash: &str,
        last_seen_at: i64,
    ) -> Result<bool, StoreError> {
        // Moving the clock for a token also ends this process's memory of
        // having refreshed it, as the passage of that much time would.
        self.token_activity.forget(token_hash);
        let token_hash = token_hash.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE tokens SET last_seen_at = ?2 WHERE token_hash = ?1",
                params![token_hash, last_seen_at],
            )? > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{DeleteTokenByPrefixOutcome, SqliteStore, TokenAuthentication, UserStore};
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

    const DAY: i64 = 86_400;

    async fn sql_now(store: &SqliteStore) -> i64 {
        store
            .with_conn(|conn| Ok(conn.query_row("SELECT unixepoch()", [], |row| row.get(0))?))
            .await
            .expect("sqlite clock")
    }

    async fn last_seen(store: &SqliteStore, token_hash: &'static str) -> i64 {
        store
            .with_conn(move |conn| {
                Ok(conn.query_row(
                    "SELECT last_seen_at FROM tokens WHERE token_hash = ?1",
                    rusqlite::params![token_hash],
                    |row| row.get(0),
                )?)
            })
            .await
            .expect("last_seen_at")
    }

    async fn expiry(store: &SqliteStore, enabled: &str, idle_days: &str, since: i64) {
        use crate::store::{keys, SettingsStore};
        store
            .put_settings(&[
                (keys::AUTH_TOKEN_EXPIRY_ENABLED, enabled),
                (keys::AUTH_TOKEN_IDLE_DAYS, idle_days),
                (keys::AUTH_TOKEN_EXPIRY_SINCE, &since.to_string()),
            ])
            .await
            .expect("expiry policy");
    }

    fn verdict(outcome: crate::store::TokenAuthentication) -> Result<i64, Option<i64>> {
        use crate::store::TokenAuthentication;
        match outcome {
            TokenAuthentication::Authenticated(user) => Ok(user.id),
            TokenAuthentication::Expired { idle_days } => Err(Some(idle_days)),
            TokenAuthentication::Unknown => Err(None),
        }
    }

    /// The whole policy against the real SQLite statement: off never expires,
    /// on rejects a token idle past the window without reviving it, use
    /// inside the window slides it, no clock starts before `since`, and an
    /// explicit revocation still removes the row.
    #[tokio::test]
    async fn sign_in_expiry_is_a_sliding_idle_window_that_never_starts_before_it_took_effect() {
        let store = SqliteStore::open_in_memory().expect("open");
        let user = store
            .create_user("paul", "hash", true)
            .await
            .expect("create");
        store
            .create_token("th_idle", user.id, Some("Living room"))
            .await
            .expect("token");
        let now = sql_now(&store).await;

        // Off: a token untouched since 1970 still authenticates.
        expiry(&store, "0", "90", now - 400 * DAY).await;
        assert!(store
            .fixture_set_token_last_seen("th_idle", 1)
            .await
            .expect("age"));
        assert_eq!(
            verdict(store.authenticate_token("th_idle").await.expect("auth")),
            Ok(user.id),
            "expiry off must keep today's non-expiring behaviour"
        );

        // On, idle past the window: a distinct verdict carrying the window,
        // and the activity timestamp is left exactly where it was — touching
        // it would slide an expired token back to life.
        expiry(&store, "1", "90", now - 400 * DAY).await;
        store
            .fixture_set_token_last_seen("th_idle", now - 91 * DAY)
            .await
            .expect("age");
        for _ in 0..3 {
            assert_eq!(
                verdict(store.authenticate_token("th_idle").await.expect("auth")),
                Err(Some(90))
            );
        }
        assert_eq!(last_seen(&store, "th_idle").await, now - 91 * DAY);
        assert!(
            store
                .user_for_token("th_idle")
                .await
                .expect("resolve")
                .is_none(),
            "callers that predate expiry must honour it too"
        );

        // Used inside the window: accepted, and the window slides to now.
        store
            .fixture_set_token_last_seen("th_idle", now - 89 * DAY)
            .await
            .expect("age");
        assert_eq!(
            verdict(store.authenticate_token("th_idle").await.expect("auth")),
            Ok(user.id)
        );
        assert!(last_seen(&store, "th_idle").await >= now);

        // The clock starts when expiry took effect: a device last seen long
        // before is not signed out on the spot, only once it stays idle for a
        // whole window after that moment.
        store
            .fixture_set_token_last_seen("th_idle", 1)
            .await
            .expect("age");
        expiry(&store, "1", "90", now - DAY).await;
        assert_eq!(
            verdict(store.authenticate_token("th_idle").await.expect("auth")),
            Ok(user.id),
            "turning expiry on must not sign an old device out at once"
        );
        store
            .fixture_set_token_last_seen("th_idle", 1)
            .await
            .expect("age");
        expiry(&store, "1", "90", now - 90 * DAY).await;
        assert_eq!(
            verdict(store.authenticate_token("th_idle").await.expect("auth")),
            Err(Some(90))
        );

        // An absent start (the seed has not run) cannot run out.
        store
            .with_conn(|conn| {
                conn.execute(
                    "DELETE FROM settings WHERE key = ?1",
                    rusqlite::params![crate::store::keys::AUTH_TOKEN_EXPIRY_SINCE],
                )?;
                Ok(())
            })
            .await
            .expect("drop since");
        assert_eq!(
            verdict(store.authenticate_token("th_idle").await.expect("auth")),
            Ok(user.id)
        );

        // Revocation is unchanged by any of it.
        assert!(store.delete_token("th_idle").await.expect("revoke"));
        assert_eq!(
            verdict(store.authenticate_token("th_idle").await.expect("auth")),
            Err(None)
        );
    }

    /// Expiry reads the activity timestamp authentication already keeps and
    /// adds no write of its own: fifty requests from a due token change one
    /// row once, and an expired token's requests change nothing.
    #[tokio::test]
    async fn sign_in_expiry_keeps_activity_writes_coalesced() {
        let store = SqliteStore::open_in_memory().expect("open");
        let user = store
            .create_user("paul", "hash", false)
            .await
            .expect("create");
        store
            .create_token("th_busy", user.id, None)
            .await
            .expect("token");
        let now = sql_now(&store).await;
        expiry(&store, "1", "90", now - 200 * DAY).await;
        let changes = || store.with_conn(|conn| Ok(conn.total_changes()));

        store
            .fixture_set_token_last_seen("th_busy", now - 2 * DAY)
            .await
            .expect("age");
        let before = changes().await.expect("changes");
        for _ in 0..50 {
            assert_eq!(
                verdict(store.authenticate_token("th_busy").await.expect("auth")),
                Ok(user.id)
            );
        }
        assert_eq!(changes().await.expect("changes") - before, 1);

        store
            .fixture_set_token_last_seen("th_busy", now - 91 * DAY)
            .await
            .expect("age");
        let before = changes().await.expect("changes");
        for _ in 0..50 {
            assert_eq!(
                verdict(store.authenticate_token("th_busy").await.expect("auth")),
                Err(Some(90))
            );
        }
        assert_eq!(changes().await.expect("changes") - before, 0);
    }

    /// Park the writer connection until told to release it, optionally
    /// running one statement on it first (while the reads carry on).
    struct HeldWriter {
        run: std::sync::mpsc::Sender<Option<&'static str>>,
        task: tokio::task::JoinHandle<Result<(), crate::error::StoreError>>,
    }

    async fn hold_writer(store: &Arc<SqliteStore>) -> HeldWriter {
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (run, commands) = std::sync::mpsc::channel::<Option<&'static str>>();
        let task = {
            let store = Arc::clone(store);
            tokio::spawn(async move {
                store
                    .with_conn(move |conn| {
                        held_tx.send(()).ok();
                        while let Ok(Some(sql)) = commands.recv() {
                            conn.execute_batch(sql)?;
                        }
                        Ok(())
                    })
                    .await
            })
        };
        tokio::task::spawn_blocking(move || held_rx.recv())
            .await
            .expect("join")
            .expect("the holder took the writer");
        HeldWriter { run, task }
    }

    impl HeldWriter {
        async fn release(self) {
            self.run.send(None).expect("release");
            self.task.await.expect("join").expect("holder");
        }
    }

    async fn file_store_with_due_token(
        token_hash: &str,
    ) -> (tempfile::TempDir, Arc<SqliteStore>, i64) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("open"));
        let user = store
            .create_user("paul", "hash", false)
            .await
            .expect("user");
        store
            .create_token(token_hash, user.id, None)
            .await
            .expect("token");
        store
            .fixture_set_token_last_seen(token_hash, 0)
            .await
            .expect("age");
        (dir, store, user.id)
    }

    /// K-05 M2: a hundred concurrent requests from one due token are served
    /// while the writer is held, except the single one the gate admitted to
    /// refresh `last_seen_at`, and that refresh lands once.
    ///
    /// Reading on the writer (the old path) serves none of them until the
    /// release; reading on the pool without the gate queues all hundred
    /// refreshes behind the writer. Either regression leaves the count at
    /// zero while the writer is held.
    #[tokio::test]
    async fn token_refresh_is_single_flight_and_reads_skip_the_writer() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (_dir, store, user_id) = file_store_with_due_token("th_burst").await;
        let writer = hold_writer(&store).await;
        let served = Arc::new(AtomicUsize::new(0));
        let requests = (0..100)
            .map(|_| {
                let store = Arc::clone(&store);
                let served = Arc::clone(&served);
                tokio::spawn(async move {
                    let verdict = store.authenticate_token("th_burst").await;
                    served.fetch_add(1, Ordering::SeqCst);
                    verdict
                })
            })
            .collect::<Vec<_>>();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while served.load(Ordering::SeqCst) < 99 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "only {} of 100 authentications completed while the writer was held",
                served.load(Ordering::SeqCst)
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            served.load(Ordering::SeqCst),
            99,
            "exactly one request, the admitted refresh, waits for the writer"
        );
        assert!(store.token_activity.is_reserved("th_burst"));
        let unwritten: i64 = store
            .with_read(|conn| {
                Ok(conn.query_row(
                    "SELECT last_seen_at FROM tokens WHERE token_hash = 'th_burst'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .expect("last_seen_at");
        assert_eq!(unwritten, 0, "nothing written while the writer is held");

        writer.release().await;
        for request in requests {
            match request.await.expect("join").expect("authenticate") {
                TokenAuthentication::Authenticated(user) => assert_eq!(user.id, user_id),
                other => panic!("expected an authenticated user, got {other:?}"),
            }
        }
        assert!(
            last_seen(&store, "th_burst").await > 0,
            "the refresh landed"
        );
    }

    /// Revocation first, then a request: the read connection sees the
    /// committed delete, so the token is unknown.
    #[tokio::test]
    async fn token_deleted_before_the_read_is_unknown() {
        let (_dir, store, _) = file_store_with_due_token("th_gone").await;
        assert!(store.delete_token("th_gone").await.expect("delete"));
        assert!(matches!(
            store.authenticate_token("th_gone").await.expect("auth"),
            TokenAuthentication::Unknown
        ));
    }

    /// A request that read the token, then a revocation, then that request's
    /// refresh: the refresh is an UPDATE that matches nothing, so the token
    /// stays deleted. The request itself is served, having read a live row.
    #[tokio::test]
    async fn token_read_then_deleted_then_touched_stays_deleted() {
        let (_dir, store, user_id) = file_store_with_due_token("th_race").await;
        let writer = hold_writer(&store).await;
        let request = {
            let store = Arc::clone(&store);
            tokio::spawn(async move { store.authenticate_token("th_race").await })
        };
        // Reserved means the read returned and the refresh is waiting for
        // the writer we hold.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !store.token_activity.is_reserved("th_race") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the read never finished"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        writer
            .run
            .send(Some("DELETE FROM tokens WHERE token_hash = 'th_race'"))
            .expect("delete");
        writer.release().await;
        match request.await.expect("join").expect("authenticate") {
            TokenAuthentication::Authenticated(user) => assert_eq!(user.id, user_id),
            other => {
                panic!("expected the request that read a live token to be served, got {other:?}")
            }
        }
        let remaining: i64 = store
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM tokens WHERE token_hash = 'th_race'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .expect("count");
        assert_eq!(
            remaining, 0,
            "the refresh must not resurrect a revoked token"
        );
        assert!(matches!(
            store.authenticate_token("th_race").await.expect("auth"),
            TokenAuthentication::Unknown
        ));
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
