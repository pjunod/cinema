//! D3t: the planning binding covers every planning key.
//!
//! `playback_input_generation` moves on `playback.*` and `transcode.*` writes
//! ([`super::PLAYBACK_INPUT_SCHEMA`], untouched, because the lineage bridge
//! recognises old backups by it). The playback plan also reads settings
//! outside those globs, so a change to one of them moved no binding. This
//! extension adds insert, update and delete triggers for exactly those keys,
//! under new revision-tagged names and with their own marker, on both stores.
//! No schema version bump on either: a cluster or binary that predates it
//! ignores the unknown trigger names, and the triggers fire identically on
//! every replica because they are replicated schema. SQLite installs them
//! from `SqliteStore::init` after `migrate` (so `user_version` does not move);
//! the replicated store installs them beside source arbitration.
//!
//! The key list is closed here, for D3b as well. It was derived from every
//! setting the create chain reads (see `PLAYBACK-START-D3-RECORD.md` §6) and is
//! pinned by the tests below and by the source scan in `plurxd`
//! (`transcode/manager/candidates.rs`), which fails when a setting the create
//! chain reads is outside every trigger and not on a reasoned exclusion list.
//!
//! Revisions: the objects below are revision 1. A binary checks only the
//! objects of the revisions it knows and tolerates higher-revision objects and
//! a higher marker version, so changing the list later means adding `_r2_*`
//! triggers and raising the marker, never editing these.
use super::keys;
use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("playback_input_keys.sql");

/// Every object revision 1 owns, sorted.
pub(crate) const OBJECT_NAMES: [&str; 4] = [
    "playback_input_keys_r1_delete",
    "playback_input_keys_r1_insert",
    "playback_input_keys_r1_update",
    "playback_input_keys_schema",
];

/// Planning keys that fall outside the `playback.*` / `transcode.*` globs the
/// original generation triggers cover. Sorted; the SQL carries the same list.
pub const PLAYBACK_INPUT_EXTRA_KEYS: [&str; 6] = [
    keys::CACHE_MAX_GB,
    keys::CLUSTER_MEDIA_POOL_ENABLED,
    keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
    keys::SUBTITLE_STORED_SOURCES,
    keys::VOD_OUTPUT_PREPARATION,
    keys::VOD_ROLLING_RETENTION,
];

/// A copy of the `plurxd` planning list (`QUALITY_PLANNING_KEYS`), so the
/// store-contract test can write every key. A `plurxd` test pins it equal to
/// that list; D3b widens both together.
pub const PLAYBACK_PLANNING_KEYS: [&str; 36] = [
    keys::HWACCEL,
    keys::DV_CONVERT,
    keys::DV_HDR_PROCESSING,
    keys::DV_DISPLAY_TARGET,
    keys::DV_FEL_REENCODE,
    keys::AUDIO_LANG,
    keys::SUB_LANG,
    keys::SUB_MODE,
    keys::PLAYBACK_DISPLAY_AWARE_AUTO,
    keys::PLAYBACK_CONTROL_PROTOCOL_V1,
    keys::TRANSCODE_RATE_MODE,
    keys::TRANSCODE_QUALITY,
    "playback.vod_reorder_frames",
    keys::PLAYBACK_SDR_MASTER_CODECS,
    keys::CACHE_MAX_GB,
    keys::HLS_READRATE,
    keys::HLS_BURST_SECS,
    keys::SW_POOL_THREADS,
    keys::MAX_HW_SESSIONS,
    keys::CONTENT_AWARE_ENCODING,
    keys::HLS_AHEAD_MAX_SECS,
    keys::HLS_AHEAD_MAX_BYTES,
    keys::HLS_SCRATCH_MAX_BYTES,
    keys::VOD_OUTPUT_PREPARATION,
    keys::VOD_ROLLING_RETENTION,
    // D3b (§4.2): the settings the create chain read after the snapshot,
    // so the one plan answers them. `vod_settings`' admission policy:
    keys::VOD_PRESENTATION,
    keys::VOD_WORKING_SET_BYTES,
    keys::VOD_BLOCK_BUDGET_SECS,
    keys::VOD_MATERIALIZE_BUDGET_SECS,
    keys::VOD_BLOCKED_GET_CAP,
    keys::VOD_INDEX_CLUSTER_CACHE,
    keys::HEVC_UNVERIFIED_COPY,
    keys::VOD_LIVE_RECOVERY,
    // the recorded playlist shape (`cluster_playlist_is_typeless`):
    keys::HLS_TYPELESS_SLIDING,
    keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
    // and placement (`remote_placement_ready`).
    keys::CLUSTER_MEDIA_POOL_ENABLED,
];

