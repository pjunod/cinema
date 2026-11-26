//! Each signed-in device's last playback capability document (Curator ADR
//! 0026 §5), kept with the login token it arrived on.
//!
//! The row belongs to the token: `token_hash` references `tokens` and
//! `user_id` references `users`, both `ON DELETE CASCADE`, and both stores
//! run with foreign keys enforced (the SQLite writer and every hiqlite
//! replica's state machine set `PRAGMA foreign_keys = ON`). Logout, revoking
//! a device, a password reset, deleting the account — every path that removes
//! the token removes the record, with nothing for a handler to remember.
//! Readers join the live token and apply the same idle-expiry policy
//! authentication applies, so an expired sign-in is not listed either.
//!
//! An additive extension in the `playback_input_keys` pattern: its own
//! marker, revision-tagged object names, no schema version bump on either
//! store. SQLite installs it from `SqliteStore::init` after `migrate`; the
//! replicated store installs it beside the other extensions. A binary checks
//! only the revision-1 objects and tolerates later revisions and a higher
//! marker version.
use crate::auth::TokenIdlePolicy;
use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("playback_device_caps.sql");

/// Every object revision 1 owns, sorted.
pub(crate) const OBJECT_NAMES: [&str; 3] = [
    "playback_device_caps_r1",
    "playback_device_caps_r1_user",
    "playback_device_caps_schema",
];

/// Largest capability document a device record keeps, in bytes, after the
/// volatile fields are stripped. The table refuses anything longer.
pub const MAX_STORED_DEVICE_CAPS_BYTES: usize = 16 * 1024;

/// Most devices one listing returns. Per account it is the token inventory's
/// bound; across accounts (a `playback:read` key) it is four of those.
pub const DEVICE_CAPS_LIST_MAX: usize = 4 * super::TOKEN_SUMMARY_MAX;

/// The current record of a login token, for the change check before a write.
pub(crate) const CURRENT: &str = "SELECT caps FROM playback_device_caps_r1 WHERE token_hash = $1";

/// Record a native login token's document. The token's own row supplies the
/// user, so a record can only ever name the account the token belongs to,
/// and a Jellyfin compatibility login is never a playback device. An
/// unchanged document is not rewritten.
pub(crate) const UPSERT: &str =
    "INSERT INTO playback_device_caps_r1(token_hash, user_id, caps, updated_at) \
     SELECT t.token_hash, t.user_id, $1, $2 FROM tokens t \
     WHERE t.token_hash = $3 \
       AND NOT EXISTS (SELECT 1 FROM jellyfin_login_tokens l WHERE l.token_hash = t.token_hash) \
     ON CONFLICT(token_hash) DO UPDATE SET caps = excluded.caps, updated_at = excluded.updated_at \
     WHERE playback_device_caps_r1.caps <> excluded.caps";

const LIST_COLUMNS: &str = "SELECT substr(t.token_hash, 1, 8) AS token_prefix, \
            substr(t.device, 1, $1) AS device, \
            u.id AS user_id, u.username AS username, t.last_seen_at AS last_seen_at, \
            c.caps AS caps, c.updated_at AS updated_at, \
            (SELECT value FROM settings WHERE key = $2) AS expiry_enabled, \
            (SELECT value FROM settings WHERE key = $3) AS idle_days, \
            (SELECT value FROM settings WHERE key = $4) AS expiry_since \
     FROM playback_device_caps_r1 c \
     JOIN tokens t ON t.token_hash = c.token_hash \
     JOIN users u ON u.id = t.user_id";

/// One account's devices, most recently used first.
pub(crate) fn list_for_user_sql() -> String {
    format!(
        "{LIST_COLUMNS} WHERE c.user_id = $5 ORDER BY t.last_seen_at DESC, t.token_hash LIMIT $6"
    )
}

/// Every account's devices, most recently used first.
pub(crate) fn list_all_sql() -> String {
    format!("{LIST_COLUMNS} ORDER BY t.last_seen_at DESC, t.token_hash LIMIT $5")
}

