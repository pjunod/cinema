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
    let mut source = frozen_source_fixture();
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
