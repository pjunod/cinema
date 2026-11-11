//! Source-only fixture helpers shared without discovering extra test bodies.

#[allow(dead_code)]
pub(crate) const MANAGED_FIXTURE_SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE (name GLOB 'subtitle_managed_*' OR name GLOB 'subtitle_model_receipts*' OR tbl_name GLOB 'subtitle_managed_*' OR tbl_name GLOB 'subtitle_model_receipts*' OR name='background_job_required_resources') AND sql IS NOT NULL ORDER BY name";

pub(crate) fn managed_fixture_connection(version: i64) -> rusqlite::Connection {
    assert!(
        matches!(version, 1..=5),
        "only published managed fixture versions are recognized"
    );
    let expected = rusqlite::Connection::open_in_memory().expect("closed managed metadata witness");
    expected.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT,target_node_id TEXT); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); CREATE VIEW background_job_required_resources AS SELECT 1;").expect("closed metadata dependencies");
    expected
        .execute_batch(include_str!("../../src/store/subtitle_managed_schema.sql"))
        .expect("published managed v1 objects");
    if version >= 2 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_managed_resources_v2.sql"
            ))
            .expect("published managed v2 objects");
    }
    if version >= 3 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_managed_resources_v3.sql"
            ))
            .expect("published managed v3 objects");
    }
    if version >= 4 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_managed_resources_v4.sql"
            ))
            .expect("published managed v4 objects");
    }
    if version == 5 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_managed_resources_v5.sql"
            ))
            .expect("published managed v5 objects");
    }
    expected
}
pub(crate) fn managed_fixture_rows(connection: &rusqlite::Connection) -> Vec<(String, String)> {
    connection
        .prepare(MANAGED_FIXTURE_SHAPE_SQL)
        .expect("managed shape query")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("managed shape rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("complete managed shape")
}
pub(crate) const SOURCE_FIXTURE_SCHEMA: &str = concat!(
    include_str!("../../src/store/background_jobs_foreground.sql"),
    include_str!("../../src/store/source_checkpoints.sql"),
    include_str!("../../src/store/background_jobs_source_interest.sql"),
    include_str!("../../src/store/background_jobs_source_service.sql"),
);

pub(crate) const SOURCE_TAIL_SHAPE: &str = "SELECT type,name,sql FROM sqlite_master
 WHERE sql IS NOT NULL AND (
 name GLOB 'foreground_source_*' OR name GLOB 'background_source_*'
 OR name GLOB 'playback_analysis_*' OR name='analysis_source_checkpoints'
 OR name='source_io_occupancy' OR name GLOB 'dispatch_turn_*'
 OR tbl_name GLOB 'foreground_source_*' OR tbl_name GLOB 'background_source_*'
 OR tbl_name GLOB 'playback_analysis_*' OR tbl_name='analysis_source_checkpoints'
 OR tbl_name GLOB 'dispatch_turn_*') ORDER BY type,name";

fn source_tail_objects(connection: &rusqlite::Connection) -> Vec<(String, String, String)> {
    connection
        .prepare(SOURCE_TAIL_SHAPE)
        .expect("source extension fixture inventory")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("source extension fixture objects")
        .collect::<Result<_, _>>()
        .expect("complete source extension fixture objects")
}

pub(crate) fn validated_source_tail_drops(
    objects: &[(String, String, String)],
    counts: &[(String, i64)],
    dispatch: bool,
    remove: impl Fn(&str) -> bool,
) -> Vec<String> {
    let expected = rusqlite::Connection::open_in_memory().expect("published source tail witness");
    expected.execute_batch(
        "CREATE TABLE background_jobs(id TEXT PRIMARY KEY,state TEXT,fence INTEGER);
         CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT);
         CREATE TABLE background_job_waiters(request_scope TEXT,request_id TEXT,state TEXT,updated_at_ms INTEGER);
         CREATE TABLE analysis_requests(request_id TEXT);",
    ).expect("published source tail dependencies");
    expected
        .execute_batch(SOURCE_FIXTURE_SCHEMA)
        .expect("published v122 source extension");
    if dispatch {
        expected
            .execute_batch(include_str!("../../src/store/background_job_turns.sql"))
            .expect("published v124 dispatch extension");
    }
    assert_eq!(
        objects,
        source_tail_objects(&expected),
        "exact published source tail required"
    );
    let expected_counts = objects
        .iter()
        .filter(|(kind, _, _)| kind == "table")
        .map(|(_, name, _)| (name.clone(), i64::from(name == "background_source_schema")))
        .collect::<Vec<_>>();
    assert_eq!(
        counts, expected_counts,
        "source custody, grants, checkpoints or dispatch demand must not be discarded"
    );
    let mut statements = Vec::new();
    for kind in ["trigger", "view", "table"] {
        for (_, name, _) in objects
            .iter()
            .filter(|(object_kind, name, _)| object_kind == kind && remove(name))
        {
            statements.push(format!(
                "DROP {} \"{}\";",
                kind.to_ascii_uppercase(),
                name.replace('"', "\"\"")
            ));
        }
    }
    statements
}

