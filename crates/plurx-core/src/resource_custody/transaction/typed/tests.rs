//! Real SQLite transitions with typed fixture data; no physical receipts minted.
pub(in crate::resource_custody) mod bootstrap;
mod planning;
mod scratch;
use super::*;
use crate::adaptive_resources::ResourceReservation;
use crate::resource_custody::tests::{deadline, new_obligation, options, owner, vector};

struct Fixture {
    _root: tempfile::TempDir,
    storage: storage::Storage,
    owner: Uuid,
    command: QueuedOperation,
    _reservation: ResourceReservation,
}
fn fixture(trial: bool) -> Fixture {
    let root = tempfile::Builder::new()
        .prefix("plurx-typed-fixture-")
        .tempdir()
        .unwrap();
    let manifest = v2::tests::fixture();
    let mut identity = owner();
    identity.owner_id = manifest.owner_id.uuid();
    let mut storage = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    register_owner(&mut storage.connection, &identity).unwrap();
    let (legacy, reservation) = new_obligation(identity.owner_id, vector(10));
    let handle = ObligationHandle {
        journal_id: storage.witness.journal_id,
        owner_id: identity.owner_id,
        obligation_id: Uuid::new_v4(),
        graph_id: manifest.graph_id.uuid(),
        runtime_binding: legacy.runtime_binding,
        #[cfg(target_os = "linux")]
        recovery: None,
    };
    let batch = TypedPrepareBatch {
        obligations: vec![TypedNewObligation {
            runtime_binding: legacy.runtime_binding,
            allocation_epoch: u64::MAX,
            resources: vector(10),
            memory_claim: crate::adaptive_resources::MemoryClaim::default(),
            manifest,
        }],
        families: vec![FamilyRequest {
            family: FamilyHandle {
                journal_id: storage.witness.journal_id,
                family_id: Uuid::new_v4(),
            },
            roles: vec![(Role::Video, RoleTarget::New(0))],
        }],
        trial,
    };
    Fixture {
        _root: root,
        storage,
        owner: identity.owner_id,
        command: QueuedOperation {
            id: Uuid::new_v4(),
            deadline: deadline(),
            command: Command::PrepareTyped {
                batch,
                handles: vec![handle],
                trial: trial.then(Uuid::new_v4),
            },
        },
        _reservation: reservation,
    }
}
fn prepare(f: &mut Fixture) -> PreparedBatch {
    match execute(&mut f.storage, &f.command, f.owner).unwrap() {
        OperationResult::Prepared(p) => p,
        _ => panic!("prepare result"),
    }
}

#[test]
fn typed_prepare_accepts_current_exact_v6_storage_schema() {
    let mut f = fixture(false);
    install_empty_v6_fixture(&mut f.storage);
    let version: i64 = f
        .storage
        .connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 6);

    let prepared = prepare(&mut f);
    assert_eq!(prepared.obligations.len(), 1);
}