/// Whether a write to `key` moves `playback_input_generation`: the original
/// glob triggers, or this extension. A test runs real SQL against this.
pub fn playback_input_key_moves_generation(key: &str) -> bool {
    key.starts_with("playback.")
        || key.starts_with("transcode.")
        || PLAYBACK_INPUT_EXTRA_KEYS.contains(&key)
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
        "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL); \
         CREATE TABLE playback_input_generation(singleton INTEGER PRIMARY KEY, generation INTEGER NOT NULL);",
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

/// SQLite: install the extension after `migrate`, idempotently. An absent
/// shape is installed in one transaction; a complete one (objects equal to the
/// canonical text, marker committed at revision 1 or later) is left alone; a
/// partial or mutated one is refused rather than repaired. `user_version` does
/// not move, so an older binary opens the database and ignores the objects.
pub(crate) fn install_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let expected = schema_objects()?;
    if sqlite_shape(conn, &expected)? {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(SCHEMA)
        .map_err(|e| StoreError::Migration(format!("installing playback input keys: {e}")))?;
    tx.commit()?;
    if sqlite_shape(conn, &expected)? {
        return Ok(());
    }
    Err(StoreError::Migration(
        "playback input keys extension did not settle".into(),
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
            "playback input keys extension has incompatible shape".into(),
        ));
    }
    let marker: i64 = conn.query_row(
        "SELECT count(*) FROM playback_input_keys_schema WHERE singleton=1 AND version>=1",
        [],
        |row| row.get(0),
    )?;
    if marker != 1 {
        return Err(StoreError::Migration(
            "playback input keys marker missing".into(),
        ));
    }
    Ok(true)
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
                "subtitles.stored_sources",
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
    fn the_canonical_objects_are_the_marker_and_three_revision_one_triggers() {
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

    fn database() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("database");
        conn.execute_batch(
            "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);",
        )
        .expect("settings");
        conn.execute_batch(super::super::PLAYBACK_INPUT_SCHEMA)
            .expect("generation schema");
        conn
    }

    fn generation(conn: &rusqlite::Connection) -> i64 {
        conn.query_row(
            "SELECT generation FROM playback_input_generation WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("generation")
    }

    /// Raw SQL is the import and migration path, so the triggers are
    /// exercised directly: insert, update, delete and rollback, for every
    /// pinned key, and for the glob keys and an unrelated key as controls.
    #[test]
    fn every_bound_key_moves_the_generation_and_others_do_not() {
        let conn = database();
        install_sqlite(&conn).expect("extension");
        let mut keys = PLAYBACK_INPUT_EXTRA_KEYS.to_vec();
        keys.extend(["playback.audio_lang", "transcode.hwaccel"]);
        for key in keys {
            let before = generation(&conn);
            conn.execute("INSERT INTO settings VALUES(?1, 'a', 1)", [key])
                .expect("insert");
            assert_eq!(generation(&conn), before + 1, "insert {key}");
            conn.execute("UPDATE settings SET value = 'b' WHERE key = ?1", [key])
                .expect("update");
            assert_eq!(generation(&conn), before + 2, "update {key}");
            conn.execute("DELETE FROM settings WHERE key = ?1", [key])
                .expect("delete");
            assert_eq!(generation(&conn), before + 3, "delete {key}");
            conn.execute_batch(&format!(
                "BEGIN; INSERT INTO settings VALUES('{key}', 'c', 1); ROLLBACK;"
            ))
            .expect("rollback");
            assert_eq!(generation(&conn), before + 3, "rollback {key}");
        }
        // Renaming a key out of, or into, the list counts on either side.
        let before = generation(&conn);
        conn.execute_batch("INSERT INTO settings VALUES('jobs.fixture', 'x', 1);")
            .expect("unrelated insert");
        assert_eq!(generation(&conn), before, "unrelated key");
        conn.execute(
            "UPDATE settings SET key = 'cache.max_gb' WHERE key = 'jobs.fixture'",
            [],
        )
        .expect("rename in");
        assert_eq!(generation(&conn), before + 1, "rename into the list");
        conn.execute(
            "UPDATE settings SET key = 'jobs.fixture' WHERE key = 'cache.max_gb'",
            [],
        )
        .expect("rename out");
        assert_eq!(generation(&conn), before + 2, "rename out of the list");
        // A write that changes nothing still moves it, as the glob triggers
        // do: one extra owner refusal and re-plan, never a missed binding.
        conn.execute("INSERT INTO settings VALUES('cache.max_gb', 'x', 1)", [])
            .expect("insert");
        let before = generation(&conn);
        conn.execute(
            "UPDATE settings SET value = 'x' WHERE key = 'cache.max_gb'",
            [],
        )
        .expect("no-op update");
        assert_eq!(generation(&conn), before + 1, "a no-op write moves it");
        // Installing twice is a no-op.
        let before = generation(&conn);
        install_sqlite(&conn).expect("idempotent");
        assert_eq!(generation(&conn), before);
    }

    /// The Rust predicate is what plurxd and the contract test reason with;
    /// the SQL is what runs. They describe the same set: every planning key,
    /// every extra key, glob edge cases that must and must not match, and
    /// unrelated keys, written through the real triggers.
    #[test]
    fn the_predicate_and_the_triggers_describe_the_same_set() {
        let conn = database();
        install_sqlite(&conn).expect("extension");
        let mut probes = PLAYBACK_PLANNING_KEYS.to_vec();
        probes.extend(PLAYBACK_INPUT_EXTRA_KEYS);
        probes.extend([
            "playback.x",
            "playback.",
            "transcode.x",
            "transcode.",
            "playbackx",
            "transcodes.x",
            "Playback.x",
            "xplayback.x",
            "cache.max_gb2",
            "vod.rolling_retention.x",
            "jobs.x",
            "subtitles.mode",
        ]);
        for key in probes {
            let before = generation(&conn);
            conn.execute("INSERT INTO settings VALUES(?1, 'a', 1)", [key])
                .expect("insert");
            let moved = generation(&conn) > before;
            assert_eq!(
                moved,
                playback_input_key_moves_generation(key),
                "insert {key}: SQL and predicate disagree"
            );
            conn.execute("DELETE FROM settings WHERE key = ?1", [key])
                .expect("delete");
        }
    }

    /// Installing over a partial or mutated shape is refused, and a store that
    /// already carries a later revision opens: unknown `_r2_*` objects and a
    /// higher marker version are tolerated.
    #[test]
    fn a_partial_shape_is_refused_and_a_later_revision_is_tolerated() {
        let conn = database();
        install_sqlite(&conn).expect("extension");
        conn.execute_batch(
            "CREATE TRIGGER playback_input_keys_r2_insert AFTER INSERT ON settings WHEN NEW.key = 'x.y' \
             BEGIN UPDATE playback_input_generation SET generation = generation + 1 WHERE singleton = 1; END; \
             UPDATE playback_input_keys_schema SET version = 2;",
        )
        .expect("a later revision");
        install_sqlite(&conn).expect("a later revision is tolerated");
        conn.execute_batch("DROP TRIGGER playback_input_keys_r1_delete;")
            .expect("break it");
        assert!(matches!(
            install_sqlite(&conn),
            Err(StoreError::Migration(_))
        ));
    }
}