/// Rewind an owned, empty v122/v123/v124 source tail to published v121.
/// Managed operations and receipts remain intact while their exact resource
/// view and marker return to v3. Source custody, grants and checkpoints refuse.
fn source_extension_rewind_plan(
    connection: &rusqlite::Connection,
    restore_managed: bool,
) -> String {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture source extension ordinal");
    assert!(
        matches!(version, 122..=124),
        "only published source extensions may be rewound"
    );
    let objects = source_tail_objects(connection);
    let counts = objects
        .iter()
        .filter(|(kind, _, _)| kind == "table")
        .map(|(_, name, _)| {
            let count: i64 = connection
                .query_row(
                    &format!(r#"SELECT count(*) FROM "{}""#, name.replace('"', r#""""#)),
                    [],
                    |row| row.get(0),
                )
                .expect("source tail accepted data inventory");
            (name.clone(), count)
        })
        .collect::<Vec<_>>();
    let mut statements = validated_source_tail_drops(&objects, &counts, version == 124, |_| true);
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM background_source_schema WHERE singleton=1 AND version=1",
            [],
            |row| row.get(0),
        )
        .expect("source tail exact marker");
    assert_eq!(marker, 1, "source tail requires its exact singleton marker");
    let managed_version = if version == 122 { 3 } else { 4 };
    assert_eq!(
        managed_fixture_rows(connection),
        managed_fixture_rows(&managed_fixture_connection(managed_version)),
        "source tail requires its exact published managed generation"
    );
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM subtitle_managed_schema WHERE singleton=1 AND version=?1",
            [managed_version],
            |row| row.get(0),
        )
        .expect("managed generation marker");
    assert_eq!(
        marker, 1,
        "managed generation marker must match the source ordinal"
    );
    if restore_managed && managed_version == 4 {
        statements
            .push(include_str!("../../src/store/subtitle_managed_resources_v3.sql").to_owned());
    }
    if restore_managed {
        statements.push("PRAGMA user_version=121;".to_owned());
    }
    statements.join("\n")
}

/// The v125 adjunct exactly as published: metadata-only index coordination
/// and the v3 copy index, spelled as the migration spells them.
const COORDINATION_FIXTURE_SCHEMA: &str = concat!(
    include_str!("../../src/store/analysis_coordination.sql"),
    "\n-- next statement\n",
    include_str!("../../src/store/copy_index_v3.sql"),
);

/// v125 replaces this v124 trigger rather than adding it.
const REPLACED_RESERVATION_TRIGGER: &str = "analysis_source_reserve_on_claim";

fn coordination_tail_witness() -> Vec<(String, String, String)> {
    let witness = rusqlite::Connection::open_in_memory().expect("published coordination witness");
    witness
        .execute_batch(
            "CREATE TABLE analysis_requests(request_id TEXT PRIMARY KEY,state TEXT,fence INTEGER);
             CREATE TABLE analysis_source_reservations(request_id TEXT);
             CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT);",
        )
        .expect("published coordination dependencies");
    witness
        .execute_batch(COORDINATION_FIXTURE_SCHEMA)
        .expect("published v125 coordination extension");
    let mut inventory = witness
        .prepare(
            "SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL
              AND name NOT IN ('analysis_requests','analysis_source_reservations','background_job_commands')
              ORDER BY type,name",
        )
        .expect("coordination witness inventory");
    inventory
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("coordination witness objects")
        .collect::<Result<_, _>>()
        .expect("complete coordination witness")
}

/// The v124 definition v125 replaced, from its own published migration.
fn published_v124_reservation_trigger() -> &'static str {
    let mut statements = include_str!("../../src/store/background_jobs_viewer_analysis.sql")
        .split("-- next statement")
        .filter(|statement| {
            statement.contains(&format!(
                "CREATE TRIGGER IF NOT EXISTS {REPLACED_RESERVATION_TRIGGER}\n"
            ))
        });
    let statement = statements
        .next()
        .expect("published v124 reservation trigger");
    assert!(
        statements.next().is_none(),
        "one published v124 reservation trigger"
    );
    statement
}

