//! Independently versioned queue extension; existing composed ordinals stay put.
use crate::error::StoreError;
use std::sync::LazyLock;

pub(crate) const SCHEMA: &str = include_str!("subtitle_managed_schema.sql");
pub(crate) const RESOURCE_MIGRATION: &str = include_str!("subtitle_managed_resources_v2.sql");
pub(crate) const RESOURCE_MIGRATION_VERSION: i64 = 120;
pub(crate) const ARTIFACT_MIGRATION: &str = include_str!("subtitle_managed_resources_v3.sql");
pub(crate) const ARTIFACT_MIGRATION_VERSION: i64 = 121;
pub(crate) const PREPARED_MIGRATION: &str = include_str!("subtitle_managed_resources_v4.sql");
pub(crate) const PREPARED_MIGRATION_VERSION: i64 = 123;
pub(crate) const TARGETED_MIGRATION: &str = include_str!("subtitle_managed_resources_v5.sql");
pub(crate) const TARGETED_MIGRATION_VERSION: i64 = 126;
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name IN ('subtitle_managed_schema','subtitle_model_receipts','subtitle_managed_source_changed','subtitle_managed_source_deleted','subtitle_managed_config_write','background_job_required_resources') ORDER BY name";

// The first statement of bootstrap's atomic legacy-probe/managed installation.
// An exact old view and no extension objects must still be committed when the
// transaction runs. A concurrent winner makes this SELECT fail before any DROP;
// the caller accepts only a subsequently verified complete managed winner.
#[cfg(feature = "hiqlite-store")]
pub(super) const BOOTSTRAP_GUARD: &str = "SELECT CASE WHEN \
    (SELECT count(*) FROM sqlite_master WHERE name IN \
    ('subtitle_managed_schema','subtitle_model_receipts','subtitle_managed_source_changed',\
    'subtitle_managed_source_deleted','subtitle_managed_config_write','background_job_required_resources'))=1 \
    AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='background_job_required_resources' AND sql=$1) \
    THEN 1 ELSE json('managed subtitle bootstrap layout changed') END";

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
pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    verify_version(conn, 5)
}
fn verify_version(conn: &rusqlite::Connection, expected: i64) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(expected) {
        return Err(StoreError::Migration(
            "managed subtitle schema layout mismatch".into(),
        ));
    }
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM subtitle_managed_schema WHERE singleton=1 AND version=?1",
        [expected],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(StoreError::Migration(
            "managed subtitle schema marker missing".into(),
        ));
    }
    Ok(())
}

pub(crate) fn verify_prepared_predecessor(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    verify_version(conn, 3)
}
#[cfg(feature = "hiqlite-store")]
pub(crate) const PREPARED_GUARD: &str = "SELECT CASE WHEN EXISTS(SELECT 1 FROM subtitle_managed_schema WHERE singleton=1 AND version=3) AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='background_job_required_resources' AND sql=$1) THEN 1 ELSE json('managed prepared-artifact predecessor changed') END";
#[cfg(feature = "hiqlite-store")]
pub(crate) fn prepared_statements() -> Vec<String> {
    PREPARED_MIGRATION
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
#[cfg(feature = "hiqlite-store")]
pub(crate) fn prepared_predecessor_view() -> String {
    SHAPES
        .4
        .iter()
        .find(|(name, _)| name == "background_job_required_resources")
        .expect("v3 resource view")
        .1
        .clone()
}

pub(crate) fn verify_targeted_predecessor(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    verify_version(conn, 4)
}
#[cfg(feature = "hiqlite-store")]
pub(crate) const TARGETED_GUARD: &str = "SELECT CASE WHEN EXISTS(SELECT 1 FROM subtitle_managed_schema WHERE singleton=1 AND version=4) AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='background_job_required_resources' AND sql=$1) THEN 1 ELSE json('managed targeted-probe predecessor changed') END";
#[cfg(feature = "hiqlite-store")]
pub(crate) fn targeted_statements() -> Vec<String> {
    TARGETED_MIGRATION
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
#[cfg(feature = "hiqlite-store")]
pub(crate) fn targeted_predecessor_view() -> String {
    SHAPES
        .5
        .iter()
        .find(|(name, _)| name == "background_job_required_resources")
        .expect("v4 resource view")
        .1
        .clone()
}

// Called while SQLite holds the migration transaction, before any DDL.
pub(crate) fn verify_resource_predecessor(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration(
            "managed resource migration requires exact v1 layout".into(),
        ));
    }
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM subtitle_managed_schema WHERE singleton=1 AND version=1",
        [],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(StoreError::Migration(
            "managed resource predecessor marker missing".into(),
        ));
    }
    Ok(())
}

