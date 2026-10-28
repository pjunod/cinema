//! Playback handoff — one title handed from a signed-in client to another
//! local app (the Linux player) through a one-time code, never a bearer in a
//! URL. The code redeems into a token of its own audience
//! ([`super::TokenAudience::PlaybackHandoff`]) that lives only in
//! `playback_handoffs_tokens`, never in `tokens`. Every native lookup reads
//! `tokens`, so no native route can accept a handoff token — including one
//! served by a binary older than this table during a rolling upgrade, which
//! finds nothing and answers 401. The account's sessions list unions the
//! table in, so the token still shows there with its device label. An
//! independently recognized extension schema, installed the same way on
//! SQLite (one migration step) and hiqlite (an idempotent install from the
//! current-schema arm).
use async_trait::async_trait;
use std::sync::LazyLock;

use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("playback_handoffs.sql");
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'playback_handoffs*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM playback_handoffs_schema WHERE singleton=1 AND version=1";
/// The tables this extension references.
const DEPENDENCIES: &str = "CREATE TABLE users(id INTEGER PRIMARY KEY);";

/// How long a minted code may wait to be redeemed.
pub const PLAYBACK_HANDOFF_CODE_TTL_SECS: i64 = 120;
/// How long a redeemed token lives, counted from redemption.
pub const PLAYBACK_HANDOFF_TOKEN_TTL_SECS: i64 = 6 * 60 * 60;

// Shared statements: both backends bind `$n` by first appearance, so each
// statement numbers its parameters in the order they first occur.

/// Records a code only while the minting token still exists, so a mint that
/// races a logout cannot leave a code the logout should have taken with it.
pub(crate) const INSERT_SQL: &str = "INSERT INTO playback_handoffs \
     (id, code_hash, user_id, file_id, source_token_hash, start_ms, audio, subtitle, origin, \
      created_at, expires_at) \
     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11 \
     WHERE EXISTS (SELECT 1 FROM tokens t WHERE t.token_hash = $5 AND t.user_id = $3)";

/// The status read: only the minting bearer may ask.
pub(crate) const STATUS_SQL: &str = "SELECT h.id AS id, h.user_id AS user_id, \
     h.file_id AS file_id, h.start_ms AS start_ms, h.audio AS audio, h.subtitle AS subtitle, \
     h.origin AS origin, h.created_at AS created_at, h.expires_at AS expires_at, \
     h.redeemed_at AS redeemed_at, 0 AS redeemed_here \
     FROM playback_handoffs h \
     WHERE h.id = $1 AND h.user_id = $2 AND h.source_token_hash = $3";

/// Redemption, step 1: claim the code once, while it is unexpired and its
/// minting token still exists. Parameters: now, code hash.
pub(crate) const CLAIM_SQL: &str = "UPDATE playback_handoffs SET redeemed_at = $1 \
     WHERE code_hash = $2 AND redeemed_at IS NULL AND expires_at > $1 \
     AND EXISTS (SELECT 1 FROM tokens s WHERE s.token_hash = playback_handoffs.source_token_hash \
                 AND s.user_id = playback_handoffs.user_id)";
/// Step 2, in the same transaction: the token, only for the claim this
/// redemption made (claimed at `now` and not yet carrying a token).
/// Parameters: token hash, device, now, token expiry, code hash.
pub(crate) const MINT_TOKEN_SQL: &str = "INSERT INTO playback_handoffs_tokens \
     (token_hash, handoff_id, user_id, file_id, source_token_hash, device, created_at, expires_at) \
     SELECT $1, h.id, h.user_id, h.file_id, h.source_token_hash, $2, $3, $4 \
     FROM playback_handoffs h \
     WHERE h.code_hash = $5 AND h.redeemed_at = $3 \
     AND NOT EXISTS (SELECT 1 FROM playback_handoffs_tokens m WHERE m.handoff_id = h.id)";
/// After the redemption: what the code is now, and whether this token is the
/// one it redeemed into. Parameters: token hash, code hash.
pub(crate) const OUTCOME_SQL: &str = "SELECT h.id AS id, h.user_id AS user_id, \
     h.file_id AS file_id, h.start_ms AS start_ms, h.audio AS audio, h.subtitle AS subtitle, \
     h.origin AS origin, h.created_at AS created_at, h.expires_at AS expires_at, \
     h.redeemed_at AS redeemed_at, \
     EXISTS (SELECT 1 FROM playback_handoffs_tokens m \
             WHERE m.handoff_id = h.id AND m.token_hash = $1) AS redeemed_here \
     FROM playback_handoffs h WHERE h.code_hash = $2";