/// A signed-in device's recorded capabilities, joined to its live token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackDeviceCaps {
    /// First eight hex digits of the token digest, as `/me/devices` names it.
    pub token_prefix: String,
    pub user_id: i64,
    pub username: String,
    pub device: Option<String>,
    pub last_seen_at: i64,
    /// The stored capability document (JSON).
    pub caps: String,
    pub updated_at: i64,
}

/// One listing row before the expiry policy is applied.
pub(crate) struct ListedDeviceCaps {
    pub record: PlaybackDeviceCaps,
    pub expiry_enabled: Option<String>,
    pub idle_days: Option<String>,
    pub expiry_since: Option<String>,
}

/// Drop the rows whose sign-in has expired under the policy read in the same
/// statement, exactly as authentication would refuse them at `now`.
pub(crate) fn live(rows: Vec<ListedDeviceCaps>, now: i64) -> Vec<PlaybackDeviceCaps> {
    rows.into_iter()
        .filter(|row| {
            TokenIdlePolicy::from_settings(
                row.expiry_enabled.as_deref(),
                row.idle_days.as_deref(),
                row.expiry_since.as_deref(),
            )
            .is_none_or(|policy| !policy.is_expired(row.record.last_seen_at, now))
        })
        .map(|mut row| {
            row.record.device = super::bounded_device_label(row.record.device);
            row.record
        })
        .collect()
}

/// `'a','b',…` over [`OBJECT_NAMES`], for an exact `IN` list.
pub(crate) fn object_name_list() -> String {
    OBJECT_NAMES
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The extension's canonical objects, for the shape check: what SQLite itself
/// stores for each, over stand-ins for the two tables they reference.
pub(crate) fn schema_objects() -> Result<Vec<(String, String)>, StoreError> {
    let conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE users(id INTEGER PRIMARY KEY); \
         CREATE TABLE tokens(token_hash TEXT PRIMARY KEY);",
    )?;
    conn.execute_batch(SCHEMA)?;
    let mut query = conn.prepare(&format!(
        "SELECT name,sql FROM sqlite_master WHERE name IN ({}) ORDER BY name",
        object_name_list()
    ))?;
    let rows = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The statements a replicated install runs, in order.
#[cfg(feature = "hiqlite-store")]
pub(crate) fn statements() -> impl Iterator<Item = &'static str> {
    SCHEMA
        .split("-- next statement\n")
        .filter(|sql| !sql.trim().is_empty())
}

/// SQLite: install the extension after `migrate`, idempotently. An absent
/// shape is installed in one transaction; a complete one is left alone; a
/// partial or mutated one is refused rather than repaired.
pub(crate) fn install_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let expected = schema_objects()?;
    if sqlite_shape(conn, &expected)? {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(SCHEMA)
        .map_err(|e| StoreError::Migration(format!("installing playback device caps: {e}")))?;
    tx.commit()?;
    if sqlite_shape(conn, &expected)? {
        return Ok(());
    }
    Err(StoreError::Migration(
        "playback device caps extension did not settle".into(),
    ))
}