// Called under the migration transaction, before replacing exact v2 DDL.
pub(crate) fn verify_artifact_predecessor(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(2)
        || conn.query_row(
            "SELECT count(*) FROM subtitle_managed_schema WHERE singleton=1 AND version=2",
            [],
            |row| row.get::<_, i64>(0),
        )? != 1
    {
        return Err(StoreError::Migration(
            "managed artifact migration requires exact v2 layout".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
pub(crate) fn artifact_statements() -> Vec<String> {
    ARTIFACT_MIGRATION
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(feature = "hiqlite-store")]
pub(crate) const ARTIFACT_GUARD: &str = "SELECT CASE WHEN EXISTS(SELECT 1 FROM subtitle_managed_schema WHERE singleton=1 AND version=2) AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='background_job_required_resources' AND sql=$1) THEN 1 ELSE json('managed artifact predecessor changed') END";

#[cfg(feature = "hiqlite-store")]
pub(crate) fn artifact_predecessor_view() -> String {
    SHAPES
        .3
        .iter()
        .find(|(name, _)| name == "background_job_required_resources")
        .expect("v2 resource view")
        .1
        .clone()
}

#[cfg(feature = "hiqlite-store")]
pub(crate) fn resource_statements() -> Vec<String> {
    RESOURCE_MIGRATION
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(feature = "hiqlite-store")]
pub(crate) const RESOURCE_GUARD: &str = "SELECT CASE WHEN \
    EXISTS(SELECT 1 FROM subtitle_managed_schema WHERE singleton=1 AND version=1) \
    AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='background_job_required_resources' AND sql=$1) \
    THEN 1 ELSE json('managed resource predecessor changed') END";

#[cfg(feature = "hiqlite-store")]
pub(crate) fn resource_predecessor_view() -> String {
    SHAPES
        .1
        .iter()
        .find(|(name, _)| name == "background_job_required_resources")
        .expect("v1 resource view")
        .1
        .clone()
}

fn normalize(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}
// SQLite normalizes IF NOT EXISTS and terminal semicolons when storing DDL.
// Measure that normalization instead of inventing a second SQL parser.
type SchemaShape = Vec<(String, String)>;
static SHAPES: LazyLock<(
    SchemaShape,
    SchemaShape,
    SchemaShape,
    SchemaShape,
    SchemaShape,
    SchemaShape,
    SchemaShape,
)> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("closed schema fixture");
    conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT,target_node_id TEXT); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT);").expect("closed stubs");
    let resources = super::background_jobs_resources::SCHEMA
        .split("-- next statement")
        .find(|sql| {
            sql.trim_start()
                .starts_with("CREATE VIEW IF NOT EXISTS background_job_required_resources")
        })
        .expect("bootstrap resource view");
    conn.execute_batch(resources).expect("bootstrap resources");
    let bootstrap = rows(&conn).expect("bootstrap resource rows");
    conn.execute_batch("DROP VIEW background_job_required_resources")
        .expect("replace bootstrap resources");
    let old = super::background_jobs_probe::SCHEMA
        .split("CREATE VIEW IF NOT EXISTS background_job_required_resources")
        .nth(1)
        .expect("historical view");
    conn.execute_batch(&format!(
        "CREATE VIEW IF NOT EXISTS background_job_required_resources{old}"
    ))
    .expect("historical resources");
    let before = rows(&conn).expect("historical rows");
    conn.execute_batch(SCHEMA).expect("managed schema");
    let managed = rows(&conn).expect("managed rows");
    conn.execute_batch(RESOURCE_MIGRATION)
        .expect("resource migration");
    let v2 = rows(&conn).expect("managed v2 rows");
    conn.execute_batch(ARTIFACT_MIGRATION)
        .expect("artifact migration");
    let v3 = rows(&conn).expect("managed v3 rows");
    conn.execute_batch(PREPARED_MIGRATION)
        .expect("prepared artifact migration");
    let v4 = rows(&conn).expect("managed v4 rows");
    conn.execute_batch(TARGETED_MIGRATION)
        .expect("targeted probe migration");
    (
        before,
        managed,
        bootstrap,
        v2,
        v3,
        v4,
        rows(&conn).expect("managed v5 rows"),
    )
});
// Bootstrap also recognizes the exact pre-probe view it just installed. This
// does not broaden the public extension version predicate: open/migrate must
// still see either the canonical post-probe predecessor or the complete winner.
#[cfg(feature = "hiqlite-store")]
pub(super) fn bootstrap_version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let canonical = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(name, sql)| (name.clone(), normalize(sql)))
            .collect::<Vec<_>>()
    };
    if canonical(shape) == canonical(&SHAPES.2) {
        Ok(None)
    } else {
        version(shape)
    }
}

pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let canonical = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(name, sql)| (name.clone(), normalize(sql)))
            .collect::<Vec<_>>()
    };
    if canonical(shape) == canonical(&SHAPES.0) {
        Ok(None)
    } else if canonical(shape) == canonical(&SHAPES.1) {
        Ok(Some(1))
    } else if canonical(shape) == canonical(&SHAPES.3) {
        Ok(Some(2))
    } else if canonical(shape) == canonical(&SHAPES.4) {
        Ok(Some(3))
    } else if canonical(shape) == canonical(&SHAPES.5) {
        Ok(Some(4))
    } else if canonical(shape) == canonical(&SHAPES.6) {
        Ok(Some(5))
    } else {
        Err(StoreError::Migration(
            "managed subtitle schema is partial or incompatible".into(),
        ))
    }
}

#[cfg(all(test, feature = "hiqlite-store"))]
mod tests {
    use super::*;
    use crate::store::SqliteStore;
    fn queued(conn: &rusqlite::Connection, id: &str, kind: &str, payload: &str) {
        conn.execute("INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms) VALUES(?1,?2,1,?3,?1,0,'queued',1000,1000,1000)",rusqlite::params![id,kind,payload]).expect("queued job");
        conn.execute("INSERT INTO background_job_waiters(request_scope,request_id,request_digest,job_id,consumer_kind,consumer_ref,priority,state,receipt_expires_ms,created_at_ms,updated_at_ms) VALUES('resource-regression',?1,'digest',?1,'analysis',?1,0,'pending',100000,1000,1000)",[id]).expect("pending waiter");
    }
    fn claim_raw(conn: &rusqlite::Connection, id: &str, kind: &str) -> bool {
        use rusqlite::OptionalExtension;
        conn.execute_batch(super::super::background_jobs_resources::SOURCE_ARBITRATION_SCHEMA)
            .expect("current source authority for current claim query");
        let body = serde_json::json!({"job_id":id,"expected_revision":0,"node_id":"node-a","boot_id":"boot","claim_id":format!("claim-{id}"),"kind":kind,"payload_version":1,"now_ms":1001,"dispatched_at_ms":1001});
        conn.query_row(
            &format!("{} RETURNING id", super::super::background_jobs::CLAIM_SQL),
            [body.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .expect("resource claim")
        .is_some()
    }
    #[test]
    fn text_artifact_upgrade_preserves_readers_and_legacy_worker_fences() {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 120).expect("deployed v2");
        conn.execute_batch("INSERT INTO users(id,username,password_hash,is_admin) VALUES(1,'admin','unused',1); INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[]'); INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','Movie','Movie'); INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(1,1,'/media/movie',100,20,'{}'); INSERT INTO settings(key,value) VALUES('subtitles.text_translation.config.v1','{\"version\":1,\"enabled\":true}');").expect("current source and authority");
        queued(
            &conn,
            "metadata",
            "metadata_refresh",
            r#"{"kind":"metadata_refresh"}"#,
        );
        assert!(claim_raw(&conn, "metadata", "metadata_refresh"));
        for (id, kind) in [
            ("legacy", "subtitle_text_translate"),
            ("artifact", "subtitle_text_translate_artifact"),
        ] {
            queued(&conn, id, kind, &serde_json::json!({"kind":kind,"file_id":1,"source_size":100,"source_mtime":20,"target_node_id":"node-a","admin_user_id":1}).to_string());
            assert!(!claim_raw(&conn, id, kind), "v2 fences every text reader");
        }
        let before: String = conn.query_row("SELECT json_group_array(json_object('resource',resource_key,'slot',slot,'job',job_id,'fence',fence,'expiry',expires_at_ms)) FROM background_job_reservations", [], |row|row.get(0)).expect("active reservation");
        let owner: String = conn.query_row("SELECT json_object('state',state,'revision',revision,'claim',claim_id,'fence',fence,'expiry',lease_expires_ms) FROM background_jobs WHERE id='metadata'", [], |row|row.get(0)).expect("active owner");
        SqliteStore::apply_next_migration_for_test(&conn).expect("v121 upgrade");
        verify_version(&conn, 3).expect("exact v3 DDL and marker");
        assert_eq!(before, conn.query_row("SELECT json_group_array(json_object('resource',resource_key,'slot',slot,'job',job_id,'fence',fence,'expiry',expires_at_ms)) FROM background_job_reservations", [], |row|row.get::<_,String>(0)).expect("unchanged reservations"));
        assert_eq!(owner, conn.query_row("SELECT json_object('state',state,'revision',revision,'claim',claim_id,'fence',fence,'expiry',lease_expires_ms) FROM background_jobs WHERE id='metadata'", [], |row|row.get::<_,String>(0)).expect("unchanged owner"));
        assert!(
            !claim_raw(&conn, "legacy", "subtitle_text_translate"),
            "legacy extraction-capable workers stay source-fenced"
        );
        assert!(
            !claim_raw(&conn, "artifact", "subtitle_text_translate"),
            "old worker kind cannot claim a new job"
        );
        assert!(claim_raw(
            &conn,
            "artifact",
            "subtitle_text_translate_artifact"
        ));
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM background_job_reservations WHERE job_id='artifact'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("artifact reservations"),
            0
        );
        for (id, kind) in [
            ("media", "fragment_index_build"),
            ("future", "future_source_reader"),
        ] {
            queued(
                &conn,
                id,
                kind,
                &serde_json::json!({"kind":kind}).to_string(),
            );
            assert!(
                !claim_raw(&conn, id, kind),
                "actual and unknown readers remain bounded"
            );
        }
        assert!(
            conn.query_row(ARTIFACT_GUARD, [artifact_predecessor_view()], |row| row
                .get::<_, i64>(0))
                .is_err(),
            "stale v2 proposal cannot overwrite v3"
        );
    }