/// Plan the rewind of an owned, empty v125/v126 tail to published v124.
/// Coordination roles, interests, designations and every v3 copy index row
/// refuse; the v124 claim reservation trigger and the managed v4 view return.
fn coordination_tail_rewind_plan(connection: &rusqlite::Connection) -> Option<String> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture coordination ordinal");
    if version <= 124 {
        return None;
    }
    assert!(
        matches!(version, 125 | 126),
        "only published coordination tails may be rewound"
    );
    let expected = coordination_tail_witness();
    let names = expected
        .iter()
        .map(|(_, name, _)| format!("'{}'", name.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let installed: Vec<(String, String, String)> = connection
        .prepare(&format!(
            "SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL
              AND (name IN ({names}) OR tbl_name IN ({names})) ORDER BY type,name"
        ))
        .expect("coordination fixture inventory")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("coordination fixture objects")
        .collect::<Result<_, _>>()
        .expect("complete coordination fixture objects");
    assert_eq!(
        installed, expected,
        "exact published coordination tail required"
    );
    let counts = expected
        .iter()
        .filter(|(kind, _, _)| kind == "table")
        .map(|(_, name, _)| {
            let count: i64 = connection
                .query_row(
                    &format!(r#"SELECT count(*) FROM "{}""#, name.replace('"', r#""""#)),
                    [],
                    |row| row.get(0),
                )
                .expect("coordination tail accepted data inventory");
            (name.clone(), count)
        })
        .collect::<Vec<_>>();
    let expected_counts = expected
        .iter()
        .filter(|(kind, _, _)| kind == "table")
        .map(|(_, name, _)| {
            (
                name.clone(),
                i64::from(name == "analysis_coordination_schema"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        counts, expected_counts,
        "coordination roles, designations or copy index rows must not be discarded"
    );
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM analysis_coordination_schema WHERE singleton=1 AND version=1",
            [],
            |row| row.get(0),
        )
        .expect("coordination tail exact marker");
    assert_eq!(
        marker, 1,
        "coordination tail requires its exact singleton marker"
    );
    let managed_version = if version == 126 { 5 } else { 4 };
    assert_eq!(
        managed_fixture_rows(connection),
        managed_fixture_rows(&managed_fixture_connection(managed_version)),
        "coordination tail requires its exact published managed generation"
    );
    let managed_marker: (i64, i64) = connection
        .query_row(
            "SELECT count(*),count(*) FILTER (WHERE singleton=1 AND version=?1) FROM subtitle_managed_schema",
            [managed_version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("coordination tail managed marker");
    assert_eq!(
        managed_marker,
        (1, 1),
        "managed generation marker must match the coordination ordinal"
    );
    let mut statements = Vec::new();
    for kind in ["trigger", "table"] {
        for (_, name, _) in expected
            .iter()
            .filter(|(object_kind, _, _)| object_kind == kind)
        {
            statements.push(format!(
                "DROP {} \"{}\";",
                kind.to_ascii_uppercase(),
                name.replace('"', "\"\"")
            ));
        }
    }
    statements.push(published_v124_reservation_trigger().to_owned());
    if managed_version == 5 {
        statements
            .push(include_str!("../../src/store/subtitle_managed_resources_v4.sql").to_owned());
    }
    statements.push("PRAGMA user_version=124;".to_owned());
    Some(statements.join("\n"))
}

/// Run `body` inside one savepoint: a refusal anywhere in it leaves the
/// fixture exactly as it was, even inside an outer fixture transaction.
pub(crate) fn within_fixture_savepoint(connection: &rusqlite::Connection, body: impl FnOnce()) {
    connection
        .execute_batch("SAVEPOINT coordination_fixture;")
        .expect("open fixture savepoint");
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(()) => connection
            .execute_batch("RELEASE coordination_fixture;")
            .expect("release fixture savepoint"),
        Err(refusal) => {
            connection
                .execute_batch("ROLLBACK TO coordination_fixture; RELEASE coordination_fixture;")
                .expect("roll back refused fixture rewind");
            std::panic::resume_unwind(refusal);
        }
    }
}

/// Rewind an owned, empty v125/v126 coordination tail to published v124;
/// a fixture already at or below v124 is left alone. The caller owns
/// atomicity (see [`within_fixture_savepoint`]).
pub(crate) fn rewind_empty_coordination_tail_to_v124(connection: &rusqlite::Connection) {
    if let Some(plan) = coordination_tail_rewind_plan(connection) {
        connection
            .execute_batch(&plan)
            .expect("published coordination tail rewind");
    }
}

#[allow(dead_code)]
pub(crate) fn validated_empty_source_tail_removal(connection: &rusqlite::Connection) -> String {
    source_extension_rewind_plan(connection, false)
}

pub(crate) fn rewind_empty_source_extensions_to_v121(connection: &rusqlite::Connection) {
    within_fixture_savepoint(connection, || {
        rewind_empty_coordination_tail_to_v124(connection);
        let statements = source_extension_rewind_plan(connection, true);
        if let Err(error) = connection.execute_batch(&statements) {
            panic!("atomic source fixture rewind: {error}");
        }
    });
}
