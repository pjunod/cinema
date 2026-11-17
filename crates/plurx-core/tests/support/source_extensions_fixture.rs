//! Source-only fixture helpers shared without discovering extra test bodies.
#[path = "title_extension_fixture.rs"]
pub(crate) mod title_extension_fixture;

#[allow(dead_code)]
pub(crate) const MANAGED_FIXTURE_SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE (name GLOB 'subtitle_managed_*' OR name GLOB 'subtitle_model_receipts*' OR tbl_name GLOB 'subtitle_managed_*' OR tbl_name GLOB 'subtitle_model_receipts*' OR name='background_job_required_resources') AND sql IS NOT NULL ORDER BY name";

pub(crate) fn managed_fixture_connection(version: i64) -> rusqlite::Connection {
    assert!(
        matches!(version, 1..=6),
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
    if version >= 5 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_managed_resources_v5.sql"
            ))
            .expect("published managed v5 objects");
    }
    if version == 6 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_managed_resources_v6.sql"
            ))
            .expect("published managed v6 objects");
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

/// The published v1 (v122 source, v124 dispatch) source tail objects.
pub(crate) fn published_source_tail(dispatch: bool) -> Vec<(String, String, String)> {
    source_tail_objects(&published_source_tail_witness(dispatch))
}

/// The source tail of a store migrated by this binary (contracted leases).
#[allow(dead_code)]
pub(crate) fn current_source_tail() -> Vec<(String, String, String)> {
    let directory = tempfile::tempdir().expect("current source tail witness");
    let path = directory.path().join("current.db");
    drop(plurx_core::store::SqliteStore::open(&path).expect("current SQLite store"));
    source_tail_objects(&rusqlite::Connection::open(&path).expect("current catalogue"))
}

fn published_source_tail_witness(dispatch: bool) -> rusqlite::Connection {
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
    expected
}

