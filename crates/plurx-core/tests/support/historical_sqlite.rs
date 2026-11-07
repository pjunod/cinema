//! Classify published SQLite extensions before rewinding owned test databases.

pub(super) fn require_fixture_marker(connection: &rusqlite::Connection, table: &str, version: i64) {
    let counts: (i64, i64) = connection
        .query_row(
            &format!("SELECT count(*),COALESCE(sum(singleton=1 AND version=?1),0) FROM {table}"),
            [version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("fixture marker inventory");
    assert_eq!(
        counts,
        (1, 1),
        "one exact published marker required for {table}"
    );
}

pub(super) fn install_source_arbitration_fixture(connection: &rusqlite::Connection) {
    for sql in [
        include_str!("../../src/store/background_jobs_foreground.sql"),
        include_str!("../../src/store/source_checkpoints.sql"),
        include_str!("../../src/store/background_jobs_source_interest.sql"),
        include_str!("../../src/store/background_jobs_source_service.sql"),
    ] {
        connection
            .execute_batch(sql)
            .expect("published source arbitration");
    }
}

/// Plan the removal of the empty source-arbitration (v122) and native (v124)
/// extensions. Use their published DDL to classify every object before
/// counting data; triggers on older tables must also be removed before replay.
pub(super) fn validated_post_v121_rewind(
    connection: &rusqlite::Connection,
    version: i64,
) -> String {
    let witness = rusqlite::Connection::open_in_memory().expect("post-v121 fixture witness");
    if version >= 122 {
        witness.execute_batch("CREATE TABLE background_jobs(id TEXT PRIMARY KEY,state TEXT,fence INTEGER); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); CREATE TABLE background_job_waiters(request_scope TEXT,request_id TEXT,state TEXT,updated_at_ms INTEGER);")
            .expect("source arbitration dependencies");
        install_source_arbitration_fixture(&witness);
    }
    if version >= 124 {
        witness.execute_batch("CREATE TABLE storage_identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), instance_id TEXT NOT NULL, node_id TEXT NOT NULL, generation TEXT NOT NULL) STRICT; CREATE TABLE sharing_local_census (node_id TEXT NOT NULL PRIMARY KEY CHECK(length(node_id) BETWEEN 1 AND 256),attempt_id TEXT NOT NULL CHECK(length(attempt_id)=36),generation INTEGER NOT NULL CHECK(generation>0),claimed_at_ms INTEGER NOT NULL CHECK(claimed_at_ms>0)) STRICT;")
            .expect("published native identity and census");
    }
    let inventory = |db: &rusqlite::Connection| {
        let mut stmt = db.prepare("WITH owned(pattern) AS (VALUES ('foreground_source_*'),('background_source_*'),('source_io_occupancy*'),('analysis_source_checkpoint*'),('playback_analysis_*'),('storage_identity*'),('sharing_local_census*'))
            SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL AND EXISTS (SELECT 1 FROM owned WHERE name GLOB pattern OR tbl_name GLOB pattern)
            ORDER BY CASE type WHEN 'trigger' THEN 0 WHEN 'view' THEN 1 WHEN 'index' THEN 2 ELSE 3 END,name").expect("post-v121 inventory");
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .expect("post-v121 schema rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("complete post-v121 schema")
    };
    let objects = inventory(&witness);
    assert_eq!(
        inventory(connection),
        objects,
        "exact post-v121 extension shape required"
    );
    let mut drops = String::new();
    for (kind, name, _) in objects {
        if kind == "table" {
            if name == "background_source_schema" {
                require_fixture_marker(connection, &name, 1);
            } else {
                let count: i64 = connection
                    .query_row(&format!("SELECT count(*) FROM {name}"), [], |row| {
                        row.get(0)
                    })
                    .expect("post-v121 fixture table inventory");
                assert_eq!(count, 0, "refuse to rewind retained rows in {name}");
            }
        }
        drops.push_str(&format!("DROP {kind} {name};"));
    }
    drops
}
