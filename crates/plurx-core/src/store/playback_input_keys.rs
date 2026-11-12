//! D3t: the planning binding covers every planning key.
//!
//! `playback_input_generation` moves on `playback.*` and `transcode.*` writes
//! ([`super::PLAYBACK_INPUT_SCHEMA`], untouched, because the lineage bridge
//! recognises old backups by it). The playback plan also reads settings
//! outside those globs, so a change to one of them moved no binding. This
//! extension adds insert, update and delete triggers for exactly those keys,
//! under new names and with their own marker, on both stores. No schema
//! version bump: a cluster or binary that predates it ignores the unknown
//! trigger names, and the triggers fire identically on every replica because
//! they are replicated schema.
//!
//! The key list is closed here, for D3b as well. It was derived from every
//! setting the create chain reads (see `PLAYBACK-START-D3-RECORD.md` §6) and is
//! pinned by the tests below and by the census in `plurxd`
//! (`transcode/manager/candidates.rs`), which fails when a planning key
//! outside the globs is not in it.
use super::keys;
use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("playback_input_keys.sql");

/// Planning keys that fall outside the `playback.*` / `transcode.*` globs the
/// original generation triggers cover. Sorted; the SQL carries the same list.
pub const PLAYBACK_INPUT_EXTRA_KEYS: [&str; 5] = [
    keys::CACHE_MAX_GB,
    keys::CLUSTER_MEDIA_POOL_ENABLED,
    keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
    keys::VOD_OUTPUT_PREPARATION,
    keys::VOD_ROLLING_RETENTION,
];

/// Whether a write to `key` moves `playback_input_generation`: the original
/// glob triggers, or this extension.
pub fn playback_input_key_moves_generation(key: &str) -> bool {
    key.starts_with("playback.")
        || key.starts_with("transcode.")
        || PLAYBACK_INPUT_EXTRA_KEYS.contains(&key)
}

/// The extension's canonical objects, for the shape check: what SQLite itself
/// stores for each, over stand-ins for the two tables they reference.
pub(crate) fn schema_objects() -> Result<Vec<(String, String)>, StoreError> {
    let conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL); \
         CREATE TABLE playback_input_generation(singleton INTEGER PRIMARY KEY, generation INTEGER NOT NULL);",
    )?;
    conn.execute_batch(SCHEMA)?;
    let mut query = conn.prepare(
        "SELECT name,sql FROM sqlite_master WHERE name LIKE 'playback_input_keys_%' ORDER BY name",
    )?;
    let rows = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_list() -> String {
        format!(
            "({})",
            PLAYBACK_INPUT_EXTRA_KEYS
                .iter()
                .map(|key| format!("'{key}'"))
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    /// The D3t list, spelled out so changing it is a deliberate edit of this
    /// test, the SQL and the record together.
    #[test]
    fn the_extra_key_list_is_pinned() {
        assert_eq!(
            PLAYBACK_INPUT_EXTRA_KEYS,
            [
                "cache.max_gb",
                "cluster.media_pool_enabled",
                "cluster.session_takeover_enabled",
                "vod.output_preparation",
                "vod.rolling_retention",
            ]
        );
        let mut sorted = PLAYBACK_INPUT_EXTRA_KEYS;
        sorted.sort_unstable();
        assert_eq!(sorted, PLAYBACK_INPUT_EXTRA_KEYS, "keep it sorted");
        for key in PLAYBACK_INPUT_EXTRA_KEYS {
            assert!(
                !key.starts_with("playback.") && !key.starts_with("transcode."),
                "{key} is already covered by the glob triggers"
            );
        }
    }

    /// The SQL and the Rust list cannot drift: every trigger names exactly
    /// the list (insert once, update twice, delete once).
    #[test]
    fn the_sql_names_exactly_the_pinned_keys() {
        assert_eq!(SCHEMA.matches(&in_list()).count(), 4);
        assert_eq!(SCHEMA.matches(" IN (").count(), 4);
    }

    #[test]
    fn the_canonical_objects_are_the_marker_and_three_triggers() {
        let names = schema_objects()
            .expect("canonical objects")
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "playback_input_keys_delete",
                "playback_input_keys_insert",
                "playback_input_keys_schema",
                "playback_input_keys_update",
            ]
        );
    }

    /// Raw SQL is the import and migration path, so the triggers are
    /// exercised directly: insert, update, delete and rollback, for every
    /// pinned key, and for the glob keys and an unrelated key as controls.
    #[test]
    fn every_bound_key_moves_the_generation_and_others_do_not() {
        let conn = rusqlite::Connection::open_in_memory().expect("database");
        conn.execute_batch(
            "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);",
        )
        .expect("settings");
        conn.execute_batch(super::super::PLAYBACK_INPUT_SCHEMA)
            .expect("generation schema");
        conn.execute_batch(SCHEMA).expect("extension");
        let generation = || {
            conn.query_row(
                "SELECT generation FROM playback_input_generation WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("generation")
        };
        let mut keys = PLAYBACK_INPUT_EXTRA_KEYS.to_vec();
        keys.extend(["playback.audio_lang", "transcode.hwaccel"]);
        for key in keys {
            let before = generation();
            conn.execute("INSERT INTO settings VALUES(?1, 'a', 1)", [key])
                .expect("insert");
            assert_eq!(generation(), before + 1, "insert {key}");
            conn.execute("UPDATE settings SET value = 'b' WHERE key = ?1", [key])
                .expect("update");
            assert_eq!(generation(), before + 2, "update {key}");
            conn.execute("DELETE FROM settings WHERE key = ?1", [key])
                .expect("delete");
            assert_eq!(generation(), before + 3, "delete {key}");
            conn.execute_batch(&format!(
                "BEGIN; INSERT INTO settings VALUES('{key}', 'c', 1); ROLLBACK;"
            ))
            .expect("rollback");
            assert_eq!(generation(), before + 3, "rollback {key}");
        }
        // Renaming a key out of, or into, the list counts on either side.
        let before = generation();
        conn.execute_batch("INSERT INTO settings VALUES('jobs.fixture', 'x', 1);")
            .expect("unrelated insert");
        assert_eq!(generation(), before, "unrelated key");
        conn.execute(
            "UPDATE settings SET key = 'cache.max_gb' WHERE key = 'jobs.fixture'",
            [],
        )
        .expect("rename in");
        assert_eq!(generation(), before + 1, "rename into the list");
        conn.execute(
            "UPDATE settings SET key = 'jobs.fixture' WHERE key = 'cache.max_gb'",
            [],
        )
        .expect("rename out");
        assert_eq!(generation(), before + 2, "rename out of the list");
        // Installing twice is a no-op.
        conn.execute_batch(SCHEMA).expect("idempotent");
        assert_eq!(generation(), before + 2);
        assert!(playback_input_key_moves_generation("cache.max_gb"));
        assert!(playback_input_key_moves_generation("playback.x"));
        assert!(!playback_input_key_moves_generation("jobs.x"));
    }
}
