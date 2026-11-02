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
    let projection = match table {
        "journal_meta" => "singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id",
        "obligations" => "obligation_id,owner_id,graph_id,allocation_epoch,resources,trial_group_id,phase,sequence,last_operation_id,manifest_json,hold_reason,settlement_json",
        _ => "*",
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
        7
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
    // Exact-v6 reopen is a no-op; old shape recognition cannot accept the new DB.
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
    db.execute_batch("DROP TABLE members; DROP TABLE family_roles; DROP TABLE retained_asset_owners; DROP TABLE recovered_s_holders; DROP TABLE recovered_r_holders; DROP TABLE asset_operation_membership; DROP TABLE operations; DROP TABLE obligations; DROP TABLE journal_meta; DROP TABLE owners;").unwrap();
    populated_v1(&mut db, &witness);
    validate(&db).expect("exact historical v1 fixture before storage reopen");
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
fn v3_schema_is_exact_and_rejects_extra_shape() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(DDL_V3).unwrap();
    validate(&db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    db.execute_batch("CREATE TABLE extra_authority(value TEXT) STRICT;")
        .unwrap();
    assert!(validate(&db).is_err());
}

#[test]
fn v4_schema_is_exact_and_rejects_extra_shape() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(DDL_V4).unwrap();
    validate(&db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    db.execute_batch("CREATE TABLE extra_authority(value TEXT) STRICT;")
        .unwrap();
    assert!(validate(&db).is_err());
}

#[test]
fn v5_schema_is_exact_and_rejects_extra_shape() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(DDL_V5).unwrap();
    validate(&db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
    db.execute_batch("CREATE TABLE extra_authority(value TEXT) STRICT;")
        .unwrap();
    assert!(validate(&db).is_err());
}

#[test]
fn v6_schema_requires_covering_family_lookup_index() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(DDL_V6).unwrap();
    validate(&db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
    let mut plan = db
        .prepare(
            "EXPLAIN QUERY PLAN SELECT EXISTS(SELECT 1 FROM family_roles WHERE obligation_id=?1 AND role<>?2)",
        )
        .unwrap();
    let details: Vec<String> = plan
        .query_map(params![Uuid::new_v4().as_bytes(), "video"], |row| {
            row.get(3)
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(details
        .iter()
        .any(|detail| { detail.contains("USING COVERING INDEX family_roles_by_obligation_role") }));
    let mut asset_plan = db
        .prepare(
            "EXPLAIN QUERY PLAN SELECT operations.result_json FROM asset_operation_membership AS membership JOIN operations ON operations.operation_id=membership.operation_id WHERE membership.asset_id=?1 ORDER BY membership.sequence",
        )
        .unwrap();
    let asset_details: Vec<String> = asset_plan
        .query_map([Uuid::new_v4().as_bytes()], |row| row.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(asset_details
        .iter()
        .any(|detail| { detail.contains("asset_operation_by_asset_sequence") }));
    db.execute_batch("DROP INDEX family_roles_by_obligation_role;")
        .unwrap();
    assert!(validate(&db).is_err());
}

#[cfg(unix)]
#[test]
fn v5_to_v6_index_upgrade_preserves_all_historical_table_rows() {
    let root = tempfile::tempdir().unwrap();
    let store = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    populated_v1(&mut db, &store.witness);
    migration::upgrade_v1_to_v2(&mut db, &store.witness).unwrap();
    migration::upgrade_v2_to_v3(&mut db, &store.witness).unwrap();
    migration::upgrade_v3_to_v4(&mut db, &store.witness).unwrap();
    migration::upgrade_v4_to_v5(&mut db, &store.witness).unwrap();
    validate(&db).unwrap();
    let tables = [
        "journal_meta",
        "owners",
        "obligations",
        "family_roles",
        "members",
        "operations",
        "retained_asset_owners",
        "recovered_s_holders",
        "recovered_r_holders",
    ];
    let before: Vec<_> = tables
        .iter()
        .map(|table| logical_rows(&db, table))
        .collect();
    migration::upgrade_v5_to_v6(&mut db, &store.witness).unwrap();
    validate(&db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
    for (table, old) in tables.iter().zip(before) {
        assert_eq!(logical_rows(&db, table), old, "{table}");
    }
    assert_eq!(
        transaction::snapshot(&db, &store.witness).unwrap().total,
        vector(u64::MAX)
    );
}

#[cfg(unix)]
#[test]
fn v5_to_v6_refuses_corrupted_oldest_asset_event_and_rolls_back_projection() {
    use crate::resource_custody::transaction::frozen_source_fixture_v6;

    let mut source = frozen_source_fixture_v6();
    let db = &mut source.storage.connection;
    let canonical = Connection::open_in_memory().unwrap();
    canonical.execute_batch(DDL_V5).unwrap();
    let old_meta_sql: String = canonical
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='journal_meta'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    db.execute_batch(
        "DROP TABLE asset_operation_membership;
         DROP INDEX family_roles_by_obligation_role;
         CREATE TEMP TABLE fixture_v6_meta AS SELECT * FROM journal_meta;
         DROP TABLE journal_meta;",
    )
    .unwrap();
    db.execute_batch(&old_meta_sql).unwrap();
    db.execute_batch(
        "INSERT INTO journal_meta SELECT singleton,5,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.fixture_v6_meta;
         DROP TABLE temp.fixture_v6_meta;
         PRAGMA user_version=5;",
    )
    .unwrap();
    validate(db).unwrap();
    let (operation, altered) = {
        let mut statement = db
            .prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")
            .unwrap();
        let mut rows = statement.query([]).unwrap();
        let mut found = None;
        while let Some(row) = rows.next().unwrap() {
            let operation: Vec<u8> = row.get(0).unwrap();
            let json: String = row.get(1).unwrap();
            let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
            if value["kind"] == "asset_changed_v3" {
                value["change"]["asset_id"] = serde_json::json!(Uuid::new_v4());
                found = Some((operation, serde_json::to_string(&value).unwrap()));
                break;
            }
        }
        found.expect("fixture has an asset event")
    };
    db.execute(
        "UPDATE operations SET result_json=?1 WHERE operation_id=?2",
        params![altered, operation],
    )
    .unwrap();
    let original_rows = logical_rows(db, "operations");
    assert!(migration::upgrade_v5_to_v6(db, &source.storage.witness).is_err());
    validate(db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(logical_rows(db, "operations"), original_rows);
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

#[cfg(unix)]
#[test]
fn recognized_v2_history_upgrades_to_v3_with_conservative_claim_and_unchanged_bytes() {
    let root = tempfile::Builder::new()
        .prefix("plurx-custody-migration-")
        .tempdir()
        .unwrap();
    let store = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    populated_v1(&mut db, &store.witness);
    migration::upgrade_v1_to_v2(&mut db, &store.witness).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    let before_obligations = logical_rows(&db, "obligations");
    let before_operations = logical_rows(&db, "operations");
    upgrade(&mut db, &store.witness).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(logical_rows(&db, "obligations"), before_obligations);
    assert_eq!(logical_rows(&db, "operations"), before_operations);
    let claim: String = db
        .query_row(
            "SELECT memory_claim FROM obligations WHERE phase='prepared'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(claim, "bounded_active");
    let recovered = transaction::snapshot(&db, &store.witness).unwrap();
    assert_eq!(
        recovered.obligations[0].memory_claim,
        crate::adaptive_resources::MemoryClaim::default()
    );
}

fn populated_v6(db: &mut Connection, witness: &storage::Witness) {
    populated_v1(db, witness);
    migration::upgrade_v1_to_v2(db, witness).unwrap();
    migration::upgrade_v2_to_v3(db, witness).unwrap();
    migration::upgrade_v3_to_v4(db, witness).unwrap();
    migration::upgrade_v4_to_v5(db, witness).unwrap();
    migration::upgrade_v5_to_v6(db, witness).unwrap();
    validate(db).unwrap();
}

fn v7_complete_rows(db: &Connection, table: &str) -> Vec<Vec<Value>> {
    if table == "journal_meta" {
        return logical_rows(db, table);
    }
    let mut stmt = db
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
        .unwrap();
    let count = stmt.column_count();
    stmt.query_map([], |row| (0..count).map(|i| row.get(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn recognized_v6_storage_reopen_upgrades_to_v7_without_losing_original_debt_or_history() {
    let root = tempfile::tempdir().unwrap();
    let store = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    let witness = store.witness.clone();
    drop(store);
    // Same guarded authority file/inode, deliberately populated as a real
    // historical version before its next actual Storage::open.
    let path = root.path().join("resource-custody/obligations.sqlite3");
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON; DROP TABLE members; DROP TABLE family_roles; DROP TABLE retained_asset_owners; DROP TABLE recovered_s_holders; DROP TABLE recovered_r_holders; DROP TABLE asset_operation_membership; DROP TABLE operations; DROP TABLE obligations; DROP TABLE journal_meta; DROP TABLE owners;").unwrap();
    populated_v6(&mut db, &witness);
    let tables = [
        "journal_meta",
        "owners",
        "obligations",
        "family_roles",
        "members",
        "operations",
        "retained_asset_owners",
        "recovered_s_holders",
        "recovered_r_holders",
        "asset_operation_membership",
    ];
    let before = tables
        .iter()
        .map(|table| v7_complete_rows(&db, table))
        .collect::<Vec<_>>();
    let operation_rowids = db
        .prepare("SELECT rowid FROM operations ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(db);
    let reopened = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::Existing,
    ))
    .unwrap();
    assert_eq!(
        reopened
            .connection
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    for (table, expected) in tables.iter().zip(before) {
        assert_eq!(
            v7_complete_rows(&reopened.connection, table),
            expected,
            "{table}"
        );
    }
    let actual_rowids = reopened
        .connection
        .prepare("SELECT rowid FROM operations ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(actual_rowids, operation_rowids);
    let snapshot = transaction::snapshot(&reopened.connection, &reopened.witness).unwrap();
    assert_eq!(snapshot.total, vector(u64::MAX));
    assert_eq!(snapshot.obligations.len(), 1);
    assert!(!snapshot.trial_groups.is_empty());
}

#[cfg(unix)]
#[test]
fn v6_to_v7_unknown_shape_and_corrupt_history_are_refused_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    for extra_shape in [true, false] {
        let mut db = Connection::open_in_memory().unwrap();
        populated_v6(&mut db, &store.witness);
        if extra_shape {
            db.execute_batch(
                "CREATE TRIGGER fixture_unknown AFTER UPDATE ON members BEGIN SELECT 1; END;",
            )
            .unwrap();
        } else {
            db.execute("UPDATE operations SET result_json='{}' WHERE rowid=(SELECT MIN(rowid) FROM operations)", []).unwrap();
        }
        let shape_before = shape(&db).unwrap();
        let old_rows = v7_complete_rows(&db, "operations");
        assert!(migration::upgrade_v6_to_v7(&mut db, &store.witness).is_err());
        assert_eq!(shape(&db).unwrap(), shape_before);
        assert_eq!(v7_complete_rows(&db, "operations"), old_rows);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
    }
}

#[test]
fn v7_schema_is_exact_and_rejects_extra_shape() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(DDL).unwrap();
    validate(&db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    db.execute_batch("CREATE TABLE unexpected_authority(x TEXT) STRICT;")
        .unwrap();
    assert!(validate(&db).is_err());
}

#[cfg(unix)]
#[test]
fn historical_v1_and_v2_snapshots_read_only_their_declared_ownership_domains() {
    let root = tempfile::tempdir().unwrap();
    let storage = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    for version in [1, 2] {
        let mut db = Connection::open_in_memory().unwrap();
        populated_v1(&mut db, &storage.witness);
        if version == 2 {
            migration::upgrade_v1_to_v2(&mut db, &storage.witness).unwrap();
        }
        validate(&db).unwrap();
        let has_asset_table: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='retained_asset_owners')",
            [], |row| row.get(0)).unwrap();
        assert_eq!(has_asset_table, version >= 2);
        let before = logical_rows(&db, "operations");
        let snapshot = transaction::snapshot(&db, &storage.witness).unwrap();
        assert_eq!(snapshot.total, vector(u64::MAX));
        assert_eq!(snapshot.obligations.len(), 1);
        assert_eq!(snapshot.families.len(), 1);
        assert_eq!(snapshot.trial_groups.len(), 1);
        assert!(snapshot.retained_assets.is_empty());
        assert_eq!(logical_rows(&db, "operations"), before);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            version
        );
    }
}

#[cfg(unix)]
#[test]
fn historical_v1_and_v2_snapshots_refuse_active_debt_without_its_original_family() {
    let root = tempfile::tempdir().unwrap();
    let storage = storage::Storage::open(&options(
        root.path(),
        owner(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    for version in [1, 2] {
        let mut db = Connection::open_in_memory().unwrap();
        populated_v1(&mut db, &storage.witness);
        if version == 2 {
            migration::upgrade_v1_to_v2(&mut db, &storage.witness).unwrap();
        }
        let before = logical_rows(&db, "obligations");
        db.execute("DELETE FROM family_roles WHERE obligation_id IN(SELECT obligation_id FROM obligations WHERE phase!='settled')", []).unwrap();
        let error = transaction::snapshot(&db, &storage.witness).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnreadableAuthority,
            "missing family is invalid authority, never an optional-table SQL failure or empty debt");
        assert_eq!(logical_rows(&db, "obligations"), before);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            version
        );
    }
}