pub(crate) fn validated_source_tail_drops(
    objects: &[(String, String, String)],
    counts: &[(String, i64)],
    dispatch: bool,
    remove: impl Fn(&str) -> bool,
) -> Vec<String> {
    assert_eq!(
        objects,
        published_source_tail(dispatch),
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

/// The v127 adjunct exactly as published: metadata-only index coordination
/// and the v3 copy index, spelled as the migration spells them.
const COORDINATION_FIXTURE_SCHEMA: &str = concat!(
    include_str!("../../src/store/analysis_coordination.sql"),
    "\n-- next statement\n",
    include_str!("../../src/store/copy_index_v3.sql"),
);

/// v127 replaces this v124 trigger rather than adding it.
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
        .expect("published v127 coordination extension");
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

/// The v124 definition v127 replaced, from its own published migration.
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

/// Plan the rewind of an owned, empty v127/v128 tail to published v126.
/// Coordination roles, interests, designations and every v3 copy index row
/// refuse; the v124 claim reservation trigger and the managed v5 view return.
fn coordination_tail_rewind_plan(connection: &rusqlite::Connection) -> Option<String> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture coordination ordinal");
    if version <= 126 {
        return None;
    }
    assert!(
        matches!(version, 127 | 128),
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
    let managed_version = if version == 128 { 6 } else { 5 };
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
    if managed_version == 6 {
        statements
            .push(include_str!("../../src/store/subtitle_managed_resources_v5.sql").to_owned());
    }
    statements.push("PRAGMA user_version=126;".to_owned());
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

/// The published trigger `name` from one migration's own statements.
fn published_trigger(migration: &str, name: &str) -> String {
    let mut statements = migration
        .split("-- next statement")
        .filter(|statement| statement.contains(&format!("CREATE TRIGGER IF NOT EXISTS {name}\n")));
    let statement = statements
        .next()
        .expect("published trigger")
        .trim()
        .to_owned();
    assert!(statements.next().is_none(), "one published {name}");
    statement
}

pub(crate) fn rewind_title_v2_to_v131(connection: &rusqlite::Connection) {
    // Rewind appended extensions in reverse migration order. The lease
    // rewind preserves delegation at v133; its own strict empty-data check
    // then removes delegation to reach v132.
    rewind_empty_source_leases_to_v133(connection);
    rewind_empty_delegation_to_v132(connection);
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("title continuation ordinal");
    if version != 132 {
        return;
    }
    title_extension_fixture::require_fixture_marker(connection, "dv_title_jobs_schema", 2);
    let actual = connection
        .prepare(title_extension_fixture::TITLE_FIXTURE_SHAPE_SQL)
        .expect("title v2 shape")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("title v2 shape rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("complete title v2 shape");
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM dv_title_jobs_schema WHERE singleton=1 AND version=2",
            [],
            |row| row.get(0),
        )
        .expect("exact title v2 marker");
    let retained: i64 = connection.query_row(
        "SELECT (SELECT count(*) FROM dv_title_jobs_demand) + (SELECT count(*) FROM background_jobs WHERE kind='dv_title_scan') + (SELECT count(*) FROM background_job_commands WHERE operation IN ('bind_dv_title','publish_dv_title')) + (SELECT count(*) FROM files WHERE json_valid(probe_json) AND (json_type(probe_json,'$.plurx_dv_title_v1') IS NOT NULL OR json_type(probe_json,'$.plurx_dv_fel_requests_v3') IS NOT NULL))",
        [], |row| row.get(0)).expect("complete title v2 retained data");
    let _ = title_extension_fixture::validated_empty_title_rewind(&actual, 2, marker, retained);
    within_fixture_savepoint(connection, || {
        connection
            .execute_batch(&title_extension_fixture::published_title_v1_restore_plan())
            .expect("restore exact published v1 title topology");
        title_extension_fixture::assert_title_fixture_generation(connection, 1);
        connection
            .execute_batch("PRAGMA user_version=131;")
            .expect("owned empty title v2 tail");
    });
}

/// Rewind an owned, empty v131 storage-domain extension to published v130.
/// Desired configuration is user data, including an already settled revision,
/// so no pending-table row may be discarded by a historical fixture.
fn rewind_empty_storage_domain_pending_to_v130(connection: &rusqlite::Connection) {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture storage domain pending ordinal");
    if version <= 130 {
        return;
    }
    assert_eq!(
        version, 131,
        "only the published v131 storage domain tail may be rewound"
    );
    let shape_sql = "SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL AND (
        name IN ('background_storage_domain_pending','background_storage_domains_live',
        'background_storage_domain_changed_roots','background_storage_domain_disturbed',
        'background_storage_domain_holders','background_storage_domain_affected_jobs',
        'background_storage_domains_apply','background_storage_domain_pending_schema')
        OR tbl_name IN ('background_storage_domain_pending','background_storage_domain_pending_schema'))
        ORDER BY type,name";
    let objects = |catalogue: &rusqlite::Connection| -> Vec<(String, String, String)> {
        catalogue
            .prepare(shape_sql)
            .expect("storage domain fixture inventory")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("storage domain fixture objects")
            .collect::<Result<_, _>>()
            .expect("complete storage domain fixture objects")
    };
    let witness = rusqlite::Connection::open_in_memory().expect("published storage domain witness");
    witness.execute_batch("CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT);")
        .expect("storage domain trigger dependency");
    witness
        .execute_batch(include_str!("../../src/store/storage_domain_pending.sql"))
        .expect("published v131 storage domain extension");
    assert_eq!(
        objects(connection),
        objects(&witness),
        "exact published storage domain tail required"
    );
    let marker: (i64, i64) = connection.query_row(
        "SELECT count(*),count(*) FILTER (WHERE singleton=1 AND version=1) FROM background_storage_domain_pending_schema",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).expect("storage domain pending exact marker");
    assert_eq!(
        marker,
        (1, 1),
        "storage domain pending requires its exact singleton marker"
    );
    let accepted: i64 = connection
        .query_row(
            "SELECT count(*) FROM background_storage_domain_pending",
            [],
            |row| row.get(0),
        )
        .expect("storage domain pending accepted data inventory");
    assert_eq!(
        accepted, 0,
        "desired storage domain configuration must not be discarded"
    );
    // Validate the entire extension before removing anything. The savepoint
    // also protects direct callers from a refused or failed DDL statement.
    within_fixture_savepoint(connection, || {
        connection
            .execute_batch(
                "DROP TRIGGER background_storage_domains_apply;
             DROP VIEW background_storage_domain_affected_jobs;
             DROP VIEW background_storage_domain_disturbed;
             DROP VIEW background_storage_domain_changed_roots;
             DROP VIEW background_storage_domain_holders;
             DROP VIEW background_storage_domains_live;
             DROP TABLE background_storage_domain_pending_schema;
             DROP TABLE background_storage_domain_pending;
             PRAGMA user_version=130;",
            )
            .expect("published storage domain pending rewind");
    });
}

/// Rewind v130 (offload: `items.offloaded_at` and its two clearing triggers)
/// to v129. No item may still be offloaded: an offload mark is accepted data
/// the rewind would silently discard, so it refuses like every other tail.
fn rewind_offload_to_v129(connection: &rusqlite::Connection) {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture offload ordinal");
    if version != 130 {
        return;
    }
    let offloaded: i64 = connection
        .query_row(
            "SELECT count(*) FROM items WHERE offloaded_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("offload marks");
    assert_eq!(
        offloaded, 0,
        "historical fixture must not discard offloaded items"
    );
    connection
        .execute_batch(
            "DROP TRIGGER items_offloaded_clear_on_file_insert;
             DROP TRIGGER items_offloaded_clear_on_file_move;
             ALTER TABLE items DROP COLUMN offloaded_at;
             PRAGMA user_version=129;",
        )
        .expect("published offload rewind");
}

/// Rewind v129 (retention keeps a charged custody's owner job) to v128: the
/// exact marker is required; the v85 enqueue and v86 upkeep triggers return.
fn rewind_custody_retention_to_v128(connection: &rusqlite::Connection) {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture custody retention ordinal");
    if version != 129 {
        return;
    }
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM background_custody_retention_schema WHERE singleton=1 AND version=1",
            [],
            |row| row.get(0),
        )
        .expect("custody retention marker");
    assert_eq!(marker, 1, "custody retention requires its exact marker");
    let plan = [
        "DROP TABLE background_custody_retention_schema;".to_owned(),
        "DROP INDEX background_source_custody_owner_work;".to_owned(),
        "DROP TRIGGER background_job_enqueue_command;".to_owned(),
        "DROP TRIGGER background_job_maintenance_command;".to_owned(),
        format!(
            "{};",
            published_trigger(
                include_str!("../../src/store/background_jobs_retention.sql"),
                "background_job_enqueue_command"
            )
            .trim_end_matches(';')
        ),
        format!(
            "{};",
            published_trigger(
                include_str!("../../src/store/background_jobs_receipt_pressure.sql"),
                "background_job_maintenance_command"
            )
            .trim_end_matches(';')
        ),
        "PRAGMA user_version=128;".to_owned(),
    ];
    connection
        .execute_batch(&plan.join("\n"))
        .expect("published custody retention rewind");
}

/// One published statement of a migration, found by its opening text.
fn published_statement(migration: &str, opening: &str) -> String {
    let mut statements = migration
        .split("-- next statement")
        .map(str::trim)
        .filter(|statement| {
            statement
                .lines()
                .skip_while(|line| line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
                .starts_with(opening)
        });
    let statement = statements.next().expect("published statement").to_owned();
    assert!(statements.next().is_none(), "one published {opening}");
    format!("{};", statement.trim_end_matches(';'))
}

// Inventory every lease-owned object (including foreign-named indexes/triggers)
// and every existing object the rewind replaces. Validate before any DROP can
// hide hostile drift from the older extensions' own strict validators.
pub(crate) const SOURCE_LEASE_REWIND_SHAPE_SQL: &str = "SELECT type,name,sql FROM sqlite_master
 WHERE sql IS NOT NULL AND (
 name GLOB 'source_lease*' OR tbl_name GLOB 'source_lease*'
 OR name IN (
 'background_job_required_resources','analysis_required_resources',
 'dispatch_turn_library_resources','dispatch_turn_contenders',
 'dispatch_turn_ordinary_occupancy','dispatch_turn_independent',
 'background_storage_domain_holders','background_storage_domain_disturbed',
 'background_storage_domain_affected_jobs','background_source_pause_effects',
 'background_source_resume_effects','background_job_enqueue_command',
 'background_job_maintenance_command')
 OR tbl_name IN (
 'background_job_required_resources','analysis_required_resources',
 'dispatch_turn_library_resources','dispatch_turn_contenders',
 'dispatch_turn_ordinary_occupancy','dispatch_turn_independent',
 'background_storage_domain_holders','background_storage_domain_disturbed',
 'background_storage_domain_affected_jobs','background_source_pause_effects',
 'background_source_resume_effects','background_job_enqueue_command',
 'background_job_maintenance_command')) ORDER BY type,name";

fn source_lease_rewind_objects(connection: &rusqlite::Connection) -> Vec<(String, String, String)> {
    connection
        .prepare(SOURCE_LEASE_REWIND_SHAPE_SQL)
        .expect("source lease rewind inventory")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("source lease rewind rows")
        .collect::<Result<_, _>>()
        .expect("complete source lease rewind inventory")
}

pub(crate) fn current_source_lease_rewind_shape() -> &'static [(String, String, String)] {
    static EXPECTED: std::sync::OnceLock<Vec<(String, String, String)>> =
        std::sync::OnceLock::new();
    EXPECTED
        .get_or_init(|| {
            let directory = tempfile::tempdir().expect("current lease rewind witness");
            let path = directory.path().join("current.db");
            drop(
                plurx_core::store::SqliteStore::open(&path)
                    .expect("canonical current SQLite store"),
            );
            let witness =
                rusqlite::Connection::open(&path).expect("current lease rewind catalogue");
            source_lease_rewind_objects(&witness)
        })
        .as_slice()
}

fn verify_current_source_lease_rewind_shape(connection: &rusqlite::Connection) {
    assert_eq!(
        source_lease_rewind_objects(connection).as_slice(),
        current_source_lease_rewind_shape(),
        "exact current source lease and overlaid catalogue required before rewind"
    );
    let marker: (i64, i64) = connection
        .query_row(
            "SELECT count(*),count(*) FILTER (WHERE singleton=1 AND version=3) FROM source_lease_schema",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("contracted source lease marker");
    assert_eq!(
        marker,
        (1, 1),
        "current lease rewind requires its singular contracted marker"
    );
}

/// Rewind an owned, empty v134-v135 source-lease tail to pre-lease v133:
/// drop the lease objects and restore the v1 source owners and the four
/// dependents the expand replaced, from their own published text.
fn rewind_empty_source_leases_to_v133(connection: &rusqlite::Connection) {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture source lease ordinal");
    if version != 135 {
        return;
    }
    verify_current_source_lease_rewind_shape(connection);
    // A job or analysis lease is also recorded by its owner's claim and
    // reservations; playback, attestation and offline leases are not.
    let leases: i64 = connection
        .query_row(SOURCE_LEASE_UNOWNED_SQL, [], |row| row.get(0))
        .expect("source leases");
    assert_eq!(
        leases, 0,
        "historical fixture must not discard source leases"
    );
    let mut plan = source_lease_rewind_plan();
    plan.push("PRAGMA user_version=133;".to_owned());
    connection
        .execute_batch(&plan.join("\n"))
        .expect("published source lease rewind");
}

/// Leases that only the lease table records (playback, attestation,
/// offline), plus receipt provenance the v1 catalogue cannot represent;
/// a rewind that would discard either is refused.
pub(crate) const SOURCE_LEASE_UNOWNED_SQL: &str =
    "SELECT count(*)+(SELECT count(*) FROM source_lease_receipt_sources) AS value FROM source_leases WHERE owner_kind NOT IN ('job','analysis')";

/// The statements that take a contracted (v3) source-lease tail back to
/// the published v1 source owners, on either backend. The claim effects
/// are left as they are: the replicated rewind restores their v1 text
/// itself, SQLite's v132 already has the open-ended text.
pub(crate) fn source_lease_rewind_plan() -> Vec<String> {
    let foreground = include_str!("../../src/store/background_jobs_foreground.sql");
    let turns = include_str!("../../src/store/background_job_turns.sql");
    let pending = include_str!("../../src/store/storage_domain_pending.sql");
    let retention = include_str!("../../src/store/background_jobs_custody_retention.sql");
    let managed_v6 = include_str!("../../src/store/subtitle_managed_resources_v6.sql");
    let analysis = include_str!("../../src/store/background_jobs_viewer_analysis.sql");
    let mut plan = Vec::new();
    for trigger in [
        "source_lease_job_claim",
        "source_lease_job_renew",
        "source_lease_job_cancelling",
        "source_lease_job_settled",
        "source_lease_analysis_claim",
        "source_lease_analysis_renew",
        "source_lease_analysis_success",
        "source_lease_attestation_renew",
        "source_lease_offline_renew",
        "source_lease_offline_end",
        "source_lease_offline_delete",
        "source_lease_resume_admission",
        "source_lease_service_root_admission",
        "source_lease_receipt_insert",
        "source_lease_receipt_update",
        "dispatch_turn_independent",
        "background_source_pause_effects",
        "background_source_resume_effects",
        "background_job_enqueue_command",
        "background_job_maintenance_command",
    ] {
        plan.push(format!("DROP TRIGGER {trigger};"));
    }
    for view in [
        "dispatch_turn_ordinary_occupancy",
        "dispatch_turn_contenders",
        "dispatch_turn_library_resources",
        "background_storage_domain_holders",
        "background_storage_domain_disturbed",
        "background_storage_domain_affected_jobs",
        "background_job_required_resources",
        "analysis_required_resources",
        "source_lease_receipt_resources",
        "source_lease_service_scopes",
        "source_lease_permissions",
        "source_lease_effective_resources",
        "source_lease_owner_scopes",
        "source_lease_job_scopes",
        "source_lease_file_resources",
        "source_lease_limits",
        "source_lease_root_resources",
        "source_lease_roots",
    ] {
        plan.push(format!("DROP VIEW {view};"));
    }
    plan.extend([
        "DROP TABLE source_lease_receipt_sources;".to_owned(),
        "DROP TABLE source_leases;".to_owned(),
        "DROP TABLE source_lease_schema;".to_owned(),
    ]);
    plan.push(foreground.to_owned());
    for (sql, opening) in [
        (
            managed_v6,
            "CREATE VIEW IF NOT EXISTS background_job_required_resources AS",
        ),
        (
            analysis,
            "CREATE VIEW IF NOT EXISTS analysis_required_resources AS",
        ),
        (turns, "CREATE VIEW dispatch_turn_library_resources AS"),
        (
            pending,
            "CREATE VIEW IF NOT EXISTS background_storage_domain_disturbed AS",
        ),
        (
            pending,
            "CREATE VIEW IF NOT EXISTS background_storage_domain_affected_jobs AS",
        ),
    ] {
        plan.push(published_statement(sql, opening));
    }
    for opening in [
        "CREATE VIEW dispatch_turn_ordinary_occupancy",
        "CREATE VIEW dispatch_turn_contenders",
        "CREATE TRIGGER dispatch_turn_independent",
    ] {
        plan.push(published_statement(turns, opening));
    }
    plan.push(published_statement(
        pending,
        "CREATE VIEW IF NOT EXISTS background_storage_domain_holders",
    ));
    for opening in [
        "CREATE INDEX IF NOT EXISTS background_source_custody_owner_work",
        "CREATE TRIGGER IF NOT EXISTS background_job_enqueue_command",
        "CREATE TRIGGER IF NOT EXISTS background_job_maintenance_command",
    ] {
        plan.push(published_statement(retention, opening));
    }
    plan
}

/// The published v1 (two-slot) claim effects, as a v1 replicated store has
/// them: its base job schema is installed only when the cluster is created.
#[allow(dead_code)]
pub(crate) const CLAIM_EFFECTS_V1: [&str; 2] = [
    "DROP TRIGGER background_job_claim_effects;",
    include_str!("../fixtures/background-job-claim-effects-v1.sql"),
];

/// Rewind an owned, empty v127-v134 coordination tail to published v126;
/// a fixture already at or below v126 is left alone. The caller owns
/// atomicity (see [`within_fixture_savepoint`]).
pub(crate) fn rewind_empty_coordination_tail_to_v126(connection: &rusqlite::Connection) {
    rewind_title_v2_to_v131(connection);
    rewind_empty_storage_domain_pending_to_v130(connection);
    rewind_offload_to_v129(connection);
    rewind_custody_retention_to_v128(connection);
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
        rewind_empty_coordination_tail_to_v126(connection);
        let statements = source_extension_rewind_plan(connection, true);
        if let Err(error) = connection.execute_batch(&statements) {
            panic!("atomic source fixture rewind: {error}");
        }
    });
}

/// Remove only the exact, entirely empty v133 extension before older tails.
/// Read and validate the complete removal plan before changing any object.
#[allow(dead_code)]
pub(crate) fn rewind_empty_delegation_to_v132(conn: &rusqlite::Connection) {
    let ordinal: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("delegation ordinal");
    if ordinal < 133 {
        return;
    };
    assert_eq!(ordinal, 133, "only published delegation tail can rewind");
    const QUERY: &str = "SELECT name,sql FROM sqlite_master WHERE (name GLOB 'delegation*' OR tbl_name GLOB 'delegation*') AND sql IS NOT NULL ORDER BY name";
    let shape = |db: &rusqlite::Connection| {
        db.prepare(QUERY)
            .expect("shape query")
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .expect("shape rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete shape")
    };
    let fixture = rusqlite::Connection::open_in_memory().expect("canonical shape");
    fixture
        .execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY);")
        .expect("dependency");
    fixture
        .execute_batch(include_str!("../../src/store/delegation.sql"))
        .expect("published delegation");
    assert_eq!(
        shape(conn),
        shape(&fixture),
        "exact delegation shape required"
    );
    let marker: i64 = conn
        .query_row(
            "SELECT count(*) FROM delegation_schema WHERE singleton=1 AND version=1",
            [],
            |r| r.get(0),
        )
        .expect("marker");
    assert_eq!(marker, 1, "published marker required");
    for table in [
        "delegation_schema",
        "delegation_clients",
        "delegation_codes",
        "delegation_grants",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .expect("data inventory");
        assert_eq!(
            count,
            if table == "delegation_schema" { 1 } else { 0 },
            "cannot discard delegation user data"
        )
    }
    conn.execute_batch("DROP TABLE delegation_grants;DROP TABLE delegation_codes;DROP TABLE delegation_clients;DROP TABLE delegation_schema;PRAGMA user_version=132;").expect("empty delegation rewind");
}