/// Authentication of the `PlaybackHandoff` audience: the user of a live
/// handoff token — unexpired, and its minting token still present.
/// Parameters: token hash, now.
pub(crate) const AUTHENTICATE_SQL: &str = "SELECT u.id AS id, u.username AS username, \
     u.password_hash AS password_hash, u.is_admin AS is_admin, u.created_at AS created_at \
     FROM playback_handoffs_tokens m JOIN users u ON u.id = m.user_id \
     WHERE m.token_hash = $1 AND m.expires_at > $2 \
     AND EXISTS (SELECT 1 FROM tokens s WHERE s.token_hash = m.source_token_hash \
                 AND s.user_id = m.user_id)";
/// The scope of a live handoff token. Parameters: token hash, now.
pub(crate) const TOKEN_SCOPE_SQL: &str = "SELECT m.handoff_id AS handoff_id, \
     m.user_id AS user_id, m.file_id AS file_id, m.expires_at AS expires_at \
     FROM playback_handoffs_tokens m \
     WHERE m.token_hash = $1 AND m.expires_at > $2 \
     AND EXISTS (SELECT 1 FROM tokens s WHERE s.token_hash = m.source_token_hash \
                 AND s.user_id = m.user_id)";

/// Advance only the currently authorized file, without extending the token or
/// detaching it from the login that minted it. Parameters: next file, token
/// hash, expected current file, now. The HTTP handler resolves the successor.
pub(crate) const ADVANCE_SQL: &str = "UPDATE playback_handoffs_tokens SET file_id = $1 \
     WHERE token_hash = $2 AND file_id = $3 AND expires_at > $4 \
     AND EXISTS (SELECT 1 FROM tokens s \
                 WHERE s.token_hash = playback_handoffs_tokens.source_token_hash \
                 AND s.user_id = playback_handoffs_tokens.user_id)";

/// The player revokes its own token. Parameter: token hash.
pub(crate) const REVOKE_SQL: &str = "DELETE FROM playback_handoffs_tokens WHERE token_hash = $1";
/// Revocation from the sessions list: exactly one handoff token of this user
/// carries the prefix, and no login token does (that one is revoked by the
/// ordinary path). Parameters: user id, prefix.
pub(crate) const REVOKE_PREFIX_SQL: &str = "DELETE FROM playback_handoffs_tokens \
     WHERE user_id = $1 AND substr(token_hash, 1, 8) = $2 \
     AND (SELECT COUNT(*) FROM playback_handoffs_tokens \
          WHERE user_id = $1 AND substr(token_hash, 1, 8) = $2) = 1 \
     AND NOT EXISTS (SELECT 1 FROM tokens WHERE user_id = $1 AND substr(token_hash, 1, 8) = $2)";

/// Pruning, step 1: handoff tokens that are expired or whose minting token is
/// gone. Parameter: now.
pub(crate) const PRUNE_TOKENS_SQL: &str = "DELETE FROM playback_handoffs_tokens \
     WHERE expires_at <= $1 \
     OR NOT EXISTS (SELECT 1 FROM tokens s \
                    WHERE s.token_hash = playback_handoffs_tokens.source_token_hash \
                    AND s.user_id = playback_handoffs_tokens.user_id)";
/// Step 2: codes past their expiry by the caller's margin with no live token.
/// Parameter: the cutoff.
pub(crate) const PRUNE_CODES_SQL: &str = "DELETE FROM playback_handoffs \
     WHERE expires_at < $1 AND NOT EXISTS (SELECT 1 FROM playback_handoffs_tokens m \
                                           WHERE m.handoff_id = playback_handoffs.id)";

/// A code as minted. Only the SHA-256 of the code is stored.
#[derive(Clone, Debug)]
pub struct NewPlaybackHandoff {
    pub id: String,
    pub code_hash: String,
    pub user_id: i64,
    pub file_id: i64,
    pub source_token_hash: String,
    pub start_ms: Option<i64>,
    pub audio: Option<i64>,
    pub subtitle: Option<i64>,
    /// The origin the minting client used, when it sent one.
    pub origin: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
}