#[test]
fn typed_prepare_accepts_current_exact_v7_storage_schema() {
    let mut f = fixture(false);
    assert_eq!(
        f.storage
            .connection
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(prepare(&mut f).obligations.len(), 1);
}

/// Test-only construction of an EMPTY historical database before any command
/// is submitted. Existing owner/witness bytes are copied, never regenerated.
/// This is not a production downgrade, migration, native proof or refund.
pub(in crate::resource_custody) fn install_empty_v6_fixture(storage: &mut storage::Storage) {
    let db = &mut storage.connection;
    assert_eq!(
        db.query_row("SELECT count(*) FROM obligations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM operations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let tx = db.transaction().unwrap();
    tx.execute_batch("CREATE TEMP TABLE fixture_empty_v6_owner AS SELECT * FROM owners; CREATE TEMP TABLE fixture_empty_v6_meta AS SELECT * FROM journal_meta;").unwrap();
    tx.execute_batch("DROP TABLE members; DROP TABLE family_roles; DROP TABLE retained_asset_owners; DROP TABLE recovered_s_holders; DROP TABLE recovered_r_holders; DROP TABLE asset_operation_membership; DROP TABLE operations; DROP TABLE obligations; DROP TABLE journal_meta; DROP TABLE owners;").unwrap();
    tx.execute_batch(crate::resource_custody::schema::DDL_V6)
        .unwrap();
    tx.execute_batch("INSERT INTO owners SELECT * FROM temp.fixture_empty_v6_owner; INSERT INTO journal_meta SELECT singleton,6,journal_id,installation_id,node_id,cluster_id,next_sequence,current_owner_id FROM temp.fixture_empty_v6_meta; DROP TABLE temp.fixture_empty_v6_owner; DROP TABLE temp.fixture_empty_v6_meta;").unwrap();
    crate::resource_custody::schema::validate(&tx).unwrap();
    tx.commit().unwrap();
}

fn change_command(handle: &ObligationHandle, sequence: i64, mutation: Mutation) -> QueuedOperation {
    let id = Uuid::new_v4();
    QueuedOperation {
        id,
        deadline: deadline(),
        command: Command::Change {
            key: TransitionKey {
                handle: handle.clone(),
                operation_id: id,
                expected_sequence: sequence,
            },
            mutation,
        },
    }
}
fn sequence(f: &Fixture) -> i64 {
    f.storage
        .connection
        .query_row("SELECT sequence FROM obligations", [], |r| r.get(0))
        .unwrap()
}
fn typed_manifest(f: &Fixture) -> v2::Manifest {
    let Command::PrepareTyped { batch, .. } = &f.command.command else {
        unreachable!()
    };
    batch.obligations[0].manifest.clone()
}
fn observed_source(f: &Fixture) -> v2::Envelope {
    let manifest = typed_manifest(f);
    let planned = manifest.members[1].clone();
    let v2::MemberPlan::SourceV1 { source, .. } = &planned else {
        unreachable!()
    };
    let observed = v2::SourceObserved {
        version: 1,
        holder_id: source.holder_id,
        member_id: source.member_id,
        copies: source.planned_copies.clone(),
        native_object: v2::NativeObject {
            file_type: v2::FileType::Regular,
            device: v2::NativeValue::Unknown {
                reason: "codec fixture, not acquired OS proof".into(),
            },
            inode: v2::NativeValue::Unknown {
                reason: "codec fixture".into(),
            },
            mount_id: v2::NativeValue::Unknown {
                reason: "codec fixture".into(),
            },
            filesystem_domain: source.filesystem_domain.clone(),
        },
        access: source.access.clone(),
        source_association: source.source_association.clone(),
    };
    v2::Envelope {
        version: 2,
        planned,
        observed: Some(v2::Observed::SourceV1 { source: observed }),
    }
}

#[test]
fn typed_prepare_persists_source_task_and_full_trial_charge_in_existing_journal() {
    let mut f = fixture(true);
    let prepared = prepare(&mut f);
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(snapshot.total, vector(10));
    assert_eq!(
        snapshot.trial_groups,
        vec![prepared.trial_group_id.unwrap()]
    );
    assert_eq!(snapshot.families.len(), 1);
    assert!(snapshot.retained_assets.is_empty());
    assert_eq!(snapshot.obligations[0].allocation_epoch, u64::MAX);
    let ObligationInventory::TypedV2 { manifest, members } = &snapshot.obligations[0].inventory
    else {
        panic!("typed inventory")
    };
    assert_eq!(manifest, &typed_manifest(&f));
    assert_eq!(members.len(), 4);
    assert!(members
        .iter()
        .all(|m| m.phase == "planned" && m.envelope.observed.is_none()));
}
#[test]
fn typed_unarmed_cancel_uses_disjoint_audit_and_never_fakes_source_close() {
    let mut f = fixture(true);
    let prepared = prepare(&mut f);
    let command = change_command(
        &prepared.obligations[0],
        prepared.sequence,
        Mutation::CancelPrepared,
    );
    execute(&mut f.storage, &command, f.owner).unwrap();
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(snapshot.total, ResourceVector::default());
    assert!(snapshot.trial_groups.is_empty());
    let json: String = f
        .storage
        .connection
        .query_row("SELECT settlement_json FROM obligations", [], |r| r.get(0))
        .unwrap();
    assert!(matches!(
        v2::decode_settlement(&json).unwrap(),
        v2::SettlementDocument::NeverArmed(_)
    ));
    assert!(codec::decode::<v2::Settlement>(&json).is_err());
}
#[test]
fn typed_armed_cancel_refuses_but_sealed_late_acquisition_keeps_exact_inventory() {
    let mut f = fixture(false);
    let prepared = prepare(&mut f);
    let envelope = observed_source(&f);
    let armed = change_command(
        &prepared.obligations[0],
        sequence(&f),
        Mutation::Arm {
            members: vec![envelope.planned.member_id()],
        },
    );
    execute(&mut f.storage, &armed, f.owner).unwrap();
    let cancel = change_command(
        &prepared.obligations[0],
        sequence(&f),
        Mutation::CancelPrepared,
    );
    assert!(execute(&mut f.storage, &cancel, f.owner).is_err());
    let seal = change_command(&prepared.obligations[0], sequence(&f), Mutation::Seal);
    execute(&mut f.storage, &seal, f.owner).unwrap();
    let acquired = change_command(
        &prepared.obligations[0],
        sequence(&f),
        Mutation::RecordTypedAcquired {
            envelope: envelope.clone(),
        },
    );
    execute(&mut f.storage, &acquired, f.owner).unwrap();
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(snapshot.total, vector(10));
    assert_eq!(snapshot.obligations[0].phase, Phase::Sealed);
    let ObligationInventory::TypedV2 { members, .. } = &snapshot.obligations[0].inventory else {
        unreachable!()
    };
    assert!(members
        .iter()
        .any(|m| m.envelope == envelope && m.phase == "acquired"));
    let duplicate = change_command(
        &prepared.obligations[0],
        sequence(&f),
        Mutation::RecordTypedAcquired { envelope },
    );
    assert!(execute(&mut f.storage, &duplicate, f.owner).is_err());
}
#[test]
fn typed_unknown_prepare_commit_reuses_same_operation_without_duplicate_charge() {
    let mut f = fixture(true);
    f.storage.fail_after_commit_once = true;
    let error = execute(&mut f.storage, &f.command, f.owner).unwrap_err();
    assert_eq!(error.certainty, CommitCertainty::Unknown);
    let prepared = prepare(&mut f);
    assert_eq!(prepared.sequence, 1);
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(snapshot.obligations.len(), 1);
    assert_eq!(snapshot.total, vector(10));
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.obligations[0].allocation_epoch = 1;
    assert_eq!(
        execute(&mut f.storage, &f.command, f.owner)
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
}
#[test]
fn typed_prepare_rejects_orphan_family_or_claimed_retained_holder_atomically() {
    let mut f = fixture(false);
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.families[0].roles[0].1 = RoleTarget::New(1);
    assert!(execute(&mut f.storage, &f.command, f.owner).is_err());
    assert_eq!(
        snapshot(&f.storage.connection, &f.storage.witness)
            .unwrap()
            .total,
        ResourceVector::default()
    );
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.families[0].roles[0].1 = RoleTarget::New(0);
    let v2::MemberPlan::SourceV1 { source, .. } = &mut batch.obligations[0].manifest.members[1]
    else {
        unreachable!()
    };
    source.planned_copies[0].owner = v2::CopyOwner::RetainedAssets {
        asset_id: v2::Id::new(Uuid::new_v4()).unwrap(),
        source_member_id: source.member_id,
    };
    assert!(execute(&mut f.storage, &f.command, f.owner).is_err());
    assert!(snapshot(&f.storage.connection, &f.storage.witness)
        .unwrap()
        .obligations
        .is_empty());
}
#[test]
fn typed_source_acquisition_rejects_changed_association_without_advancing_sequence() {
    let mut f = fixture(false);
    let prepared = prepare(&mut f);
    let mut envelope = observed_source(&f);
    let armed = change_command(
        &prepared.obligations[0],
        sequence(&f),
        Mutation::Arm {
            members: vec![envelope.planned.member_id()],
        },
    );
    execute(&mut f.storage, &armed, f.owner).unwrap();
    let before = sequence(&f);
    let Some(v2::Observed::SourceV1 { source }) = &mut envelope.observed else {
        unreachable!()
    };
    source.source_association.object_version = "changed-source".into();
    let acquired = change_command(
        &prepared.obligations[0],
        before,
        Mutation::RecordTypedAcquired { envelope },
    );
    assert!(execute(&mut f.storage, &acquired, f.owner).is_err());
    assert_eq!(sequence(&f), before);
    assert_eq!(
        snapshot(&f.storage.connection, &f.storage.witness)
            .unwrap()
            .total,
        vector(10)
    );
}

#[test]
fn typed_late_source_copy_enrichment_is_monotonic_and_cannot_forget_a_held_copy() {
    let mut f = fixture(false);
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    let v2::MemberPlan::SourceV1 { source, .. } = &mut batch.obligations[0].manifest.members[1]
    else {
        unreachable!()
    };
    let mut copy = source.planned_copies[0].clone();
    copy.copy_id = v2::Id::new(Uuid::new_v4()).unwrap();
    copy.acquisition_operation_id = v2::Id::new(Uuid::new_v4()).unwrap();
    source.planned_copies.push(copy);
    let prepared = prepare(&mut f);
    let full = observed_source(&f);
    let mut first = full.clone();
    let Some(v2::Observed::SourceV1 { source }) = &mut first.observed else {
        unreachable!()
    };
    source.copies.truncate(1);
    let armed = change_command(
        &prepared.obligations[0],
        sequence(&f),
        Mutation::Arm {
            members: vec![full.planned.member_id()],
        },
    );
    execute(&mut f.storage, &armed, f.owner).unwrap();
    let acquired = change_command(
        &prepared.obligations[0],
        sequence(&f),
        Mutation::RecordTypedAcquired {
            envelope: first.clone(),
        },
    );
    execute(&mut f.storage, &acquired, f.owner).unwrap();
    let sealed = change_command(&prepared.obligations[0], sequence(&f), Mutation::Seal);
    execute(&mut f.storage, &sealed, f.owner).unwrap();
    let enriched = change_command(
        &prepared.obligations[0],
        sequence(&f),
        Mutation::RecordTypedAcquired {
            envelope: full.clone(),
        },
    );
    execute(&mut f.storage, &enriched, f.owner).unwrap();
    let before = sequence(&f);
    let forgotten = change_command(
        &prepared.obligations[0],
        before,
        Mutation::RecordTypedAcquired { envelope: first },
    );
    assert!(execute(&mut f.storage, &forgotten, f.owner).is_err());
    assert_eq!(sequence(&f), before);
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(snapshot.total, vector(10));
    let ObligationInventory::TypedV2 { members, .. } = &snapshot.obligations[0].inventory else {
        unreachable!()
    };
    assert!(members.iter().any(|m| m.envelope == full));
}

#[test]
fn unknown_producer_claim_is_durable_and_changes_exact_prepare_replay() {
    let mut f = fixture(false);
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.obligations[0].memory_claim = crate::adaptive_resources::MemoryClaim::UnknownProducerPeak;
    prepare(&mut f);
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(
        snapshot.obligations[0].memory_claim,
        crate::adaptive_resources::MemoryClaim::UnknownProducerPeak
    );
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.obligations[0].memory_claim = crate::adaptive_resources::MemoryClaim::default();
    assert_eq!(
        execute(&mut f.storage, &f.command, f.owner)
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
}

#[test]
fn typed_prepare_cannot_claim_resident_without_retirement() {
    let mut f = fixture(false);
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.obligations[0].memory_claim = crate::adaptive_resources::MemoryClaim::KnownBounded {
        active_producer: false,
    };
    assert!(execute(&mut f.storage, &f.command, f.owner).is_err());
    let rows: i64 = f
        .storage
        .connection
        .query_row("SELECT count(*) FROM obligations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

fn legacy_prepare_command(f: &Fixture) -> (QueuedOperation, ResourceReservation) {
    let (new, reservation) = new_obligation(f.owner, vector(10));
    let handle = ObligationHandle {
        journal_id: f.storage.witness.journal_id,
        owner_id: f.owner,
        obligation_id: Uuid::new_v4(),
        graph_id: new.manifest.graph_id,
        runtime_binding: new.runtime_binding,
        #[cfg(target_os = "linux")]
        recovery: None,
    };
    let command = QueuedOperation {
        id: Uuid::new_v4(),
        deadline: deadline(),
        command: Command::Prepare {
            batch: PrepareBatch {
                obligations: vec![new],
                families: vec![FamilyRequest {
                    family: FamilyHandle {
                        journal_id: f.storage.witness.journal_id,
                        family_id: Uuid::new_v4(),
                    },
                    roles: vec![(Role::Audio, RoleTarget::New(0))],
                }],
                trial: false,
            },
            handles: vec![handle],
            trial: None,
        },
    };
    (command, reservation)
}

#[test]
fn legacy_prepare_cannot_join_an_active_unknown_producer() {
    let mut f = fixture(false);
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.obligations[0].memory_claim = crate::adaptive_resources::MemoryClaim::UnknownProducerPeak;
    prepare(&mut f);
    let (legacy, _reservation) = legacy_prepare_command(&f);
    let sequence_before: i64 = f
        .storage
        .connection
        .query_row("SELECT next_sequence FROM journal_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        execute(&mut f.storage, &legacy, f.owner).unwrap_err().kind,
        ErrorKind::Conflict
    );
    assert_eq!(
        f.storage
            .connection
            .query_row("SELECT next_sequence FROM journal_meta", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        sequence_before
    );
    assert_eq!(
        f.storage
            .connection
            .query_row("SELECT count(*) FROM obligations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn typed_unknown_producer_cannot_join_an_active_legacy_prepare() {
    let mut f = fixture(false);
    let (legacy, _reservation) = legacy_prepare_command(&f);
    assert!(matches!(
        execute(&mut f.storage, &legacy, f.owner).unwrap(),
        OperationResult::Prepared(_)
    ));
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.obligations[0].memory_claim = crate::adaptive_resources::MemoryClaim::UnknownProducerPeak;
    let sequence_before: i64 = f
        .storage
        .connection
        .query_row("SELECT next_sequence FROM journal_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        execute(&mut f.storage, &f.command, f.owner)
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
    assert_eq!(
        f.storage
            .connection
            .query_row("SELECT next_sequence FROM journal_meta", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        sequence_before
    );
    assert_eq!(
        f.storage
            .connection
            .query_row("SELECT count(*) FROM obligations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

mod copy_task_settlement;

#[test]
fn v7_unknown_adapter_or_memory_claim_cannot_replace_the_original_prepared_rows() {
    let mut f = fixture(false);
    let prepared = prepare(&mut f);
    let obligation = prepared.obligations[0].obligation_id;
    let before: Vec<(String, String)> = f
        .storage
        .connection
        .prepare("SELECT adapter,identity_json FROM members ORDER BY member_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(f
        .storage
        .connection
        .execute(
            "UPDATE members SET adapter='unrecognized_adapter' WHERE obligation_id=?1",
            [obligation.as_bytes()]
        )
        .is_err());
    assert!(f
        .storage
        .connection
        .execute(
            "UPDATE obligations SET memory_claim='unrecognized_claim' WHERE obligation_id=?1",
            [obligation.as_bytes()]
        )
        .is_err());
    let after: Vec<(String, String)> = f
        .storage
        .connection
        .prepare("SELECT adapter,identity_json FROM members ORDER BY member_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        crate::resource_custody::transaction::snapshot(&f.storage.connection, &f.storage.witness)
            .unwrap()
            .obligations
            .len(),
        1
    );
}
