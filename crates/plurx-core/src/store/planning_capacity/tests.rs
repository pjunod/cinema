use super::*;
use rusqlite::{params, Connection};

fn database() -> Connection {
    let conn = Connection::open_in_memory().expect("planning database");
    conn.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT 0) STRICT;
        CREATE TABLE files(id INTEGER PRIMARY KEY,item_id INTEGER,probe_json TEXT);
        CREATE TABLE items(id INTEGER PRIMARY KEY,title TEXT);
        INSERT INTO items VALUES(1,'original title');
        INSERT INTO files VALUES(1,1,'original probe');")
        .expect("baseline source");
    conn.execute_batch(super::super::PLAYBACK_INPUT_SCHEMA)
        .expect("generation");
    conn
}

fn automatic(conn: &Connection) {
    for key in &CAPACITY_KEYS[..2] {
        conn.execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES(?1,'0')",
            [key],
        )
        .expect("cache");
        conn.execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES(?1,?2)",
            params![
                format!("{key}.provenance"),
                r#"{"schema":1,"mode":"automatic","bound_value":"0"}"#
            ],
        )
        .expect("saved intent");
    }
}

struct Read {
    capacity: PlanningCapacity,
    values: BTreeMap<String, String>,
    probe: String,
    title: String,
    generation: i64,
}
fn read(conn: &Connection, fallback: bool) -> Result<Read, rusqlite::Error> {
    conn.query_row(
        &statement("id,item_id", fallback),
        params![1, selected_keys(&[]).expect("keys")],
        |row| {
            let encoded: String = row.get("planning_settings")?;
            let values =
                serde_json::from_str(&encoded).expect("SQL strings, not parsed companions");
            let catalog: String = row.get("planning_capacity_catalog")?;
            let marker: Option<String> = row.get("planning_capacity_marker")?;
            Ok(Read {
                capacity: PlanningCapacity::from_statement(&values, &catalog, marker.as_deref()),
                values,
                probe: row.get("planning_probe")?,
                title: row.get("planning_item_title")?,
                generation: row.get("planning_generation")?,
            })
        },
    )
}

#[test]
fn verified_planning_preserves_raw_intent_and_key_presence_in_one_statement() {
    let conn = database();
    super::super::resource_settings_schema::install_sqlite(&conn).expect("complete installation");
    for fallback in [false, true] {
        let sql = statement("id,item_id", fallback);
        let prepared = conn.prepare(&sql).expect("both fixed templates prepare");
        assert_eq!(prepared.parameter_name(1), Some("$1"));
        assert_eq!(prepared.parameter_name(2), Some("$2"));
        #[cfg(feature = "hiqlite-store")]
        super::super::hiqlite::validate_sql(&sql).expect("canonical replicated placeholder order");
    }
    automatic(&conn);
    let snapshot = read(&conn, false).expect("normal full statement");
    assert!(matches!(
        snapshot.capacity.installation,
        CapacityInstallation::Ready { .. }
    ));
    for key in CAPACITY_KEYS {
        assert!(snapshot.values.contains_key(key), "required raw key {key}");
    }
    assert_eq!(snapshot.values[keys::SW_POOL_THREADS], "0");
    assert_eq!(
        snapshot.capacity.saved_modes.hardware_sessions.state,
        ReadState::Automatic
    );
    assert_eq!(
        snapshot
            .capacity
            .admission_value(&snapshot.values, keys::MAX_HW_SESSIONS),
        None
    );
    assert_eq!(
        snapshot
            .capacity
            .admission_value(&snapshot.values, keys::SW_POOL_THREADS),
        None
    );
    assert_eq!(snapshot.probe, "original probe");
    assert_eq!(snapshot.title, "original title");
    // No settings-map mutation or manufactured stored defaults.
    conn.execute(
        "UPDATE settings SET value=' 00 ' WHERE key=?1",
        [keys::MAX_HW_SESSIONS],
    )
    .expect("old writer");
    let legacy = read(&conn, false).expect("same normal read");
    assert_eq!(
        legacy
            .capacity
            .admission_value(&legacy.values, keys::MAX_HW_SESSIONS),
        Some(" 00 ")
    );
    assert_ne!(legacy.capacity.binding, snapshot.capacity.binding);
}

