//! Source-only fixture helpers shared without discovering extra test bodies.

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

/// Rewind v132 (F+ curve fits) to v131: the exact marker is required and
/// stored fits refuse; the extension's own objects go.
fn rewind_dv_refit_to_v131(connection: &rusqlite::Connection) {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture F+ ordinal");
    if version != 132 {
        return;
    }
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM background_dv_refit_schema WHERE singleton=1 AND version=1",
            [],
            |row| row.get(0),
        )
        .expect("F+ marker");
    assert_eq!(marker, 1, "F+ curve fits require their exact marker");
    let rows: i64 = connection
        .query_row(
            "SELECT (SELECT count(*) FROM background_dv_refit_titles)
              + (SELECT count(*) FROM background_dv_refit_scenes)
              + (SELECT count(*) FROM background_dv_refit_outcomes)",
            [],
            |row| row.get(0),
        )
        .expect("F+ fixture data");
    assert_eq!(rows, 0, "stored F+ fits cannot be rewound");
    let objects: Vec<(String, String)> = connection
        .prepare(
            "SELECT type,name FROM sqlite_master WHERE name GLOB 'background_dv_refit_*'
              AND type IN ('trigger','table') ORDER BY type DESC,name",
        )
        .expect("F+ objects")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("F+ object rows")
        .collect::<Result<_, _>>()
        .expect("F+ object inventory");
    let mut plan: Vec<String> = objects
        .iter()
        .map(|(kind, name)| format!("DROP {} {name};", kind.to_ascii_uppercase()))
        .collect();
    plan.push("PRAGMA user_version=131;".into());
    connection
        .execute_batch(&plan.join("\n"))
        .expect("published F+ rewind");
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

/// Rewind an owned, empty v127-v132 coordination tail to published v126;
/// a fixture already at or below v126 is left alone. The caller owns
/// atomicity (see [`within_fixture_savepoint`]).
pub(crate) fn rewind_empty_coordination_tail_to_v126(connection: &rusqlite::Connection) {
    rewind_dv_refit_to_v131(connection);
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