/// A stored code and what it hands off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackHandoff {
    pub id: String,
    pub user_id: i64,
    pub file_id: i64,
    pub start_ms: Option<i64>,
    pub audio: Option<i64>,
    pub subtitle: Option<i64>,
    pub origin: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub redeemed_at: Option<i64>,
}

/// One redemption attempt: the presented code's hash and the token it would
/// become. Digests only; the plaintext token never reaches the store.
#[derive(Clone)]
pub struct PlaybackHandoffRedemption {
    pub code_hash: String,
    pub token_hash: String,
    pub device: Option<String>,
    pub now: i64,
    pub token_expires_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaybackHandoffRedeemOutcome {
    /// This attempt claimed the code and its token now exists.
    Redeemed(PlaybackHandoff),
    /// The code exists but was already used, has expired, or its minting
    /// token was revoked.
    Gone,
    /// No such code.
    Unknown,
}

/// What a live handoff token may do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackHandoffTokenScope {
    pub handoff_id: String,
    pub user_id: i64,
    pub file_id: i64,
    pub expires_at: i64,
}

#[async_trait]
pub trait PlaybackHandoffStore: Send + Sync {
    /// Record a minted code. `false` when the minting token no longer exists.
    async fn create_playback_handoff(
        &self,
        handoff: NewPlaybackHandoff,
    ) -> Result<bool, StoreError>;
    /// The code `id`, only for the user and the exact token that minted it.
    async fn playback_handoff_for_minter(
        &self,
        id: &str,
        user_id: i64,
        source_token_hash: &str,
    ) -> Result<Option<PlaybackHandoff>, StoreError>;
    /// Claim a code once and mint its token and audience marker atomically.
    async fn redeem_playback_handoff(
        &self,
        redemption: &PlaybackHandoffRedemption,
    ) -> Result<PlaybackHandoffRedeemOutcome, StoreError>;
    /// The scope of a live handoff token: `None` when it is unknown, expired,
    /// revoked, or its minting token is gone.
    async fn playback_handoff_token_scope(
        &self,
        token_hash: &str,
        now: i64,
    ) -> Result<Option<PlaybackHandoffTokenScope>, StoreError>;
    /// Compare-and-swap the live token's file to the server-resolved next
    /// episode. False for a stale predecessor, expiry or revoked minter.
    async fn advance_playback_handoff(
        &self,
        token_hash: &str,
        expected_file_id: i64,
        next_file_id: i64,
        now: i64,
    ) -> Result<bool, StoreError>;
    /// Revoke one handoff token by its digest.
    async fn revoke_playback_handoff_token(&self, token_hash: &str) -> Result<bool, StoreError>;
    /// Revoke the one handoff token of `user_id` whose digest starts with
    /// `prefix`, when no login token shares the prefix.
    async fn revoke_playback_handoff_token_by_prefix(
        &self,
        user_id: i64,
        prefix: &str,
    ) -> Result<bool, StoreError>;
    /// Delete dead handoff tokens and codes expired before `codes_before`.
    async fn prune_playback_handoffs(&self, now: i64, codes_before: i64)
        -> Result<u64, StoreError>;
}

/// Turns the outcome read into a verdict.
pub(crate) fn redeem_outcome(row: Option<(PlaybackHandoff, bool)>) -> PlaybackHandoffRedeemOutcome {
    match row {
        Some((handoff, true)) => PlaybackHandoffRedeemOutcome::Redeemed(handoff),
        Some((_, false)) => PlaybackHandoffRedeemOutcome::Gone,
        None => PlaybackHandoffRedeemOutcome::Unknown,
    }
}

#[cfg(feature = "hiqlite-store")]
pub(crate) fn statements() -> Vec<String> {
    SCHEMA
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn rows(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut query = conn.prepare(SHAPE_SQL)?;
    let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

static SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("handoff schema fixture");
    conn.execute_batch(DEPENDENCIES)
        .expect("handoff schema dependencies");
    conn.execute_batch(SCHEMA).expect("handoff schema");
    rows(&conn).expect("handoff schema shape")
});

/// `None` when nothing of the extension exists, `Some(1)` when its exact shape
/// does; anything in between is refused rather than repaired.
pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let canonical = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(name, sql)| {
                (
                    name.clone(),
                    sql.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect::<Vec<_>>()
    };
    if shape.is_empty() {
        Ok(None)
    } else if canonical(shape) == canonical(&SHAPE) {
        Ok(Some(1))
    } else {
        Err(StoreError::Migration(
            "playback handoff schema is partial or incompatible".into(),
        ))
    }
}

pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration(
            "playback handoff schema missing".into(),
        ));
    }
    let count: i64 = conn.query_row(MARKER_SQL, [], |row| row.get(0))?;
    if count != 1 {
        return Err(StoreError::Migration(
            "playback handoff marker missing".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch(DEPENDENCIES).expect("dependencies");
        conn
    }

    #[test]
    fn handoff_schema_installs_whole_and_refuses_a_partial_shape() {
        let conn = fixture();
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch("BEGIN").expect("transaction");
        conn.execute_batch(SCHEMA).expect("install");
        verify_sqlite(&conn).expect("complete");
        conn.execute_batch("ROLLBACK").expect("rollback");
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch(SCHEMA).expect("install");
        conn.execute_batch(SCHEMA).expect("replay is idempotent");
        verify_sqlite(&conn).expect("replayed");
        conn.execute_batch("DROP INDEX playback_handoffs_tokens_source")
            .expect("corrupt fixture");
        assert!(verify_sqlite(&conn).is_err());
    }

    #[test]
    fn handoff_hiqlite_statements_are_the_sqlite_schema_split() {
        #[cfg(feature = "hiqlite-store")]
        {
            let conn = fixture();
            for statement in statements() {
                conn.execute_batch(&statement).expect("statement");
            }
            verify_sqlite(&conn).expect("same shape as the SQLite step");
        }
    }

    /// The lookup every native route used before this extension existed —
    /// and that a binary older than it still runs — finds no handoff token:
    /// handoff tokens are not rows in `tokens`.
    #[test]
    fn handoff_tokens_are_invisible_to_the_pre_handoff_native_lookup() {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch(
            "CREATE TABLE users(id INTEGER PRIMARY KEY, username TEXT, password_hash TEXT, \
             is_admin INTEGER, created_at INTEGER); \
             CREATE TABLE tokens(token_hash TEXT PRIMARY KEY, user_id INTEGER, device TEXT, \
             created_at INTEGER, last_seen_at INTEGER); \
             CREATE TABLE jellyfin_login_tokens(token_hash TEXT PRIMARY KEY); \
             INSERT INTO users VALUES (1, 'paul', 'hash', 1, 0); \
             INSERT INTO tokens VALUES ('minter', 1, 'web', 0, 0);",
        )
        .expect("fixture tables");
        conn.execute_batch(SCHEMA).expect("install");
        let p = rusqlite::params![
            "h",
            "code",
            1,
            7,
            "minter",
            None::<i64>,
            None::<i64>,
            -1,
            None::<String>,
            10,
            1000
        ];
        assert_eq!(conn.execute(INSERT_SQL, p).expect("mint"), 1);
        assert_eq!(
            conn.execute(CLAIM_SQL, rusqlite::params![100, "code"])
                .expect("claim"),
            1
        );
        assert_eq!(
            conn.execute(
                MINT_TOKEN_SQL,
                rusqlite::params!["player", "plurx Player (Linux)", 100, 100 + 21_600, "code"],
            )
            .expect("mint token"),
            1
        );
        // The native predicate as it stood before this change, verbatim in
        // substance: a token row with no Jellyfin marker is a native bearer.
        const OLD_NATIVE: &str = "SELECT u.id FROM users u JOIN tokens t ON t.user_id = u.id \
             WHERE t.token_hash = ?1 AND ('native' = 'native') = NOT EXISTS ( \
             SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash = t.token_hash)";
        let native = |hash: &str| -> Option<i64> {
            use rusqlite::OptionalExtension;
            conn.query_row(OLD_NATIVE, [hash], |row| row.get(0))
                .optional()
                .expect("old native lookup")
        };
        assert_eq!(
            native("minter"),
            Some(1),
            "the fixture's native lookup works"
        );
        assert_eq!(
            native("player"),
            None,
            "a handoff token is never a native bearer"
        );
        let rows: i64 = conn
            .query_row(
                "SELECT count(*) FROM tokens WHERE token_hash = 'player'",
                [],
                |row| row.get(0),
            )
            .expect("plain tokens lookup");
        assert_eq!(rows, 0);
        // It authenticates under its own audience only.
        let user: i64 = conn
            .query_row(AUTHENTICATE_SQL, rusqlite::params!["player", 200], |row| {
                row.get(0)
            })
            .expect("handoff audience");
        assert_eq!(user, 1);
    }
}
