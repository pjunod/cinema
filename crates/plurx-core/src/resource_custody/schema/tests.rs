//! Authored upgrade regressions; compiled now, executed only after final review.
use super::*;
use crate::resource_custody::{
    tests::{options, owner, vector},
    *,
};
use rusqlite::{params, types::Value};

fn legacy(connection: &Connection, witness: &storage::Witness) {
    connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    connection.execute_batch(DDL_V1).unwrap();
    connection
        .execute(
            "INSERT INTO journal_meta VALUES(1,1,?1,?2,?3,?4,0,NULL)",
            params![
                witness.journal_id.as_bytes(),
                witness.installation_id.as_bytes(),
                witness.node_id,
                witness.cluster_id
            ],
        )
        .unwrap();
}
fn logical_rows(db: &Connection, table: &str) -> Vec<Vec<Value>> {
    let projection = if table == "journal_meta" {
        "singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id"
    } else {
        "*"
    };
    let mut stmt = db
        .prepare(&format!("SELECT {projection} FROM {table} ORDER BY 1,2"))
        .unwrap();
    let count = stmt.column_count();
    stmt.query_map([], |row| (0..count).map(|i| row.get(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn populated_v1(db: &mut Connection, witness: &storage::Witness) {
    legacy(db, witness);
    let identity = owner();
    transaction::register_owner(db, &identity).unwrap();
    let trial = Uuid::new_v4();
    let mut last_operation = Uuid::nil();
    for index in 0..2 {
        let obligation = Uuid::new_v4();
        let graph = Uuid::new_v4();
        let member_id = Uuid::new_v4();
        let prepared_operation = Uuid::new_v4();
        let cancelled_operation = Uuid::new_v4();
        let plan = PlannedMember {
            member_id,
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: "fixture-domain".into(),
            locator: "old fixed root slot".into(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "no extinction authority".into(),
            },
            owned_by: None,
            depends_on: Vec::new(),
            maximum_cardinality: 1,
            protected_paths: Vec::new(),
        };
        let manifest = Manifest {
            version: 1,
            graph_id: graph,
            owner_id: identity.owner_id,
            attempt_id: None,
            members: vec![plan.clone()],
        };
        let settlement = (index == 1).then(|| {
            codec::encode(&codec::SettlementRecord::PreparedNeverArmed {
                version: 1,
                owner_id: identity.owner_id,
            })
            .unwrap()
        });
        let sequence = if index == 0 { 1 } else { 3 };
        let phase = if index == 0 { "prepared" } else { "settled" };
        last_operation = if index == 0 {
            prepared_operation
        } else {
            cancelled_operation
        };
        // Noncanonical whitespace is legal OLD JSON and must survive byte-exact.
        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        db.execute(
            "INSERT INTO obligations VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,NULL,?11)",
            params![
                obligation.as_bytes(),
                identity.owner_id.as_bytes(),
                graph.as_bytes(),
                u64::MAX.to_le_bytes(),
                codec::vector_bytes(vector(u64::MAX)),
                trial.as_bytes(),
                phase,
                sequence,
                last_operation.as_bytes(),
                manifest_json,
                settlement
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO family_roles VALUES(?1,'video',?2)",
            params![Uuid::new_v4().as_bytes(), obligation.as_bytes()],
        )
        .unwrap();
        let envelope = codec::MemberEnvelope {
            version: 1,
            planned: plan,
            observed: None,
        };
        db.execute(
            "INSERT INTO members VALUES(?1,?2,'root','process_root',1,?3,?4,?5)",
            params![
                obligation.as_bytes(),
                member_id.as_bytes(),
                if index == 0 { "planned" } else { "closed" },
                serde_json::to_string_pretty(&envelope).unwrap(),
                settlement
            ],
        )
        .unwrap();
        let prepared = serde_json::json!({"kind":"prepared","sequence":index+1,"obligations":[obligation],"trial":trial});
        db.execute(
            "INSERT INTO operations VALUES(?1,?2,?3)",
            params![
                prepared_operation.as_bytes(),
                codec::encode(&"0".repeat(64)).unwrap(),
                serde_json::to_string_pretty(&prepared).unwrap()
            ],
        )
        .unwrap();
        if index == 1 {
            let changed = serde_json::json!({"kind":"changed","operation_id":cancelled_operation,"obligation_id":obligation,"graph_id":graph,"sequence":3,"phase":"settled"});
            db.execute(
                "INSERT INTO operations VALUES(?1,?2,?3)",
                params![
                    cancelled_operation.as_bytes(),
                    codec::encode(&"1".repeat(64)).unwrap(),
                    serde_json::to_string_pretty(&changed).unwrap()
                ],
            )
            .unwrap();
        }
    }
    assert!(!last_operation.is_nil());
    db.execute("UPDATE journal_meta SET next_sequence=3", [])
        .unwrap();
    transaction::snapshot(db, witness).expect("valid historical fixture");
}

#[cfg(unix)]
#[test]
fn v1_upgrade_preserves_active_settled_history_trial_and_full_width_values() {
    let root = tempfile::tempdir().unwrap();
    let store = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    populated_v1(&mut db, &store.witness);
    let tables = [
        "journal_meta",
        "owners",
        "obligations",
        "family_roles",
        "members",
        "operations",
    ];
    let before: Vec<_> = tables
        .iter()
        .map(|table| logical_rows(&db, table))
        .collect();
    upgrade(&mut db, &store.witness).unwrap();
    validate(&db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    for (table, old) in tables.iter().zip(before) {
        assert_eq!(logical_rows(&db, table), old, "{table}");
    }
    let snapshot = transaction::snapshot(&db, &store.witness).unwrap();
    assert_eq!(snapshot.total, vector(u64::MAX));
    assert_eq!(snapshot.obligations.len(), 1);
    assert_eq!(snapshot.obligations[0].allocation_epoch, u64::MAX);
    assert_eq!(snapshot.trial_groups.len(), 1);
    assert!(snapshot.retained_assets.is_empty());
    // Exact-v2 reopen is a no-op; old shape recognition cannot accept the new DB.
    let rows = logical_rows(&db, "operations");
    upgrade(&mut db, &store.witness).unwrap();
    assert_eq!(rows, logical_rows(&db, "operations"));
    let old = Connection::open_in_memory().unwrap();
    old.execute_batch(DDL_V1).unwrap();
    assert_ne!(shape(&old).unwrap(), shape(&db).unwrap());
}
#[cfg(unix)]
#[test]
fn malformed_v1_semantic_history_is_refused_before_schema_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    populated_v1(&mut db, &store.witness);
    db.execute("DELETE FROM operations WHERE operation_id=(SELECT last_operation_id FROM obligations WHERE phase='prepared')", []).unwrap();
    let old_shape = shape(&db).unwrap();
    let old_rows = logical_rows(&db, "obligations");
    assert!(upgrade(&mut db, &store.witness).is_err());
    assert_eq!(shape(&db).unwrap(), old_shape);
    assert_eq!(logical_rows(&db, "obligations"), old_rows);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[cfg(unix)]
#[test]
fn v1_upgrade_local_staging_failure_preserves_original_tables_and_version() {
    let root = tempfile::tempdir().unwrap();
    let store = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    populated_v1(&mut db, &store.witness);
    // A same-connection TEMP name collision is a definite pre-COMMIT failure,
    // never permission to stamp v2 or discard old authority.
    db.execute_batch("CREATE TEMP TABLE custody_upgrade_members(value INTEGER);")
        .unwrap();
    let old = logical_rows(&db, "members");
    assert!(upgrade(&mut db, &store.witness).is_err());
    validate(&db).unwrap();
    assert_eq!(logical_rows(&db, "members"), old);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[cfg(unix)]
#[test]
fn ready_v1_open_upgrades_same_inode_without_replacing_witness() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let identity = owner();
    let store = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    let witness = store.witness.clone();
    drop(store);
    let path = root.path().join("resource-custody/obligations.sqlite3");
    let witness_path = root.path().join("resource-custody/installation.json");
    let witness_bytes = std::fs::read(&witness_path).unwrap();
    let inode = std::fs::metadata(&path).unwrap().ino();
    // Build a historical installation fixture, NOT a production downmigration.
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE retained_asset_owners; DROP TABLE members; DROP TABLE family_roles; DROP TABLE operations; DROP TABLE obligations; DROP TABLE journal_meta; DROP TABLE owners;").unwrap();
    populated_v1(&mut db, &witness);
    drop(db);
    let reopened = storage::Storage::open(&options(
        root.path(),
        identity,
        InstallationContext::Existing,
    ))
    .unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
    assert_eq!(std::fs::read(witness_path).unwrap(), witness_bytes);
    assert_eq!(
        transaction::snapshot(&reopened.connection, &reopened.witness)
            .unwrap()
            .total,
        vector(u64::MAX)
    );
}
#[test]
fn v2_schema_is_exact_and_rejects_extra_shape() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(DDL).unwrap();
    validate(&db).unwrap();
    db.execute_batch("CREATE TABLE extra_authority(value TEXT) STRICT;")
        .unwrap();
    assert!(validate(&db).is_err());
}

#[cfg(unix)]
#[test]
fn v1_upgrade_disk_full_rolls_back_persistent_ddl_and_history() {
    let root = tempfile::tempdir().unwrap();
    let store = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    populated_v1(&mut db, &store.witness);
    let pages: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    // v2's extra table/unique-index roots need pages; existing main pages may
    // be reused during rebuild, but persistent growth is refused by SQLite.
    db.execute_batch(&format!("PRAGMA max_page_count={pages};"))
        .unwrap();
    let old_shape = shape(&db).unwrap();
    let old_rows = logical_rows(&db, "members");
    assert!(upgrade(&mut db, &store.witness).is_err());
    validate(&db).unwrap();
    assert_eq!(shape(&db).unwrap(), old_shape);
    assert_eq!(logical_rows(&db, "members"), old_rows);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