#[test]
fn absent_partial_and_future_schema_cannot_authorize_saved_automatic() {
    for schema in [None, Some("CREATE TABLE resource_settings_schema(singleton INTEGER)"),
        Some("CREATE TABLE resource_settings_schema(singleton INTEGER,version INTEGER,revision INTEGER,write_claim TEXT)")] {
        let conn = database();
        if let Some(schema) = schema { conn.execute_batch(schema).expect("unknown optional schema"); }
        automatic(&conn);
        let first = read(&conn, false);
        let snapshot = match first {
            Ok(snapshot) => snapshot,
            Err(error) => {
                assert!(sqlite_missing_optional(&error), "only optional parse failures fall back: {error}");
                read(&conn, true).expect("baseline-only full read")
            }
        };
        assert_eq!(snapshot.capacity.saved_modes.hardware_sessions.state, ReadState::Automatic);
        assert_eq!(snapshot.capacity.verified_modes.hardware_sessions.state, ReadState::LegacyAmbiguous);
        assert_eq!(snapshot.capacity.admission_value(&snapshot.values, keys::MAX_HW_SESSIONS), Some("0"));
        assert_eq!(snapshot.capacity.admission_value(&snapshot.values, keys::SW_POOL_THREADS), Some("0"));
        assert_eq!(snapshot.probe, "original probe");
        assert_eq!(snapshot.title, "original title");
    }
}

#[test]
fn marker_and_shape_changes_invalidate_capacity_even_with_equal_generation() {
    let conn = database();
    super::super::resource_settings_schema::install_sqlite(&conn).expect("complete installation");
    automatic(&conn);
    let before = read(&conn, false).expect("ready");
    conn.execute(
        "UPDATE resource_settings_schema SET revision=revision+1",
        [],
    )
    .expect("marker-only restore revision");
    let changed = read(&conn, false).expect("ready with new revision");
    assert_eq!(before.generation, changed.generation);
    assert_eq!(before.values, changed.values);
    assert_ne!(before.capacity.binding, changed.capacity.binding);
    conn.execute(
        "UPDATE resource_settings_schema SET write_claim='private-claim-not-exposed'",
        [],
    )
    .expect("active write fence");
    let claimed = read(&conn, false).expect("ordinary playback still reads");
    assert_eq!(
        claimed.capacity.installation,
        CapacityInstallation::InvalidMarker
    );
    assert_eq!(
        claimed
            .capacity
            .admission_value(&claimed.values, keys::MAX_HW_SESSIONS),
        Some("0")
    );
    assert!(!serde_json::to_string(&claimed.capacity)
        .expect("diagnostic")
        .contains("private-claim-not-exposed"));
    conn.execute("UPDATE resource_settings_schema SET write_claim=NULL", [])
        .expect("clear");
    conn.execute("DROP TRIGGER resource_settings_numeric_au", [])
        .expect("partial restore");
    let partial = read(&conn, false).expect("unknown shape conservatively reads");
    assert_eq!(
        partial.capacity.installation,
        CapacityInstallation::Unrecognized
    );
    assert_eq!(before.generation, partial.generation);
    assert_ne!(before.capacity.binding, partial.capacity.binding);
}

#[test]
fn fallback_never_upgrades_a_concurrent_install_and_uses_only_its_own_source() {
    let conn = database();
    automatic(&conn);
    let error = read(&conn, false).err().expect("marker absent at prepare");
    assert!(sqlite_missing_optional(&error));
    super::super::resource_settings_schema::install_sqlite(&conn).expect("concurrent install");
    automatic(&conn);
    conn.execute_batch(
        "UPDATE files SET probe_json='new probe'; UPDATE items SET title='new title';
        UPDATE playback_input_generation SET generation=999;",
    )
    .expect("changed complete source");
    let fallback = read(&conn, true).expect("one full fallback");
    assert_eq!(
        fallback.capacity.installation,
        CapacityInstallation::UnverifiedMarker
    );
    assert_eq!(
        fallback.capacity.saved_modes.hardware_sessions.state,
        ReadState::Automatic
    );
    assert_eq!(
        fallback.capacity.verified_modes.hardware_sessions.state,
        ReadState::LegacyAmbiguous
    );
    assert_eq!(fallback.probe, "new probe");
    assert_eq!(fallback.title, "new title");
    assert_eq!(fallback.generation, 999);
    let next_request = read(&conn, false).expect("new request may independently verify");
    assert!(matches!(
        next_request.capacity.installation,
        CapacityInstallation::Ready { .. }
    ));
    assert_ne!(fallback.capacity.binding, next_request.capacity.binding);
}