/// `true` when the complete extension is installed, `false` when none of its
/// objects exist, an error for anything between.
fn sqlite_shape(
    conn: &rusqlite::Connection,
    expected: &[(String, String)],
) -> Result<bool, StoreError> {
    let mut query = conn.prepare(&format!(
        "SELECT name,COALESCE(sql,'') FROM sqlite_master WHERE name IN ({}) ORDER BY name",
        object_name_list()
    ))?;
    let installed = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if installed.is_empty() {
        return Ok(false);
    }
    if installed != expected {
        return Err(StoreError::Migration(
            "playback device caps extension has incompatible shape".into(),
        ));
    }
    let marker: i64 = conn.query_row(
        "SELECT count(*) FROM playback_device_caps_schema WHERE singleton=1 AND version>=1",
        [],
        |row| row.get(0),
    )?;
    if marker != 1 {
        return Err(StoreError::Migration(
            "playback device caps marker missing".into(),
        ));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("database");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; \
             CREATE TABLE users(id INTEGER PRIMARY KEY); \
             CREATE TABLE tokens(token_hash TEXT PRIMARY KEY, \
                 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE); \
             CREATE TABLE jellyfin_login_tokens(token_hash TEXT PRIMARY KEY); \
             INSERT INTO users VALUES(1); INSERT INTO tokens VALUES('aaaa', 1);",
        )
        .expect("parents");
        conn
    }

    fn rows(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM playback_device_caps_r1", [], |row| {
            row.get(0)
        })
        .expect("count")
    }

    #[test]
    fn the_canonical_objects_are_the_marker_the_table_and_its_index() {
        let names = schema_objects()
            .expect("canonical objects")
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(names, OBJECT_NAMES);
        let mut sorted = OBJECT_NAMES;
        sorted.sort_unstable();
        assert_eq!(sorted, OBJECT_NAMES, "keep it sorted");
    }

    /// The upsert writes once, skips an unchanged document, and the row goes
    /// with its token and with its user.
    #[test]
    fn a_record_follows_its_token_and_skips_an_unchanged_document() {
        let conn = database();
        install_sqlite(&conn).expect("extension");
        install_sqlite(&conn).expect("idempotent");
        let upsert = |caps: &str, at: i64| {
            conn.execute(UPSERT, rusqlite::params![caps, at, "aaaa"])
                .expect("upsert")
        };
        assert_eq!(upsert("{}", 1), 1);
        assert_eq!(upsert("{}", 2), 0, "unchanged is not rewritten");
        assert_eq!(upsert("{\"v\":2}", 3), 1);
        assert_eq!(
            conn.execute(UPSERT, rusqlite::params!["{}", 4, "unknown"])
                .expect("absent token"),
            0,
            "no token, no record"
        );
        conn.execute("DELETE FROM tokens WHERE token_hash = 'aaaa'", [])
            .expect("logout");
        assert_eq!(rows(&conn), 0, "the record goes with its token");

        conn.execute("INSERT INTO tokens VALUES('bbbb', 1)", [])
            .expect("token");
        conn.execute(UPSERT, rusqlite::params!["{}", 5, "bbbb"])
            .expect("upsert");
        conn.execute("DELETE FROM users WHERE id = 1", [])
            .expect("delete user");
        assert_eq!(rows(&conn), 0, "the record goes with its user");

        assert!(conn
            .execute(
                "INSERT INTO playback_device_caps_r1 VALUES('cccc', 1, '{}', 1)",
                [],
            )
            .is_err());
    }

    #[test]
    fn a_partial_shape_is_refused_and_a_later_revision_is_tolerated() {
        let conn = database();
        install_sqlite(&conn).expect("extension");
        conn.execute_batch(
            "CREATE TABLE playback_device_caps_r2(token_hash TEXT PRIMARY KEY) STRICT; \
             UPDATE playback_device_caps_schema SET version = 2;",
        )
        .expect("a later revision");
        install_sqlite(&conn).expect("a later revision is tolerated");
        conn.execute_batch("DROP INDEX playback_device_caps_r1_user;")
            .expect("break it");
        assert!(matches!(
            install_sqlite(&conn),
            Err(StoreError::Migration(_))
        ));
    }

    #[test]
    fn an_expired_sign_in_is_not_listed() {
        let row = |last_seen_at: i64| ListedDeviceCaps {
            record: PlaybackDeviceCaps {
                token_prefix: "aaaa".into(),
                user_id: 1,
                username: "paul".into(),
                device: None,
                last_seen_at,
                caps: "{}".into(),
                updated_at: 0,
            },
            expiry_enabled: Some("1".into()),
            idle_days: Some("1".into()),
            expiry_since: Some("0".into()),
        };
        let now = 10 * 86_400;
        let kept = live(vec![row(now - 10), row(now - 2 * 86_400)], now);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].last_seen_at, now - 10);
        let mut off = row(0);
        off.expiry_enabled = Some("0".into());
        assert_eq!(live(vec![off], now).len(), 1, "expiry off keeps every row");
    }
}
