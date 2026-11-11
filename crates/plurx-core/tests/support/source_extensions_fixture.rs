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

pub(crate) const SCAN_REQUEST_FIXTURE_SHAPE_SQL: &str = "SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL AND (name GLOB 'background_scan_request_v2*' OR tbl_name GLOB 'background_scan_request_v2*') ORDER BY type,name";

fn scan_request_fixture_objects(
    connection: &rusqlite::Connection,
) -> Vec<(String, String, String)> {
    connection
        .prepare(SCAN_REQUEST_FIXTURE_SHAPE_SQL)
        .expect("scan request sidecar inventory")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("scan request sidecar objects")
        .collect::<Result<_, _>>()
        .expect("complete scan request sidecar objects")
}

/// The published v125 sidecar objects, built from the migration's own SQL.
pub(crate) fn scan_request_fixture_shape() -> Vec<(String, String, String)> {
    let expected = rusqlite::Connection::open_in_memory().expect("scan request witness");
    expected
        .execute_batch("CREATE TABLE background_library_requests(request_id TEXT PRIMARY KEY);")
        .expect("scan request sidecar dependency");
    expected
        .execute_batch(include_str!("../../src/store/scan_request_v2.sql"))
        .expect("published v125 scan request sidecar");
    scan_request_fixture_objects(&expected)
}

/// The pure removal plan for the v125 contract v2 scan request sidecar: the
/// exact published DDL, its one marker and no request facts, or no plan. Triggers first, then the tables;
/// the unique index goes with its table.
pub(crate) fn validated_scan_request_drops(
    objects: &[(String, String, String)],
    marker: (i64, i64),
    requests: i64,
) -> Vec<String> {
    assert_eq!(
        objects,
        scan_request_fixture_shape(),
        "exact published scan request sidecar required"
    );
    assert_eq!(
        marker,
        (1, 1),
        "the scan request sidecar requires its exact singleton marker"
    );
    assert_eq!(
        requests, 0,
        "targeted scan request facts must not be discarded"
    );
    let mut statements = Vec::new();
    for kind in ["trigger", "table"] {
        for (_, name, _) in objects
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
    statements
}

/// At v125, the validated sidecar removal; below it the sidecar must be absent.
fn scan_request_rewind_plan(connection: &rusqlite::Connection, version: i64) -> Vec<String> {
    let objects = scan_request_fixture_objects(connection);
    if version < 125 {
        assert!(
            objects.is_empty(),
            "a fixture before v125 cannot carry the scan request sidecar"
        );
        return Vec::new();
    }
    // Validate DDL before using any table identity in the inventory.
    assert_eq!(
        objects,
        scan_request_fixture_shape(),
        "exact published scan request sidecar required"
    );
    let marker: (i64, i64) = connection
        .query_row(
            "SELECT count(*),COALESCE(sum(singleton=1 AND version=1),0) FROM background_scan_request_v2_schema",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("scan request marker inventory");
    let requests: i64 = connection
        .query_row(
            "SELECT count(*) FROM background_scan_request_v2",
            [],
            |row| row.get(0),
        )
        .expect("scan request data inventory");
    validated_scan_request_drops(&objects, marker, requests)
}

/// Rewind an owned v126 fixture to published v125: the exact managed v5
/// resource view and marker return to v4. Managed operations, receipts and
/// every reservation stay as they are; only the requirement view changes.
#[allow(dead_code)]
pub(crate) fn rewind_empty_targeted_managed_to_v125(connection: &rusqlite::Connection) {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture targeted managed ordinal");
    assert_eq!(
        version, 126,
        "only the published v126 managed generation may be rewound"
    );
    assert_eq!(
        managed_fixture_rows(connection),
        managed_fixture_rows(&managed_fixture_connection(5)),
        "exact published managed v5 shape required"
    );
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM subtitle_managed_schema WHERE singleton=1 AND version=5",
            [],
            |row| row.get(0),
        )
        .expect("managed v5 marker");
    assert_eq!(marker, 1, "managed v5 requires its exact singleton marker");
    connection
        .execute_batch(&format!(
            "SAVEPOINT targeted_managed_fixture; {} PRAGMA user_version=125; RELEASE targeted_managed_fixture;",
            include_str!("../../src/store/subtitle_managed_resources_v4.sql")
        ))
        .expect("atomic targeted managed fixture rewind");
}

/// Rewind an owned v125 fixture to published v124: only the exact empty
/// scan request sidecar is removed.
#[allow(dead_code)]
pub(crate) fn rewind_empty_scan_request_to_v124(connection: &rusqlite::Connection) {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture scan request ordinal");
    assert_eq!(
        version, 125,
        "only the published v125 sidecar may be rewound"
    );
    let statements = scan_request_rewind_plan(connection, version).join("\n");
    connection
        .execute_batch(&format!(
            "SAVEPOINT scan_request_fixture; {statements} PRAGMA user_version=124; RELEASE scan_request_fixture;"
        ))
        .expect("atomic scan request fixture rewind");
}

/// Rewind an owned, empty v122-v126 source tail to published v121 (the v125
/// scan request sidecar first, through its own validated plan).
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
        matches!(version, 122..=126),
        "only published source extensions may be rewound"
    );
    let sidecar = scan_request_rewind_plan(connection, version);
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
    let mut statements = sidecar;
    statements.extend(validated_source_tail_drops(
        &objects,
        &counts,
        version >= 124,
        |_| true,
    ));
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM background_source_schema WHERE singleton=1 AND version=1",
            [],
            |row| row.get(0),
        )
        .expect("source tail exact marker");
    assert_eq!(marker, 1, "source tail requires its exact singleton marker");
    let managed_version = match version {
        122 => 3,
        123..=125 => 4,
        _ => 5,
    };
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
    if restore_managed && managed_version >= 4 {
        statements
            .push(include_str!("../../src/store/subtitle_managed_resources_v3.sql").to_owned());
    }
    if restore_managed {
        statements.push("PRAGMA user_version=121;".to_owned());
    }
    statements.join("\n")
}

#[allow(dead_code)]
pub(crate) fn validated_empty_source_tail_removal(connection: &rusqlite::Connection) -> String {
    source_extension_rewind_plan(connection, false)
}

pub(crate) fn rewind_empty_source_extensions_to_v121(connection: &rusqlite::Connection) {
    let statements = source_extension_rewind_plan(connection, true);
    let sql = format!(
        "SAVEPOINT source_extension_fixture; {statements} RELEASE source_extension_fixture;"
    );
    if let Err(error) = connection.execute_batch(&sql) {
        connection
            .execute_batch(
                "ROLLBACK TO source_extension_fixture; RELEASE source_extension_fixture;",
            )
            .expect("rollback failed source fixture rewind");
        panic!("atomic source fixture rewind: {error}");
    }
}