#[test]
fn malformed_optional_marker_and_companion_never_become_playback_errors_or_authority() {
    let conn = database();
    super::super::resource_settings_schema::install_sqlite(&conn).expect("complete installation");
    automatic(&conn);
    let catalog: String = conn
        .query_row(
            &statement("id", false),
            params![1, selected_keys(&[]).expect("keys")],
            |r| r.get("planning_capacity_catalog"),
        )
        .expect("exact shape");
    let values = read(&conn, false).expect("ready").values;
    for marker in [
        "[]",
        "[[1,1,-1,true]]",
        "[[1,2,0,true]]",
        "[[1,1,0,true],[1,1,0,true]]",
        "[[1,1,null,true]]",
        "[[1,1,0,false]]",
        "broken",
    ] {
        let capacity = PlanningCapacity::from_statement(&values, &catalog, Some(marker));
        assert_eq!(
            capacity.installation,
            CapacityInstallation::InvalidMarker,
            "{marker}"
        );
        assert_eq!(
            capacity.admission_value(&values, keys::MAX_HW_SESSIONS),
            Some("0")
        );
    }
    conn.execute(
        "INSERT OR REPLACE INTO settings(key,value) VALUES(?1,'not json')",
        [keys::MAX_HW_SESSIONS_PROVENANCE],
    )
    .expect("malformed saved text");
    let malformed = read(&conn, false).expect("JSON quotes text; no json(value)");
    assert_eq!(
        malformed.capacity.saved_modes.hardware_sessions.state,
        ReadState::LegacyAmbiguous
    );
    assert_eq!(
        malformed
            .capacity
            .admission_value(&malformed.values, keys::MAX_HW_SESSIONS),
        Some("0")
    );
    conn.execute("DELETE FROM resource_settings_schema", [])
        .expect("missing marker");
    assert_eq!(
        read(&conn, false)
            .expect("legacy available")
            .capacity
            .installation,
        CapacityInstallation::InvalidMarker
    );
}

#[test]
fn only_exact_optional_name_resolution_allows_fallback_not_step_or_baseline_errors() {
    let conn = database();
    let missing = read(&conn, false).err().expect("optional table absent");
    assert!(sqlite_missing_optional(&missing));
    conn.execute("DROP TABLE items", [])
        .expect("mandatory table gone");
    assert!(!sqlite_missing_optional(
        &read(&conn, true).err().expect("baseline failure")
    ));
    let step = conn
        .query_row("SELECT abs(-9223372036854775808)", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect_err("deliberate runtime overflow");
    assert!(!sqlite_missing_optional(&step));
    #[cfg(feature = "hiqlite-store")]
    {
        let sql = statement("id,item_id", false);
        let vendor = hiqlite::Error::from(missing);
        assert!(hiqlite_missing_optional(&vendor, &sql));
        assert!(!hiqlite_missing_optional(&hiqlite::Error::from(step), &sql));
        assert!(!hiqlite_missing_optional(
            &hiqlite::Error::Timeout(sql.clone()),
            &sql
        ));
        assert!(!hiqlite_missing_optional(
            &hiqlite::Error::PrepareStatement(
                "no such table: main.resource_settings_schema".into()
            ),
            &sql
        ));
        assert!(!hiqlite_missing_optional(
            &hiqlite::Error::Sqlite(format!("no such table: items in {sql} at offset 0").into()),
            &sql
        ));
        for suffix in ["-1", "bogus", "99999999999999999999"] {
            assert!(!hiqlite_missing_optional(
                &hiqlite::Error::Sqlite(
                    format!(
                        "no such table: main.resource_settings_schema in {sql} at offset {suffix}"
                    )
                    .into()
                ),
                &sql
            ));
        }
    }
}
