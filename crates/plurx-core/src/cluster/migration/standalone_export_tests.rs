use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().expect("conversion fixture");
    let source = directory.path().join("source.sqlite");
    let target = directory.path().join("native.sqlite");
    let connection = Connection::open(&source).expect("source");
    connection.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at INTEGER NOT NULL) STRICT;
        INSERT INTO settings VALUES('instance.id','same-server',17);
        CREATE TABLE records(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,amount REAL,bytes BLOB,missing TEXT) STRICT;
        INSERT INTO records VALUES(5,'one',1.5,x'0001ff',NULL);
        INSERT INTO records VALUES(90,'deleted',NULL,NULL,NULL);
        DELETE FROM records WHERE id=90;
        CREATE TABLE item_identity_watermark(singleton INTEGER PRIMARY KEY,high_water INTEGER NOT NULL,importing INTEGER NOT NULL) STRICT;
        INSERT INTO item_identity_watermark VALUES(1,12345,0);
        CREATE TABLE effects(singleton INTEGER PRIMARY KEY,revision INTEGER NOT NULL) STRICT;
        INSERT INTO effects VALUES(1,42);
        CREATE TABLE cluster_meta(singleton INTEGER PRIMARY KEY,schema_version INTEGER,protocol_min INTEGER,protocol_max INTEGER,migrated_at INTEGER) STRICT;")
        .expect("source schema");
    connection
        .execute(
            "INSERT INTO cluster_meta VALUES(1,?1,1,1,0)",
            [AUTH_SCHEMA_VERSION],
        )
        .expect("source version");
    drop(connection);
    let connection = Connection::open(&target).expect("target");
    // Different physical column order proves that data parity is by column
    // identity, rather than by the order of columns in either backend's DDL.
    connection.execute_batch("CREATE TABLE settings(updated_at INTEGER NOT NULL,key TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
        CREATE TABLE records(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,amount REAL,bytes BLOB,missing TEXT) STRICT;
        CREATE TABLE item_identity_watermark(singleton INTEGER PRIMARY KEY,high_water INTEGER NOT NULL,importing INTEGER NOT NULL) STRICT;
        INSERT INTO item_identity_watermark VALUES(1,0,0);
        CREATE TABLE effects(singleton INTEGER PRIMARY KEY,revision INTEGER NOT NULL) STRICT;
        INSERT INTO effects VALUES(1,0);
        CREATE TRIGGER write_effect AFTER INSERT ON records BEGIN UPDATE effects SET revision=revision+1; END;")
        .expect("target schema");
    connection
        .pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION)
        .expect("native version");
    drop(connection);
    (directory, source, target)
}

fn predecessor_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().expect("closed predecessor directory");
    let source = directory.path().join("source.sqlite");
    let target = directory.path().join("native.sqlite");
    crate::store::SqliteStore::create_native_pre_source_import_target(&source)
        .expect("production predecessor application schema");
    crate::store::SqliteStore::create_native_pre_source_import_target(&target)
        .expect("fresh predecessor native target");
    let connection = Connection::open(&source).expect("predecessor source");
    connection.execute_batch("CREATE TABLE cluster_meta(singleton INTEGER PRIMARY KEY,schema_version INTEGER NOT NULL);
        INSERT INTO cluster_meta VALUES(1,81);
        INSERT INTO settings(key,value,updated_at) VALUES('instance.id','same-server',17) ON CONFLICT(key) DO UPDATE SET value=excluded.value;").expect("frozen replicated predecessor marker fixture");
    (directory, source, target)
}