#[cfg(test)]
mod delegation_rewind_tests {
    use super::*;
    fn snapshot(c: &rusqlite::Connection) -> Vec<String> {
        let mut out = vec![];
        for query in [
            "SELECT json_array(name,sql) FROM sqlite_master ORDER BY name",
            "SELECT json_array(id,name,secret_hash,redirect,generation,active) FROM delegation_clients ORDER BY id",
            "SELECT json_array(code_hash,client_id,client_generation,user_id,subject,source_hash,challenge,redirect,expires_at,claim) FROM delegation_codes ORDER BY code_hash",
            "SELECT json_array(grant_hash,code_hash,client_id,client_generation,user_id,subject,source_hash,expires_at) FROM delegation_grants ORDER BY grant_hash",
            "SELECT json_array(singleton,version) FROM delegation_schema",
        ] {
            out.extend(
                c.prepare(query)
                    .expect("snapshot query")
                    .query_map([], |r| r.get::<_, String>(0))
                    .expect("snapshot rows")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("snapshot"),
            )
        }
        out
    }
    fn fixture() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().expect("fixture");
        c.execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY);")
            .expect("dependency");
        c.execute_batch(include_str!("../../src/store/delegation.sql"))
            .expect("delegation");
        c.pragma_update(None, "user_version", 133).expect("ordinal");
        c
    }
    #[test]
    fn empty_delegation_rewind_is_strict_and_preserves_user_data() {
        let empty = fixture();
        rewind_empty_delegation_to_v132(&empty);
        let ordinal: i64 = empty
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .expect("ordinal");
        assert_eq!(ordinal, 132);
        for mutation in [
            "INSERT INTO delegation_clients(id,name,secret_hash,redirect) VALUES('keep','keep','hash','https://keep')",
            "INSERT INTO delegation_codes VALUES('keep','client',1,1,'subject','source','challenge','redirect',9999999999,'')",
            "INSERT INTO delegation_grants VALUES('keep','code','client',1,1,'subject','source',9999999999)",
            "DELETE FROM delegation_schema",
            "DROP INDEX delegation_codes_expiry",
            "CREATE INDEX alien ON delegation_clients(name)",
        ] {
            let c = fixture();
            if mutation.starts_with("INSERT INTO delegation_codes")||mutation.starts_with("INSERT INTO delegation_grants") {
                c.execute_batch("INSERT INTO users(id) VALUES(1);INSERT INTO delegation_clients(id,name,secret_hash,redirect) VALUES('client','Curator','hash','https://curator.example/api/v1/auth/cinema/callback');").expect("valid populated fixture parents");
            }
            c.execute_batch(mutation).expect("refusal fixture");
            let before = snapshot(&c);
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                within_fixture_savepoint(&c, || rewind_empty_delegation_to_v132(&c))
            }));
            assert!(refused.is_err());
            let after = snapshot(&c);
            assert_eq!(before, after, "refusal must not mutate fixture");
        }
    }
}
