//! Atomic retained-asset retirement regressions. Fixture observations are
//! journal test data, not evidence of physical native closure.
use super::*;
use crate::resource_custody::codec_v2 as v2;
use crate::resource_custody::tests::{deadline, new_obligation};
use crate::resource_custody::transaction::typed::tests::bootstrap::frozen_source_fixture;

struct RetirementFixture {
    source: super::super::typed::tests::bootstrap::FrozenSourceFixture,
    command: QueuedOperation,
    retained: v2::RetainedAssetsV2,
    target_resources: ResourceVector,
}

fn make_fixture(corrupt_snapshot: bool) -> RetirementFixture {
    make_fixture_with_source(corrupt_snapshot, frozen_source_fixture())
}

fn make_fixture_with_source(
    corrupt_snapshot: bool,
    source: super::super::typed::tests::bootstrap::FrozenSourceFixture,
) -> RetirementFixture {
    let target_obligation_id = Uuid::new_v4();
    let target_graph_id = Uuid::new_v4();
    let target_member_id = v2::Id::new(Uuid::new_v4()).unwrap();
    let target_operation_id = Uuid::new_v4();

    let mut target_manifest = source.manifest.clone();
    target_manifest.graph_id = v2::Id::new(target_graph_id).unwrap();
    let v2::MemberPlan::ScratchAssetsV3 { slot, plan } = &mut target_manifest.members[0] else {
        unreachable!()
    };
    slot.member_id = target_member_id;
    let snapshot = v2::scratch::bootstrap::frozen_snapshot_ref(&source.plan, &source.full)
        .expect("source snapshot reference");
    let mut transfer = v2::MemberTransfer {
        source_member_id: v2::Id::new(source.member.uuid()).unwrap(),
        target_member_id,
        identity: v2::TransferIdentity::ScratchAssetsV3 {
            plan: source.plan.clone(),
            snapshot,
        },
    };
    if corrupt_snapshot {
        let v2::TransferIdentity::ScratchAssetsV3 { snapshot, .. } = &mut transfer.identity else {
            unreachable!()
        };
        let replacement = if snapshot.inventory_sha256.starts_with('0') {
            "1"
        } else {
            "0"
        };
        snapshot.inventory_sha256.replace_range(..1, replacement);
    }

    let (new_target, _target_reservation) = new_obligation(
        source.owner,
        ResourceVector {
            handles: u64::from(plan.limits.live_handles),
            peak_memory_bytes: plan.limits.resident_memory_bytes,
            scratch_bytes: plan.limits.reserved_storage_bytes,
            ..ResourceVector::default()
        },
    );
    let target = ObligationHandle {
        journal_id: source.handle.journal_id,
        owner_id: source.owner,
        obligation_id: target_obligation_id,
        graph_id: target_graph_id,
        runtime_binding: new_target.runtime_binding,
        #[cfg(target_os = "linux")]
        recovery: None,
    };
    target_manifest.owner_id = v2::Id::new(source.owner).unwrap();
    let retained = v2::RetainedAssetsV2 {
        version: 2,
        asset_id: source.plan.asset_id,
        rendition_id: source.plan.rendition_id.clone(),
        generation_epoch: source.plan.generation_epoch,
        producer_obligation_id: v2::Id::new(source.handle.obligation_id()).unwrap(),
        producer_graph_id: v2::Id::new(source.handle.graph_id()).unwrap(),
        transfer_operation_id: v2::Id::new(target_operation_id).unwrap(),
        target_obligation_id: v2::Id::new(target_obligation_id).unwrap(),
        target_graph_id: v2::Id::new(target_graph_id).unwrap(),
        target_owner_id: v2::Id::new(source.owner).unwrap(),
        transfer: transfer.clone(),
        reader_ids: Vec::new(),
        reconciliation: v2::Reconciliation::LiveOwned,
    };
    let settlement = v2::Settlement {
        version: 2,
        owner_id: v2::Id::new(source.owner).unwrap(),
        graph_id: v2::Id::new(source.handle.graph_id()).unwrap(),
        operation_id: v2::Id::new(target_operation_id).unwrap(),
        members: vec![v2::MemberDisposition::Transferred {
            transfer,
            target_obligation_id: v2::Id::new(target_obligation_id).unwrap(),
            target_graph_id: v2::Id::new(target_graph_id).unwrap(),
            asset_id: source.plan.asset_id,
            operation_id: v2::Id::new(target_operation_id).unwrap(),
        }],
    };
    // Derive expected fixture credit from the original uncorrupted inventory;
    // the corrupt-reference case must reach the transaction, not panic while
    // constructing its deliberately invalid request.
    let totals = &source.full.state.asset.totals;
    let target_resources = ResourceVector {
        handles: u64::from(totals.handles_reserved),
        peak_memory_bytes: totals.memory_reserved_bytes,
        scratch_bytes: totals.charged_storage_bytes,
        ..ResourceVector::default()
    };
    let receipt = PhysicalRetirementReceipt::adapter_fixture(
        source.handle.runtime_binding,
        target.runtime_binding,
    );
    let proof = PhysicalAssetRetirement {
        receipt,
        settlement,
        retained: retained.clone(),
        target_manifest,
        target_resources,
        target,
    };
    let expected_sequence: i64 = source
        .storage
        .connection
        .query_row(
            "SELECT sequence FROM obligations WHERE obligation_id=?1",
            [source.handle.obligation_id().as_bytes()],
            |row| row.get(0),
        )
        .unwrap();
    let command = QueuedOperation {
        id: target_operation_id,
        deadline: deadline(),
        command: Command::Change {
            key: TransitionKey {
                handle: source.handle.clone(),
                operation_id: target_operation_id,
                expected_sequence,
            },
            mutation: Mutation::RetireWithAssets(Box::new(proof)),
        },
    };
    RetirementFixture {
        source,
        command,
        retained,
        target_resources,
    }
}