#[test]
fn closed_predecessor_copies_local_history_without_enabling_or_faking_source() {
    let (_directory, source, target) = predecessor_fixture();
    let connection = Connection::open(&source).expect("predecessor source");
    assert_eq!(
        validate_schema_cohort(&connection).expect("exact closed predecessor"),
        crate::store::AUTH_SCHEMA_BASELINE_VERSION
    );
    connection.execute("INSERT INTO users(id,username,password_hash,is_admin,created_at) VALUES(7,'retained-user','retained-hash',0,1)",[]).expect("retained local user");
    connection.execute("INSERT INTO media_session_requests(user_id,request_id,request_fingerprint,playback_id,state,claim_expires_at_ms,incarnation_id,owner_node_id,response_json,updated_at_ms) VALUES(7,'retained-request',?1,'retained-playback','resolved',100,?2,'same-node','{}',101)",rusqlite::params!["a".repeat(64),uuid::Uuid::new_v4().to_string()]).expect("retained local request history");
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .expect("close the exact immutable source cut");
    drop(connection);
    let before = super::super::sha256_file(&source).expect("immutable predecessor hash");
    copy_image(&source, &target).expect("strict predecessor row parity");
    assert_eq!(
        before,
        super::super::sha256_file(&source).expect("source remained immutable")
    );
    let target = Connection::open(&target).expect("native predecessor target");
    assert_eq!(
        target
            .query_row(
                "SELECT user_id,request_id,state,updated_at_ms FROM media_session_requests",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?
                ))
            )
            .expect("exact retained local request"),
        (7, "retained-request".into(), "resolved".into(), 101)
    );
    let owner_column:i64=target.query_row("SELECT count(*) FROM pragma_table_info('media_session_requests') WHERE name='owner_key'",[],|row|row.get(0)).expect("predecessor remains a real predecessor");
    assert_eq!(owner_column, 0);
    assert!(!table_exists(&target, "main", "cluster_meta").expect("no replicated marker"));
    assert!(
        !table_exists(&target, "main", "sharing_source_schema_installation")
            .expect("no Source installation proof")
    );
    let connection = Connection::open(&source).expect("original immutable predecessor");
    assert_eq!(
        connection
            .query_row("SELECT schema_version FROM cluster_meta", [], |row| row
                .get::<_, i64>(0))
            .expect("former authority unchanged"),
        81
    );
    connection
        .execute("UPDATE cluster_meta SET schema_version=82", [])
        .expect("false current marker fixture");
    assert!(
        validate_schema_cohort(&connection).is_err(),
        "marker alone cannot invent current Source layout"
    );
    connection
        .execute("UPDATE cluster_meta SET schema_version=80", [])
        .expect("unsupported predecessor fixture");
    assert!(
        validate_schema_cohort(&connection).is_err(),
        "older or unknown schemas do not qualify by numeric range"
    );
}