    #[test]
    fn text_artifact_upgrade_rolls_back_and_refuses_drifted_v2() {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 120).expect("deployed v2");
        let before = rows(&conn).expect("exact v2");
        conn.execute_batch("BEGIN").expect("begin");
        verify_artifact_predecessor(&conn).expect("exact v2 predecessor");
        conn.execute_batch(ARTIFACT_MIGRATION).expect("upgrade");
        verify_version(&conn, 3).expect("exact v3");
        assert!(
            verify_resource_predecessor(&conn).is_err(),
            "old v1 installer refuses v3"
        );
        conn.execute_batch("ROLLBACK").expect("rollback");
        assert_eq!(rows(&conn).expect("restored"), before);
        conn.execute_batch("DROP VIEW background_job_required_resources; CREATE VIEW background_job_required_resources AS SELECT id AS job_id,'source_io' AS resource_key FROM background_jobs").expect("unknown shape");
        let drift = rows(&conn).expect("drifted shape");
        assert!(SqliteStore::apply_next_migration_for_test(&conn).is_err());
        assert_eq!(rows(&conn).expect("unchanged drift"), drift);
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("ordinal"),
            120
        );
    }

    #[test]
    fn text_model_resource_upgrade_preserves_active_leases_and_unblocks_artifact_setup() {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 119).expect("old deployed schema");
        conn.execute(
            "INSERT INTO users(id,username,password_hash,is_admin) VALUES(1,'admin','unused',1)",
            [],
        )
        .expect("admin");
        queued(
            &conn,
            "metadata",
            "metadata_refresh",
            r#"{"kind":"metadata_refresh"}"#,
        );
        assert!(claim_raw(&conn, "metadata", "metadata_refresh"));
        queued(
            &conn,
            "text-model",
            "subtitle_text_model_operation",
            r#"{"kind":"subtitle_text_model_operation","target_node_id":"node-a","admin_user_id":1}"#,
        );
        assert!(
            !claim_raw(&conn, "text-model", "subtitle_text_model_operation"),
            "old view reproduces live queue starvation"
        );
        let before:String=conn.query_row("SELECT json_group_array(json_object('resource',resource_key,'slot',slot,'job',job_id,'fence',fence,'expiry',expires_at_ms)) FROM background_job_reservations",[],|row|row.get(0)).expect("reservations");
        let owner:String=conn.query_row("SELECT json_object('state',state,'revision',revision,'claim',claim_id,'fence',fence,'expiry',lease_expires_ms) FROM background_jobs WHERE id='metadata'",[],|row|row.get(0)).expect("owner");
        SqliteStore::apply_next_migration_for_test(&conn).expect("transactional upgrade");
        verify_artifact_predecessor(&conn).expect("exact historical v2 DDL and marker");
        let after:String=conn.query_row("SELECT json_group_array(json_object('resource',resource_key,'slot',slot,'job',job_id,'fence',fence,'expiry',expires_at_ms)) FROM background_job_reservations",[],|row|row.get(0)).expect("reservations");
        assert_eq!(before, after, "upgrade preserves active reservations");
        assert_eq!(owner,conn.query_row("SELECT json_object('state',state,'revision',revision,'claim',claim_id,'fence',fence,'expiry',lease_expires_ms) FROM background_jobs WHERE id='metadata'",[],|row|row.get::<_,String>(0)).expect("owner"));
        assert!(
            claim_raw(&conn, "text-model", "subtitle_text_model_operation"),
            "artifact download does not need a media reader"
        );
        let source_count:i64=conn.query_row("SELECT count(*) FROM background_job_reservations WHERE job_id='text-model' AND resource_key LIKE 'source_io%'",[],|row|row.get(0)).expect("text resources");
        assert_eq!(source_count, 0);
        for (id, kind) in [
            ("media", "fragment_index_build"),
            ("future", "future_source_reader"),
        ] {
            queued(
                &conn,
                id,
                kind,
                &serde_json::json!({"kind":kind}).to_string(),
            );
            assert!(
                !claim_raw(&conn, id, kind),
                "{kind}: conservative source-reader bound remains"
            );
        }
    }
    #[test]
    fn text_model_resource_upgrade_is_atomic_and_refuses_unknown_predecessors() {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 119).expect("old schema");
        let old = rows(&conn).expect("v1 shape");
        conn.execute_batch("BEGIN").expect("begin");
        verify_resource_predecessor(&conn).expect("predecessor");
        conn.execute_batch(RESOURCE_MIGRATION).expect("upgrade");
        assert_eq!(version(&rows(&conn).expect("shape")).expect("v2"), Some(2));
        assert!(
            conn.query_row(RESOURCE_GUARD, [resource_predecessor_view()], |row| row
                .get::<_, i64>(0))
                .is_err(),
            "stale v1 installer refuses complete v2 winner"
        );
        conn.execute_batch("ROLLBACK").expect("rollback");
        assert_eq!(rows(&conn).expect("restored shape"), old);
        conn.execute_batch("DROP VIEW background_job_required_resources; CREATE VIEW background_job_required_resources AS SELECT id AS job_id,'source_io' AS resource_key FROM background_jobs").expect("unrecognized view");
        assert!(SqliteStore::apply_next_migration_for_test(&conn).is_err());
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .expect("version"),
            119
        );
    }

    #[test]
    fn bootstrap_recognizes_only_exact_resources_probe_and_complete_managed_layouts() {
        assert_eq!(
            bootstrap_version(&SHAPES.0).expect("probe predecessor"),
            None
        );
        assert_eq!(
            bootstrap_version(&SHAPES.2).expect("resources predecessor"),
            None
        );
        assert!(version(&SHAPES.2).is_err(), "ordinary open remains strict");
        assert_eq!(
            bootstrap_version(&SHAPES.1).expect("managed winner"),
            Some(1)
        );
        let mut arbitrary = SHAPES.0.clone();
        arbitrary[0].1 = "CREATE VIEW background_job_required_resources AS SELECT 1".into();
        assert!(bootstrap_version(&arbitrary).is_err());
        let mut partial = SHAPES.1.clone();
        partial.retain(|(name, _)| name != "subtitle_model_receipts");
        assert!(bootstrap_version(&partial).is_err());
    }

    #[test]
    fn bootstrap_transaction_refuses_stale_legacy_observer_before_dropping_managed_view() {
        fn install(conn: &mut rusqlite::Connection, observed: &str) -> rusqlite::Result<()> {
            let txn = conn.transaction()?;
            txn.query_row(BOOTSTRAP_GUARD, [observed], |row| row.get::<_, i64>(0))?;
            txn.execute_batch(super::super::background_jobs_probe::SCHEMA)?;
            txn.execute_batch(SCHEMA)?;
            txn.commit()
        }
        let mut conn = rusqlite::Connection::open_in_memory().expect("bootstrap fixture");
        SqliteStore::apply_migrations_for_test(&conn, 110).expect("legacy queue fixture");
        let observed = rows(&conn).expect("observed predecessor")[0].1.clone();
        let before = rows(&conn).expect("predecessor shape");
        {
            let txn = conn.transaction().expect("interrupted install");
            txn.query_row(BOOTSTRAP_GUARD, [&observed], |row| row.get::<_, i64>(0))
                .expect("unchanged legacy guard");
            txn.execute_batch(super::super::background_jobs_probe::SCHEMA)
                .expect("legacy replacement");
            txn.execute_batch(SCHEMA).expect("staged managed install");
            txn.rollback().expect("interrupted install rolls back");
        }
        assert_eq!(rows(&conn).expect("rolled back shape"), before);
        install(&mut conn, &observed).expect("competing winner commits");
        conn.execute(
            "INSERT INTO subtitle_model_receipts VALUES('node','base',?1)",
            [r#"{"proof":"retained"}"#],
        )
        .expect("populated receipt");
        conn.execute(r#"INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms) VALUES('install','subtitle_model_operation',1,'{"node_id":"node","model_id":"base"}','install',0,'queued',0,1,1)"#, [])
            .expect("populated queued installation");
        let winner = rows(&conn).expect("complete winner shape");
        assert!(
            install(&mut conn, &observed).is_err(),
            "stale snapshot cannot pass the first statement"
        );
        assert_eq!(rows(&conn).expect("winner shape retained"), winner);
        assert_eq!(version(&winner).expect("strict winner"), Some(1));
        let receipt: String = conn.query_row("SELECT receipt_json FROM subtitle_model_receipts WHERE node_id='node' AND model_id='base'", [], |row| row.get(0)).expect("receipt retained");
        assert_eq!(receipt, r#"{"proof":"retained"}"#);
        let queued: (String, i64) = conn
            .query_row(
                "SELECT state,revision FROM background_jobs WHERE id='install'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("queue retained");
        assert_eq!(queued, ("queued".into(), 0));
    }

    #[test]
    fn managed_extension_preserves_old_lineage_and_resource_rows_and_refuses_partial_shape() {
        let conn = rusqlite::Connection::open_in_memory().expect("schema contract fixture");
        SqliteStore::apply_migrations_for_test(&conn, 110).expect("schema contract fixture");
        let old_lineage: String = conn
            .query_row(super::super::schema_lineage::FINGERPRINT_QUERY, [], |row| {
                row.get(0)
            })
            .expect("schema contract fixture");
        assert_eq!(
            version(&rows(&conn).expect("schema contract fixture"))
                .expect("schema contract fixture"),
            None
        );
        // The historical view's definition is unchanged for every historical
        // kind, including provider and media-domain unions.
        let historical_view: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name='background_job_required_resources'",
                [],
                |row| row.get(0),
            )
            .expect("schema contract fixture");
        conn.execute_batch(&historical_view.replace(
            "background_job_required_resources",
            "old_required_resources",
        ))
        .expect("schema contract fixture");
        conn.execute_batch("BEGIN")
            .expect("schema contract fixture");
        conn.execute_batch(SCHEMA).expect("schema contract fixture");
        assert_eq!(
            version(&rows(&conn).expect("schema contract fixture"))
                .expect("schema contract fixture"),
            Some(1)
        );
        let new_lineage: String = conn
            .query_row(super::super::schema_lineage::FINGERPRINT_QUERY, [], |row| {
                row.get(0)
            })
            .expect("schema contract fixture");
        assert_eq!(
            old_lineage, new_lineage,
            "old recognizer inputs remain byte-identical"
        );
        conn.execute_batch("INSERT INTO libraries(id,name,kind,paths) VALUES(1,'a','movies','[\"/media/a\",\"/media/b\"]'),(2,'b','movies','[\"/other\"]'); INSERT INTO background_storage_domains VALUES(1,'/media/a','nas'),(1,'/media/b','nas'),(2,'/other','disk'); INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','A','A'),(2,2,'movie','B','B'); INSERT INTO files(id,item_id,path,size,mtime) VALUES(1,1,'/media/a/a',10,100),(2,2,'/other/b',20,200);").expect("schema contract fixture");
        for (i, kind) in crate::store::background_jobs::JOB_METRIC_KINDS
            .iter()
            .enumerate()
        {
            if kind.starts_with("subtitle_model") {
                continue;
            }
            let payload = format!(
                r#"{{"kind":"{kind}","file_id":{},"library_id":{}}}"#,
                i % 3,
                i % 3
            );
            conn.execute("INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms) VALUES(?1,?2,1,?3,?1,0,'queued',0,0,0)",rusqlite::params![format!("j{i}"),kind,payload]).expect("schema contract fixture");
        }
        let changed:i64=conn.query_row("SELECT (SELECT count(*) FROM (SELECT * FROM old_required_resources EXCEPT SELECT * FROM background_job_required_resources)) + (SELECT count(*) FROM (SELECT * FROM background_job_required_resources EXCEPT SELECT * FROM old_required_resources))",[],|row|row.get(0)).expect("schema contract fixture");
        assert_eq!(
            changed, 0,
            "old binaries read identical resource obligations"
        );
        conn.execute_batch("ROLLBACK")
            .expect("schema contract fixture");
        assert_eq!(
            version(&rows(&conn).expect("schema contract fixture"))
                .expect("schema contract fixture"),
            None,
            "interruption rolls back complete install"
        );
        conn.execute_batch(SCHEMA).expect("schema contract fixture");
        assert_eq!(
            version(&rows(&conn).expect("schema contract fixture"))
                .expect("schema contract fixture"),
            Some(1),
            "replay recognizes complete winner"
        );
        conn.execute_batch("DROP TABLE subtitle_model_receipts")
            .expect("schema contract fixture");
        assert!(
            version(&rows(&conn).expect("schema contract fixture")).is_err(),
            "marker alone cannot certify install"
        );
    }
    #[test]
    fn managed_extension_and_source82_layout_commute_and_startup_replay_detects_drift() {
        for extension_first in [true, false] {
            let conn = rusqlite::Connection::open_in_memory().expect("schema contract fixture");
            SqliteStore::apply_migrations_for_test(&conn, 110).expect("schema contract fixture");
            conn.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE cluster_meta(singleton INTEGER PRIMARY KEY,schema_version INTEGER); INSERT INTO cluster_meta VALUES(1,81);").expect("schema contract fixture");
            // Source arbitration is installed before the managed adjunct in production.
            // Its restore trigger is a required Source81 predecessor dependency.
            conn.execute_batch(super::super::background_jobs_resources::SOURCE_ARBITRATION_SCHEMA)
                .expect("source arbitration prerequisite");
            // Source activation normally follows membership/purpose-key setup.
            // These presence-only fixtures are NOT cluster admission evidence.
            for name in [
                "cluster_nodes",
                "cluster_node_capabilities",
                "cluster_join_tokens",
                "cluster_node_join_staging",
                "cluster_node_removals",
                "cluster_node_removal_attempts",
                "cluster_sharing_membership_intents",
                "cluster_sharing_join_intents",
                "cluster_sharing_join_declarations",
                "cluster_sharing_membership_generation",
            ] {
                conn.execute_batch(&format!("CREATE TABLE {name}(fixture INTEGER)"))
                    .expect("schema contract fixture");
            }
            conn.execute_batch(
                super::super::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
            )
            .expect("schema contract fixture");
            if extension_first {
                conn.execute_batch(SCHEMA).expect("schema contract fixture");
            }
            for statement in super::super::sharing_source_schema::layout_statements() {
                conn.execute_batch(&statement)
                    .unwrap_or_else(|error| panic!("Source layout: {error}: {statement}"));
            }
            conn.execute_batch(super::super::sharing_source_schema::BOOT_INTENTS_SCHEMA)
                .expect("schema contract fixture");
            conn.execute_batch(super::super::sharing_source_schema::INSTALLATION_SCHEMA)
                .expect("schema contract fixture");
            conn.execute_batch(super::super::sharing_source_schema::TRANSACTION_SCHEMA)
                .expect("schema contract fixture");
            conn.execute_batch("UPDATE cluster_meta SET schema_version=82; INSERT INTO sharing_source_schema_installation VALUES(1,71,printf('%064d',0),1); INSERT INTO sharing_source_schema_transaction_guard VALUES(1,1);").expect("schema contract fixture");
            let guard = super::super::sharing_source_schema::installed_guard();
            let source_ready = |conn: &rusqlite::Connection| {
                conn.query_row(
                    &format!("SELECT CASE WHEN ({guard}) THEN 1 ELSE 0 END"),
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("schema contract fixture")
            };
            assert_eq!(
                source_ready(&conn),
                1,
                "actual Source82 installed recognizer accepts layout"
            );
            if !extension_first {
                conn.execute_batch(SCHEMA).expect("schema contract fixture");
            }
            assert_eq!(
                source_ready(&conn),
                1,
                "managed adjunct preserves Source recognizer in either order"
            );
            assert_eq!(
                version(&rows(&conn).expect("schema contract fixture"))
                    .expect("schema contract fixture"),
                Some(1)
            );
            let old = SHAPES.0[0].1.clone();
            conn.execute_batch(&format!(
                "DROP VIEW background_job_required_resources; {old}"
            ))
            .expect("schema contract fixture");
            assert!(
                version(&rows(&conn).expect("schema contract fixture")).is_err(),
                "startup refuses historical view replay under new marker"
            );
        }
    }
    #[test]
    fn prepared_artifact_source_upgrade_keeps_real_readers_and_existing_occupancy() {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 122)
            .expect("deployed source authority with managed v3");
        for kind in [
            "artifact_hydrate",
            "artifact_verify",
            "artwork_derivative",
            "subtitle_text_translate",
            "subtitle_transcribe",
            "dv_title_scan",
        ] {
            queued(
                &conn,
                kind,
                kind,
                &serde_json::json!({"kind":kind}).to_string(),
            );
        }
        let source_count = |kind: &str| {
            conn.query_row(
            "SELECT count(*) FROM background_job_required_resources WHERE job_id=?1 AND resource_key LIKE 'source_io%'",
            [kind], |row| row.get::<_, i64>(0),
        ).expect("source classification")
        };
        assert_eq!(
            source_count("artifact_verify"),
            1,
            "v3 charged prepared integrity bytes"
        );
        conn.execute("INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms) VALUES('source_io',0,'artifact_verify',1,90000)", []).expect("old owned reservation");
        let before: String = conn.query_row("SELECT json_group_array(json_array(resource_key,slot,job_id,fence,expires_at_ms)) FROM background_job_reservations", [], |row|row.get(0)).expect("snapshot prior physical reservations");
        SqliteStore::apply_next_migration_for_test(&conn).expect("v123 prepared classification");
        verify_version(&conn, 4).expect("canonical v4");
        for kind in ["artifact_hydrate", "artifact_verify", "artwork_derivative"] {
            assert_eq!(
                source_count(kind),
                0,
                "prepared artifact is not original source: {kind}"
            );
        }
        for kind in [
            "subtitle_text_translate",
            "subtitle_transcribe",
            "dv_title_scan",
        ] {
            assert_eq!(
                source_count(kind),
                1,
                "original media reader stays charged: {kind}"
            );
        }
        assert_eq!(before,conn.query_row("SELECT json_group_array(json_array(resource_key,slot,job_id,fence,expires_at_ms)) FROM background_job_reservations", [], |row|row.get::<_,String>(0)).expect("read retained physical reservations"), "new classification never fabricates old physical settlement");
    }

    #[test]
    fn targeted_probe_upgrade_drops_only_its_source_reader_and_keeps_occupancy() {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 125).expect("deployed managed v4");
        for (id, kind, generation) in [
            ("targeted", "library_scan", "library-targeted-v1"),
            ("full", "library_scan", "library-work-v1"),
            ("refresh", "metadata_refresh", "library-targeted-v1"),
        ] {
            queued(
                &conn,
                id,
                kind,
                &serde_json::json!({"kind":kind,"library_id":1,"generation":generation})
                    .to_string(),
            );
        }
        let keys = |id: &str| {
            conn.prepare("SELECT resource_key FROM background_job_required_resources WHERE job_id=?1 ORDER BY resource_key")
                .expect("resource query")
                .query_map([id], |row| row.get::<_, String>(0))
                .expect("resource rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("complete resources")
        };
        let charged = ["provider:anilist", "provider:tmdb", "source_io"];
        assert_eq!(
            keys("targeted"),
            charged,
            "v4 charged the targeted probe a reader"
        );
        conn.execute("INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms) VALUES('source_io',0,'targeted',1,90000)", []).expect("old owned reservation");
        let before: String = conn.query_row("SELECT json_group_array(json_array(resource_key,slot,job_id,fence,expires_at_ms)) FROM background_job_reservations", [], |row|row.get(0)).expect("snapshot prior physical reservations");
        SqliteStore::apply_next_migration_for_test(&conn).expect("v126 targeted classification");
        verify_sqlite(&conn).expect("canonical v5");
        assert_eq!(
            keys("targeted"),
            ["provider:anilist", "provider:tmdb"],
            "a targeted probe takes no background source reader"
        );
        assert_eq!(keys("full"), charged, "a full scan keeps its reader");
        assert_eq!(
            keys("refresh"),
            charged,
            "only the library_scan kind of the targeted generation is exempt"
        );
        assert_eq!(before,conn.query_row("SELECT json_group_array(json_array(resource_key,slot,job_id,fence,expires_at_ms)) FROM background_job_reservations", [], |row|row.get::<_,String>(0)).expect("read retained physical reservations"), "new classification never fabricates old physical settlement");
    }

    #[test]
    fn targeted_probe_view_names_the_library_targeted_generation_constant() {
        // The v5 view's SQL literal and the Rust generation are one value.
        let literal = format!(
            "job.generation IS '{}'",
            super::super::background_jobs_library::LIBRARY_TARGETED_GENERATION
        );
        assert!(TARGETED_MIGRATION.contains(&literal), "{literal}");
    }

    #[test]
    fn a_v126_store_reopens_without_touching_the_managed_view_or_marker() {
        let directory = tempfile::tempdir().expect("store directory");
        let path = directory.path().join("store.db");
        drop(SqliteStore::open(&path).expect("fresh v126 store"));
        let snapshot = || {
            let conn = rusqlite::Connection::open(&path).expect("reopen for snapshot");
            verify_sqlite(&conn).expect("canonical v5");
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .expect("ordinal");
            (rows(&conn).expect("managed shape"), version)
        };
        let before = snapshot();
        assert_eq!(before.1, TARGETED_MIGRATION_VERSION);
        drop(SqliteStore::open(&path).expect("restart"));
        assert_eq!(snapshot(), before, "a restart is a no-op at v126/v5");
    }

    #[test]
    fn targeted_probe_upgrade_refuses_drifted_predecessor_without_advancing_ordinal() {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 125).expect("deployed managed v4");
        conn.execute_batch("DROP VIEW background_job_required_resources; CREATE VIEW background_job_required_resources AS SELECT id AS job_id,'source_io' AS resource_key FROM background_jobs;").expect("drift");
        let before = rows(&conn).expect("read drifted predecessor shape");
        assert!(SqliteStore::apply_next_migration_for_test(&conn).is_err());
        assert_eq!(
            before,
            rows(&conn).expect("read shape after refused migration")
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("read unchanged migration ordinal"),
            125
        );
    }

    #[test]
    fn prepared_artifact_upgrade_refuses_drifted_predecessor_without_advancing_ordinal() {
        let conn = rusqlite::Connection::open_in_memory().expect("SQLite");
        SqliteStore::apply_migrations_for_test(&conn, 122).expect("deployed source authority");
        conn.execute_batch("DROP VIEW background_job_required_resources; CREATE VIEW background_job_required_resources AS SELECT id AS job_id,'source_io' AS resource_key FROM background_jobs;").expect("drift");
        let before = rows(&conn).expect("read drifted predecessor shape");
        assert!(SqliteStore::apply_next_migration_for_test(&conn).is_err());
        assert_eq!(
            before,
            rows(&conn).expect("read shape after refused migration")
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("read unchanged migration ordinal"),
            122
        );
    }
}