#[test]
fn retained_retirement_is_atomic_and_exact_retry_recovers_lost_ack() {
    let mut f = make_fixture(false);
    let Mutation::RetireWithAssets(proof) = (match &f.command.command {
        Command::Change { mutation, .. } => mutation,
        _ => unreachable!(),
    }) else {
        unreachable!()
    };
    let source_binding = proof.receipt.source_binding();
    let target_binding = proof.receipt.target_binding();
    f.source.storage.fail_after_commit_once = true;
    assert_eq!(
        execute(&mut f.source.storage, &f.command, f.source.owner)
            .unwrap_err()
            .certainty,
        CommitCertainty::Unknown
    );
    let retry = execute(&mut f.source.storage, &f.command, f.source.owner)
        .expect("exact replay returns committed outcome");
    let OperationResult::RetiredWithAssets { ack, target } = retry else {
        panic!("retirement acknowledgment")
    };
    assert_eq!(ack.operation_id.uuid(), f.command.id);
    assert_eq!(
        target.obligation_id(),
        f.retained.target_obligation_id.uuid()
    );
    let Mutation::RetireWithAssets(proof) = (match &f.command.command {
        Command::Change { mutation, .. } => mutation,
        _ => unreachable!(),
    }) else {
        unreachable!()
    };
    assert_eq!(proof.receipt.source_binding(), source_binding);
    assert_eq!(proof.receipt.target_binding(), target_binding);

    let snapshot = snapshot(&f.source.storage.connection, &f.source.storage.witness)
        .expect("restart-style full journal reconstruction");
    assert_eq!(snapshot.total, f.target_resources);
    assert_eq!(snapshot.obligations.len(), 1);
    assert_eq!(
        snapshot.obligations[0].memory_claim,
        crate::adaptive_resources::MemoryClaim::KnownBounded {
            active_producer: false
        }
    );
    assert_eq!(
        snapshot.obligations[0].obligation_id,
        target.obligation_id()
    );
    let source_phase: String = f
        .source
        .storage
        .connection
        .query_row(
            "SELECT phase FROM obligations WHERE obligation_id=?1",
            [f.source.handle.obligation_id().as_bytes()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_phase, "settled");
    assert_eq!(snapshot.retained_assets.len(), 1);
    // This real transferred V2 resident carries no producer peak. A later
    // cold unknown producer can be admitted beside its still-held S charge.
    let tx = f.source.storage.connection.transaction().unwrap();
    super::super::check_memory_claim_combination(
        &tx,
        [crate::adaptive_resources::MemoryClaim::UnknownProducerPeak],
    )
    .unwrap();
    drop(tx);
    let operation_count: i64 = f
        .source
        .storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE operation_id=?1",
            [f.command.id.as_bytes()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(operation_count, 1);
}

#[test]
fn corrupt_frozen_snapshot_rolls_back_source_target_and_retains_receipt() {
    let mut f = make_fixture(true);
    let Mutation::RetireWithAssets(proof) = (match &f.command.command {
        Command::Change { mutation, .. } => mutation,
        _ => unreachable!(),
    }) else {
        unreachable!()
    };
    let source_binding = proof.receipt.source_binding();
    assert!(execute(&mut f.source.storage, &f.command, f.source.owner).is_err());
    let Mutation::RetireWithAssets(proof) = (match &f.command.command {
        Command::Change { mutation, .. } => mutation,
        _ => unreachable!(),
    }) else {
        unreachable!()
    };
    assert_eq!(proof.receipt.source_binding(), source_binding);
    let source_phase: String = f
        .source
        .storage
        .connection
        .query_row(
            "SELECT phase FROM obligations WHERE obligation_id=?1",
            [f.source.handle.obligation_id().as_bytes()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_phase, "sealed");
    let targets: i64 = f
        .source
        .storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM obligations WHERE graph_id=?1",
            [f.retained.target_graph_id.uuid().as_bytes()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(targets, 0);
    let retained: i64 = f
        .source
        .storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM retained_asset_owners WHERE asset_id=?1",
            [f.retained.asset_id.uuid().as_bytes()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, 0);
}

#[test]
fn settled_v3_retirement_inventory_requires_exact_retained_transfer() {
    let mut f = make_fixture(false);
    execute(&mut f.source.storage, &f.command, f.source.owner).unwrap();
    let db = &f.source.storage.connection;
    let settlement: String = db
        .query_row(
            "SELECT settlement_json FROM obligations WHERE obligation_id=?1",
            [f.source.handle.obligation_id().as_bytes()],
            |r| r.get(0),
        )
        .unwrap();
    let manifest = codec::encode(&f.source.manifest).unwrap();
    let load_original = || {
        super::super::inventory::load(
            db,
            f.source.handle.obligation_id(),
            f.source.owner,
            f.source.handle.graph_id(),
            Phase::Settled,
            &manifest,
            Some(&settlement),
        )
    };
    assert!(matches!(
        load_original().expect("V2 retained document binds the original settled producer"),
        ObligationInventory::TypedV2 { .. }
    ));
    let mut changed = f.retained.clone();
    changed.transfer.target_member_id = v2::Id::new(Uuid::new_v4()).unwrap();
    db.execute(
        "UPDATE retained_asset_owners SET custody_json=?2 WHERE obligation_id=?1",
        params![
            changed.target_obligation_id.uuid().as_bytes(),
            codec::encode(&changed).unwrap()
        ],
    )
    .unwrap();
    assert!(
        load_original().is_err(),
        "a different retained transfer is not the original settlement"
    );
}

/// These are complete journal AUDIT fixtures, not a NativeRetainedSImportProof
/// or positive process-exit witness. The tested authority is schema migration.
fn retained_v6_with_adoption_history() -> RetirementFixture {
    let source = super::super::typed::tests::bootstrap::frozen_source_fixture_v6();
    let mut f = make_fixture_with_source(false, source);
    let OperationResult::RetiredWithAssets { target, .. } =
        execute(&mut f.source.storage, &f.command, f.source.owner).unwrap()
    else {
        panic!("original fixture retirement");
    };
    let before_adoption = snapshot(&f.source.storage.connection, &f.source.storage.witness)
        .expect("real V3 retirement must reconstruct before adding adoption audit history");
    assert_eq!(before_adoption.retained_assets.len(), 1);
    assert!(before_adoption.recovered_s_holders.is_empty());
    let db = &mut f.source.storage.connection;
    let mut next_owner =
        super::super::load_owner(db, f.source.owner, &f.source.storage.witness).unwrap();
    next_owner.owner_id = Uuid::new_v4();
    super::super::register_owner(db, &next_owner).unwrap();
    let original_sequence: i64 = db
        .query_row(
            "SELECT sequence FROM obligations WHERE obligation_id=?1",
            [target.obligation_id().as_bytes()],
            |r| r.get(0),
        )
        .unwrap();
    let head = f
        .source
        .full
        .state
        .asset
        .head
        .clone()
        .expect("actual original frozen audit head");
    let frozen =
        v2::scratch::bootstrap::frozen_snapshot_ref(&f.source.plan, &f.source.full).unwrap();
    let operation = Uuid::new_v4();
    let tx = db.transaction().unwrap();
    let sequence = super::super::next_sequence(&tx).unwrap();
    let ack = RecoveryAdoptionAck {
        operation_id: operation,
        obligation_id: target.obligation_id(),
        previous_owner_id: f.source.owner,
        current_owner_id: next_owner.owner_id,
        sequence,
    };
    // Append internally consistent historical data only. No native receipt is
    // constructed, and no production adoption/close/refund API consumes this.
    tx.execute(
        "INSERT INTO operations VALUES(?1,?2,?3)",
        params![
            operation.as_bytes(),
            codec::encode(&"0".repeat(64)).unwrap(),
            codec::encode(&DiskResult::RecoveredSAdoption(ack)).unwrap()
        ],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO recovered_s_holders VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            target.obligation_id().as_bytes(),
            sequence,
            operation.as_bytes(),
            f.source.owner.as_bytes(),
            next_owner.owner_id.as_bytes(),
            original_sequence,
            head.operation_id.uuid().as_bytes(),
            i64::try_from(head.revision).unwrap(),
            frozen.inventory_sha256
        ],
    )
    .unwrap();
    tx.commit().unwrap();
    let restored = snapshot(db, &f.source.storage.witness).unwrap();
    assert_eq!(restored.retained_assets.len(), 1);
    assert_eq!(restored.recovered_s_holders.len(), 1);
    assert!(
        db.query_row("SELECT count(*) FROM asset_operation_membership", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
            > 1
    );
    f
}

fn complete_history_rows(db: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let projection = if table == "journal_meta" {
        "singleton,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id"
    } else {
        "*"
    };
    let mut statement = db
        .prepare(&format!("SELECT {projection} FROM {table} ORDER BY 1,2"))
        .unwrap();
    let count = statement.column_count();
    statement
        .query_map([], |row| (0..count).map(|i| row.get(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn v6_to_v7_preserves_typed_retained_adoption_and_asset_projection_history() {
    let mut f = retained_v6_with_adoption_history();
    assert_v7_typed_migration(&mut f, false);
}

#[test]
fn v7_sql_error_after_persistent_rebuild_rolls_back_typed_history_and_schema() {
    let mut f = retained_v6_with_adoption_history();
    assert_v7_typed_migration(&mut f, true);
}

fn assert_v7_typed_migration(f: &mut RetirementFixture, after_ddl_error: bool) {
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
    let db = &mut f.source.storage.connection;
    let before = tables
        .iter()
        .map(|table| complete_history_rows(db, table))
        .collect::<Vec<_>>();
    let rowids = db
        .prepare("SELECT rowid FROM operations ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let catalog = db
        .prepare("SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type,name")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let failure =
        after_ddl_error.then(crate::resource_custody::schema::fail_next_v7_after_ddl_for_test);
    let result = crate::resource_custody::schema::upgrade(db, &f.source.storage.witness);
    if after_ddl_error {
        assert_eq!(result.unwrap_err().kind, ErrorKind::Storage);
        assert!(
            failure.as_ref().unwrap().was_reached(),
            "actual post-DDL failure hook was reached"
        );
        let after_catalog = db
            .prepare(
                "SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type,name",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(after_catalog, catalog);
    } else {
        result.unwrap();
    }
    drop(failure);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        if after_ddl_error { 6 } else { 7 }
    );
    crate::resource_custody::schema::validate(db).unwrap();
    for (table, expected) in tables.iter().zip(before) {
        assert_eq!(complete_history_rows(db, table), expected, "{table}");
    }
    let after_rowids = db
        .prepare("SELECT rowid FROM operations ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(after_rowids, rowids);
    let restored = snapshot(db, &f.source.storage.witness).unwrap();
    assert_eq!(restored.retained_assets.len(), 1);
    assert_eq!(restored.recovered_s_holders.len(), 1);
    assert_eq!(restored.total, f.target_resources);
    assert_eq!(
        db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