#[test]
fn predecessor_conversion_preserves_classification_overrides_and_reconstructs_exact_known_fts() {
    let (_directory, source, target) = predecessor_fixture();
    let connection = Connection::open(&source).expect("classified predecessor");
    connection.execute_batch("INSERT INTO libraries(id,name,kind,paths,created_at) VALUES(11,'retained-library','movies','[]',1);
        INSERT INTO items(id,library_id,kind,title,sort_title,overview,genres,tags,added_at,updated_at) VALUES(27,11,'movie','Authoritymigration heist','Authoritymigration heist','A preserved classification','[\"Crime\"]','[\"manual tag\"]',1,2);
        CREATE VIRTUAL TABLE items_fts_vocab USING fts5vocab(items_fts,'instance');").expect("actual classified catalogue");
    let source_json: String = connection
        .query_row(
            &format!(
                "SELECT {} FROM items i WHERE i.id=27",
                crate::store::classification::SOURCE
            ),
            [],
            |row| row.get(0),
        )
        .expect("canonical classification source");
    let input: crate::store::classification::Input =
        serde_json::from_str(&source_json).expect("actual classifier input");
    let classification =
        crate::metadata::classification::classify(&input.metadata(), vec!["heist".into()]);
    let overrides = crate::metadata::classification::Overrides {
        include: vec!["manual:favorite".into()],
        exclude: vec!["genre:war".into()],
    };
    let record = crate::store::classification::Record {
        source_json: source_json.clone(),
        classification,
        overrides,
        revision: 23,
    };
    let payload = serde_json::to_string(&record.classification).expect("classification payload");
    let overrides = serde_json::to_string(&record.overrides).expect("manual corrections");
    let terms = crate::store::classification::terms(&record);
    connection.execute("INSERT INTO media_classifications(item_id,source_json,payload,overrides,terms,revision) VALUES(27,?1,?2,?3,?4,23)",rusqlite::params![source_json,payload,overrides,terms]).expect("actual persisted classification");
    // An incomplete generated index is derived data; preserve the durable
    // classification and rebuild using the production metadata fence.
    connection
        .execute("DELETE FROM classification_fts", [])
        .expect("stale derived index fixture");
    validate_schema_cohort(&connection)
        .expect("actual closed predecessor with classification data");
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .expect("closed classifier source cut");
    drop(connection);
    let report = copy_image(&source, &target).expect("classification transfer");
    for name in [
        "classification_fts",
        "classification_fts_config",
        "classification_fts_content",
        "items_fts_vocab",
    ] {
        assert_eq!(
            report
                .iter()
                .find(|entry| entry.table == name)
                .expect("complete FTS census")
                .disposition,
            "rebuilt-derived-index"
        );
    }
    let converted = Connection::open(&target).expect("converted native classification");
    let stored:(String,String,String,String,i64)=converted.query_row("SELECT source_json,payload,overrides,terms,revision FROM media_classifications WHERE item_id=27",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).expect("retained classification and corrections");
    assert_eq!(stored, (source_json, payload, overrides, terms, 23));
    assert_eq!(
        converted
            .query_row(
                "SELECT count(*) FROM classification_fts WHERE classification_fts MATCH 'favorite'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("reconstructed correction search"),
        1
    );
    assert_eq!(
        converted
            .query_row(
                "SELECT count(*) FROM items_fts WHERE items_fts MATCH 'Authoritymigration'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("reconstructed item search"),
        1
    );
    let connection = Connection::open(&source).expect("immutable classification image");
    let mut unknown = inventory(&connection, "main").expect("source inventory")
        ["classification_fts_config"]
        .clone();
    unknown.kind = "shadow".into();
    assert!(
        !derived_table("classification_fts_future_custody", &unknown),
        "known prefixes cannot discard new durable shadow tables"
    );
}

#[test]
fn live_source_wal_refuses_before_modifying_the_native_destination() {
    let (_directory, source, target) = predecessor_fixture();
    let live = Connection::open(&source).expect("live writer fixture");
    live.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO users(id,username,password_hash,is_admin,created_at) VALUES(17,'wal-user','retained-hash',0,1);").expect("committed source WAL row");
    let before = super::super::sha256_file(&target).expect("destination before refusal");
    assert!(copy_image(&source, &target)
        .expect_err("immutable attach must not bypass live WAL")
        .to_string()
        .contains("closed immutable image"));
    assert_eq!(
        before,
        super::super::sha256_file(&target).expect("destination unchanged")
    );
    assert_eq!(
        live.query_row("SELECT count(*) FROM users WHERE id=17", [], |row| row
            .get::<_, i64>(0))
            .expect("committed WAL data remains authoritative"),
        1
    );
}

#[test]
fn logical_conversion_preserves_all_types_row_identity_and_deleted_high_water() {
    let (_directory, source, target) = fixture();
    let before = super::super::sha256_file(&source).expect("source hash");
    let report = copy_image(&source, &target).expect("convert current compatible data");
    let connection = Connection::open(&target).expect("converted target");
    let row: (i64, String, f64, Vec<u8>, Option<String>) = connection
        .query_row(
            "SELECT rowid,name,amount,bytes,missing FROM records",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("typed row");
    assert_eq!(row, (5, "one".into(), 1.5, vec![0, 1, 255], None));
    assert_eq!(
        connection
            .query_row(
                "SELECT high_water FROM item_identity_watermark",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("allocator"),
        12345
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name='records'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("sequence"),
        90
    );
    assert_eq!(
        connection
            .query_row("SELECT revision FROM effects", [], |row| row
                .get::<_, i64>(0))
            .expect("counter"),
        42,
        "bulk copy must not execute application publication triggers"
    );
    connection
        .execute("INSERT INTO records(name) VALUES('after conversion')", [])
        .expect("ordinary native mutation");
    assert_eq!(connection.last_insert_rowid(), 91);
    assert_eq!(
        connection
            .query_row("SELECT revision FROM effects", [], |row| row
                .get::<_, i64>(0))
            .expect("counter"),
        43,
        "native triggers must be restored"
    );
    assert_eq!(
        before,
        super::super::sha256_file(&source).expect("unchanged source")
    );
    let metadata = report
        .iter()
        .find(|entry| entry.table == "cluster_meta")
        .expect("classified metadata");
    assert_eq!(metadata.disposition, "retired-cluster-metadata");
    assert_eq!(metadata.source_rows, 1);
    assert!(!table_exists(&connection, "main", "cluster_meta").expect("native authority"));
}

#[test]
fn logical_conversion_preserves_nonempty_dispatch_demand_fairness_and_verdict() {
    let (_directory, source, target) = fixture();
    for path in [&source, &target] {
        let connection = Connection::open(path).expect("dispatch conversion database");
        connection
            .execute_batch("CREATE TABLE background_jobs(id TEXT PRIMARY KEY);")
            .expect("dispatch parent");
        for sql in include_str!("../../store/background_job_turns.sql").split("-- next statement") {
            if sql.contains("CREATE TABLE dispatch_turn_") {
                connection
                    .execute_batch(sql)
                    .expect("published dispatch table");
            }
        }
    }
    let connection = Connection::open(&source).expect("retained dispatch source");
    connection.execute_batch(r#"
        INSERT INTO background_jobs VALUES('dispatch-job');
        INSERT INTO dispatch_turn_ready VALUES('dispatch-job','fixture-node',7,9999,'["/library"]');
        INSERT INTO dispatch_turn_wait VALUES('maintenance','retained','["source_io"]',1,1000,7);
        INSERT INTO dispatch_turn_attempt VALUES(1,'analysis','attempt','["source_io"]',10,1,0,NULL,0);
    "#).expect("nonempty dispatch state");
    drop(connection);
    let before = super::super::sha256_file(&source).expect("immutable source hash");
    let report = copy_image(&source, &target).expect("preserve dispatch state");
    let connection = Connection::open(&target).expect("native dispatch state");
    let tables = inventory(&connection, "main").expect("native table inventory");
    for name in [
        "dispatch_turn_ready",
        "dispatch_turn_wait",
        "dispatch_turn_attempt",
    ] {
        let entry = report
            .iter()
            .find(|entry| entry.table == name)
            .expect("dispatch receipt entry");
        assert_eq!(entry.disposition, "copied-native-data");
        assert_eq!(entry.source_rows, 1);
        assert_eq!(
            table_digest(&connection, "main", name, &tables[name]).expect("native parity"),
            (entry.source_rows, entry.source_sha256.clone())
        );
    }
    assert_eq!(
        before,
        super::super::sha256_file(&source).expect("unchanged source")
    );
}

#[test]
fn nonempty_unmapped_domain_data_refuses_before_mutating_native_target() {
    for extra in [
        "CREATE TABLE unfamiliar_domain(value TEXT); INSERT INTO unfamiliar_domain VALUES('must survive');",
        "CREATE TABLE sharing_purpose_key_archive(envelope BLOB); INSERT INTO sharing_purpose_key_archive VALUES(x'0f00');",
        "ALTER TABLE records ADD COLUMN future_fact TEXT; UPDATE records SET future_fact='must survive';",
    ] {
        let (_directory,source,target)=fixture();
        Connection::open(&source).expect("source").execute_batch(extra).expect("future data");
        let before=super::super::sha256_file(&target).expect("target before refusal");
        assert!(copy_image(&source,&target).is_err());
        assert_eq!(before,super::super::sha256_file(&target).expect("target after refusal"));
    }
}

#[test]
fn identity_bearing_target_is_not_an_import_destination() {
    let (_directory, source, target) = fixture();
    Connection::open(&target)
        .expect("target")
        .execute(
            "INSERT INTO settings VALUES(1,'instance.id','another-server')",
            [],
        )
        .expect("target identity");
    let error = copy_image(&source, &target).expect_err("must not overwrite an initialized server");
    assert!(error.to_string().contains("already an initialized server"));
}

#[test]
fn removed_peer_history_still_refuses_a_singleton_conversion() {
    let (_directory, source, _target) = predecessor_fixture();
    let connection = Connection::open(&source).expect("source");
    connection.execute_batch("CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,raft_id INTEGER,removed_at INTEGER);
        INSERT INTO cluster_nodes VALUES('same-node',1,NULL),('removed-peer',2,100);")
        .expect("membership history");
    let error = validate_source(&connection, "same-server", "same-node", 1)
        .expect_err("removed members are historical peers");
    assert!(error.to_string().contains("removed peer history"));
}

#[test]
fn unknown_empty_table_is_explicitly_recorded_instead_of_silently_omitted() {
    let (_directory, source, target) = fixture();
    Connection::open(&source)
        .expect("source")
        .execute_batch("CREATE TABLE future_empty_domain(value TEXT);")
        .expect("empty extension");
    let report = copy_image(&source, &target).expect("no unclassified durable values");
    let empty = report
        .iter()
        .find(|entry| entry.table == "future_empty_domain")
        .expect("complete census");
    assert_eq!(empty.disposition, "empty-source-only-table");
    assert_eq!(empty.source_rows, 0);
}

#[test]
fn ambiguous_and_restored_membership_history_is_not_a_private_singleton_proof() {
    let (_directory, source, _target) = predecessor_fixture();
    let connection = Connection::open(&source).expect("source");
    assert!(validate_source(&connection, "same-server", "same-node", 1)
        .expect_err("missing history is not evidence of no peers")
        .to_string()
        .contains("history is ambiguous"));
    connection.execute_batch("CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,raft_id INTEGER,removed_at INTEGER); INSERT INTO cluster_nodes VALUES('same-node',1,NULL);")
        .expect("local history");
    validate_source(&connection, "same-server", "same-node", 1).expect("complete private history");
    connection
        .execute(
            "INSERT INTO settings VALUES(?1,'100',100)",
            [crate::store::keys::CLUSTER_RESTORE_GENERATION],
        )
        .expect("restore provenance");
    assert!(validate_source(&connection, "same-server", "same-node", 1)
        .expect_err("restore can clear old peer rows but cannot prove private origin")
        .to_string()
        .contains("restored cluster history"));
}

/// Canonical current protocol DDL only. These are isolated stored metadata
/// fixtures, never live member observations or Source admission permissions.
fn install_current_sharing_protocol_fixture(connection: &Connection) {
    // Closed base declarations and additive role columns used by the real
    // membership schema. Install every table the canonical protocol triggers
    // attach to, and every referenced column, without creating member rows.
    connection.execute_batch("CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY,raft_id INTEGER NOT NULL UNIQUE CHECK(raft_id>0),raft_address TEXT NOT NULL,api_address TEXT NOT NULL,last_seen_at INTEGER NOT NULL,removed_at INTEGER) STRICT;
        ALTER TABLE cluster_nodes ADD COLUMN role TEXT;
        CREATE TABLE cluster_join_tokens (token_hash TEXT PRIMARY KEY,raft_id INTEGER NOT NULL CHECK(raft_id>0),expires_at INTEGER NOT NULL,state TEXT NOT NULL CHECK(state IN ('issued','redeeming','redeemed')),node_id TEXT,created_at INTEGER NOT NULL,redeemed_at INTEGER) STRICT;
        ALTER TABLE cluster_join_tokens ADD COLUMN role TEXT;
        CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY,attempt_id TEXT NOT NULL,barrier_index INTEGER,started_at INTEGER NOT NULL) STRICT;
        CREATE TABLE cluster_node_capabilities (node_id TEXT NOT NULL,capability TEXT NOT NULL,last_seen_at INTEGER NOT NULL,PRIMARY KEY(node_id,capability)) STRICT;
        CREATE TABLE cluster_node_heartbeat_intents (node_id TEXT PRIMARY KEY,last_seen_at INTEGER NOT NULL) STRICT;
        CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY) STRICT;").expect("canonical isolated control dependencies");
    for statement in crate::cluster::membership::sharing_member_admission_guard_schema() {
        connection
            .execute_batch(&statement)
            .expect("canonical current sharing protocol declaration");
    }
    connection
        .execute_batch(crate::cluster::membership::PURPOSE_BOOTSTRAP_GUARD_SCHEMA)
        .expect("canonical persistent bootstrap assertion");
    connection
        .execute(
            "INSERT INTO cluster_sharing_purpose_bootstrap_guard VALUES(1,1)",
            [],
        )
        .expect("stored completed assertion fixture");
    connection.execute("UPDATE cluster_sharing_membership_generation SET generation=generation+1 WHERE singleton=1",[]).expect("retained observation lineage");
}

#[test]
fn current_source_protocol_census_retires_only_observations_and_preserves_grant_generations() {
    let (_directory, source, target) = predecessor_fixture();
    let connection = Connection::open(&source).expect("protocol source");
    install_current_sharing_protocol_fixture(&connection);
    validate_sharing_protocol_retirement(&connection)
        .expect("closed and settled current protocol cohort");
    connection.execute_batch("INSERT INTO sharing_invitations(id,token_hash,library_ids_json,created_at_ms,expires_at_ms,state) VALUES('retained-invitation','invitation-hash','[]',1,2,'open');
        INSERT INTO sharing_exports VALUES('retained-grant','retained-invitation','retained-recipient','Recipient','retained-token-hash',7,9,11,13,'active',2,1,1);").expect("durable grant and domain generations");
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .expect("closed protocol image");
    drop(connection);
    let before = super::super::sha256_file(&source).expect("immutable protocol cut");
    let report = copy_image(&source, &target).expect("settled protocol retirement");
    for table in SHARING_PROTOCOL_TABLES {
        let entry = report
            .iter()
            .find(|entry| entry.table == table)
            .expect("complete protocol census");
        assert_eq!(entry.disposition, "retired-cluster-metadata");
        assert_eq!(entry.source_sha256.len(), 64);
        assert_eq!(
            entry.source_rows,
            u64::from(
                table == "cluster_sharing_membership_generation"
                    || table == "cluster_sharing_purpose_bootstrap_guard"
            )
        );
    }
    assert_eq!(
        before,
        super::super::sha256_file(&source).expect("former authority unchanged")
    );
    let converted = Connection::open(&target).expect("native domain data");
    assert_eq!(converted.query_row("SELECT scope_generation,credential_generation,catalogue_generation,mutation_generation,state,token_hash FROM sharing_exports WHERE id='retained-grant'",[],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?))).expect("all domain generations retained"),(7,9,11,13,"active".into(),"retained-token-hash".into()));
    for table in SHARING_PROTOCOL_TABLES {
        assert!(!table_exists(&converted, "main", table).expect("native metadata inventory"));
    }
}

#[test]
fn source_protocol_pending_operations_and_unclassified_schema_extensions_refuse() {
    let (_directory, source, _target) = predecessor_fixture();
    let connection = Connection::open(&source).expect("pending protocol source");
    // Isolate protocol retirement even before any Source factory installed
    // purpose material. These are the fixture's empty tables only.
    connection.execute_batch("DROP TABLE sharing_catalogue_keys; DROP TABLE sharing_file_locator_keys; DROP TABLE sharing_purpose_key_archive; DROP TABLE sharing_purpose_key_installation; DROP TABLE sharing_purpose_transaction_guard;").expect("pre-installation protocol fixture");
    install_current_sharing_protocol_fixture(&connection);
    let capability = crate::cluster::membership::SHARING_PURPOSE_KEYS_CAPABILITY;
    for (begin,settle) in [
        (format!("INSERT INTO cluster_sharing_join_intents VALUES('{}','{capability}',1)","a".repeat(64)),"DELETE FROM cluster_sharing_join_intents"),
        (format!("INSERT INTO cluster_sharing_join_declarations VALUES('{}','pending-node',2,'http://127.0.0.1:1','http://127.0.0.1:2','{capability}',1)","b".repeat(64)),"DELETE FROM cluster_sharing_join_declarations"),
        ("INSERT INTO cluster_sharing_membership_intents VALUES(2,'pending-node','pending-attempt','learner','http://127.0.0.1:1','http://127.0.0.1:2',1)".into(),"DELETE FROM cluster_sharing_membership_intents"),
    ] {
        connection.execute_batch(&begin).expect("unsettled protocol fixture");
        assert!(validate_sharing_protocol_retirement(&connection).expect_err("pending protocol cannot retire").to_string().contains("pending cluster Source admission"));
        connection.execute_batch(settle).expect("owner settlement fixture");
        validate_sharing_protocol_retirement(&connection).expect("settled protocol");
    }
    connection.execute("CREATE INDEX unclassified_protocol_index ON cluster_sharing_membership_generation(generation)",[]).expect("unknown schema extension");
    assert!(
        validate_sharing_protocol_retirement(&connection).is_err(),
        "new protocol objects require explicit classification"
    );
    connection
        .execute("DROP INDEX unclassified_protocol_index", [])
        .expect("remove fixture extension");
    connection
        .execute(
            "ALTER TABLE cluster_sharing_membership_generation ADD COLUMN durable_domain TEXT",
            [],
        )
        .expect("unknown durable column");
    assert!(
        validate_sharing_protocol_retirement(&connection).is_err(),
        "retirement cannot hide new durable columns"
    );
}

#[test]
fn substituted_bootstrap_view_refuses_before_native_destination_mutation() {
    let (_directory, source, target) = predecessor_fixture();
    let connection = Connection::open(&source).expect("substituted protocol source");
    install_current_sharing_protocol_fixture(&connection);
    connection.execute_batch("DROP TABLE cluster_sharing_purpose_bootstrap_guard; CREATE VIEW cluster_sharing_purpose_bootstrap_guard AS SELECT 1 AS singleton,1 AS passed;").expect("unknown known-name protocol object");
    assert!(validate_sharing_protocol_retirement(&connection)
        .expect_err("a view is not an absent optional guard")
        .to_string()
        .contains("cluster_sharing_purpose_bootstrap_guard"));
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .expect("closed unknown protocol image");
    drop(connection);
    let before = super::super::sha256_file(&target).expect("destination before protocol refusal");
    assert!(copy_image(&source, &target)
        .expect_err("unknown guard definition cannot be retired by name")
        .to_string()
        .contains("cluster_sharing_purpose_bootstrap_guard"));
    assert_eq!(
        before,
        super::super::sha256_file(&target).expect("destination unchanged after refusal")
    );
}
