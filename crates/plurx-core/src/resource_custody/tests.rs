//! Authored regression bodies. Execute only after integrated review/fixes.
use super::*;
mod faults;
mod invariants;
use crate::adaptive_resources::{ResourceLedger, WorkloadDemand};
use crate::process::observation::Metric;

pub(super) fn owner() -> OwnerIdentity {
    OwnerIdentity {
        owner_id: Uuid::new_v4(),
        node_id: "fixture-node".into(),
        cluster_id: "fixture-cluster".into(),
        os_boot: BootIdentity::Unknown {
            reason: "fixture has no extinction authority".into(),
        },
        daemon_process: ProcessIdentity {
            version: 1,
            pid: std::process::id(),
            creation_marker: None,
            domain: "fixture-domain".into(),
        },
        platform_domain: PlatformDomain {
            version: 1,
            platform: std::env::consts::OS.into(),
            host_identity: None,
            namespace: None,
            continuity_provider: None,
        },
    }
}
pub(super) fn options(
    root: &std::path::Path,
    identity: OwnerIdentity,
    installation: InstallationContext,
) -> OpenOptions {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("daemon.lock"))
        .expect("fixture lock file");
    OpenOptions {
        data_dir: std::fs::canonicalize(root).expect("canonical fixture"),
        excluded_roots: Vec::new(),
        installation,
        owner: identity,
        daemon_lock: DaemonLockGuard::from_held_lock(lock),
        queue_capacity: 4,
    }
}
fn manifest(owner: Uuid) -> Manifest {
    Manifest {
        version: 1,
        graph_id: Uuid::new_v4(),
        owner_id: owner,
        attempt_id: None,
        members: vec![PlannedMember {
            member_id: Uuid::new_v4(),
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: "fixture-domain".into(),
            locator: "predeclared-root-slot".into(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "fixture is not containment proof".into(),
            },
            owned_by: None,
            depends_on: Vec::new(),
            maximum_cardinality: 1,
            protected_paths: Vec::new(),
        }],
    }
}
pub(super) fn vector(value: u64) -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: value,
        peak_memory_bytes: value,
        handles: value,
        processes: value,
        hardware_slots: value,
        tuner_slots: value,
        scratch_bytes: value,
    }
}
pub(super) fn deadline() -> Instant {
    Instant::now() + std::time::Duration::from_secs(30)
}

#[test]
fn full_u64_vector_and_epoch_codecs_never_truncate_to_sqlite_integer() {
    let value = vector(u64::MAX);
    assert_eq!(
        codec::vector_from_bytes(&codec::vector_bytes(value)).expect("vector"),
        value
    );
    assert_eq!(
        codec::epoch_from_bytes(&u64::MAX.to_le_bytes()).expect("epoch"),
        u64::MAX
    );
    assert!(codec::vector_from_bytes(&[0; 55]).is_err());
    assert!(codec::epoch_from_bytes(&[0; 7]).is_err());
    assert!(codec::uuid_from_bytes(&[0; 16]).is_err());
}
#[test]
fn unknown_versions_fields_adapters_and_manifest_cycles_refuse() {
    assert!(codec::decode::<ProcessIdentity>(
        r#"{"version":1,"pid":1,"creation_marker":null,"domain":"node","extra":0}"#
    )
    .is_err());
    let mut m = manifest(Uuid::new_v4());
    m.members[0].adapter_version = 2;
    assert!(codec::validate_manifest(&m).is_err());
    m.members[0].adapter_version = 1;
    let self_id = m.members[0].member_id;
    m.members[0].depends_on.push(self_id);
    assert!(codec::validate_manifest(&m).is_err());
    assert!(codec::decode::<Manifest>(&"x".repeat(codec::JSON_LIMIT + 1)).is_err());
}
#[test]
fn canonical_schema_rejects_same_version_missing_constraint_or_foreign_trigger() {
    let db = rusqlite::Connection::open_in_memory().expect("db");
    db.execute_batch(schema::DDL).expect("ddl");
    schema::validate(&db).expect("exact recognized shape");
    db.execute_batch("CREATE TRIGGER foreign_writer AFTER INSERT ON owners BEGIN SELECT 1; END;")
        .expect("foreign trigger");
    assert_eq!(
        schema::validate(&db)
            .expect_err("must refuse extra shape")
            .kind,
        ErrorKind::UnreadableAuthority
    );
}
#[cfg(unix)]
#[test]
fn installation_missing_ready_database_or_witness_preserves_remaining_authority() {
    let root = tempfile::tempdir().expect("durable TMPDIR fixture");
    let identity = owner();
    let store = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    drop(store);
    let path = root.path().join("resource-custody");
    let witness = std::fs::read(path.join("installation.json")).expect("witness bytes");
    std::fs::rename(
        path.join("obligations.sqlite3"),
        path.join("preserved.sqlite3"),
    )
    .expect("preserve db");
    assert!(storage::Storage::open(&options(
        root.path(),
        identity,
        InstallationContext::Existing
    ))
    .is_err());
    assert_eq!(
        std::fs::read(path.join("installation.json")).expect("unchanged witness"),
        witness
    );
    assert!(!path.join("obligations.sqlite3").exists());
}
#[cfg(unix)]
#[test]
fn corrupt_future_or_foreign_installation_never_rebuilds() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let store = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    store
        .connection
        .execute_batch("PRAGMA user_version=99;")
        .expect("future format");
    drop(store);
    let db = root.path().join("resource-custody/obligations.sqlite3");
    let before = std::fs::read(&db).expect("before bytes");
    assert!(storage::Storage::open(&options(
        root.path(),
        identity,
        InstallationContext::Existing
    ))
    .is_err());
    assert_eq!(std::fs::read(&db).expect("after bytes"), before);
    let mut foreign = owner();
    foreign.node_id = "foreign-node".into();
    assert!(storage::Storage::open(&options(
        root.path(),
        foreign,
        InstallationContext::Existing
    ))
    .is_err());
}
#[cfg(unix)]
#[test]
fn symlink_hardlink_and_directory_replacement_refuse_without_empty_fallback() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let store = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    let path = root.path().join("resource-custody");
    std::fs::hard_link(
        path.join("obligations.sqlite3"),
        root.path().join("db-alias"),
    )
    .expect("hardlink fixture");
    assert!(store.verify().is_err());
    std::fs::remove_file(root.path().join("db-alias")).expect("remove alias");
    std::fs::rename(&path, root.path().join("retained-custody")).expect("preserve old authority");
    std::os::unix::fs::symlink(root.path().join("retained-custody"), &path)
        .expect("symlink replacement");
    assert!(store.verify().is_err());
    assert!(storage::Storage::open(&options(
        root.path(),
        identity,
        InstallationContext::Existing
    ))
    .is_err());
}

// These fixtures obtain the actual process-local binding through the main-owned
// read-only registration seam; they never mint a PhysicalCloseReceipt.
pub(super) fn new_obligation(
    identity: Uuid,
    resources: ResourceVector,
) -> (
    NewObligation,
    crate::adaptive_resources::ResourceReservation,
) {
    let ledger = ResourceLedger::new(1, Metric::Known(vector(u64::MAX))).expect("fixture ledger");
    let (reservation, _cleanup) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(resources),
            limit_epoch: 1,
            trial: false,
        })
        .expect("fixture reservation");
    let new = NewObligation {
        runtime_binding: reservation.custody_binding(),
        allocation_epoch: 1,
        resources,
        manifest: manifest(identity),
    };
    (new, reservation)
}
#[cfg(unix)]
#[tokio::test]
async fn atomic_family_prepare_shared_roles_count_once_and_retain_trial_group() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let (journal, _) = CustodyJournal::open(options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("open");
    let (video, _v) = new_obligation(identity.owner_id, vector(2));
    let (audio, _a) = new_obligation(identity.owner_id, vector(1));
    let f1 = journal.new_family();
    let f2 = journal.new_family();
    let batch = PrepareBatch {
        obligations: vec![video, audio],
        families: vec![
            FamilyRequest {
                family: f1,
                roles: vec![
                    (Role::Video, RoleTarget::New(0)),
                    (Role::Audio, RoleTarget::New(1)),
                ],
            },
            FamilyRequest {
                family: f2,
                roles: vec![(Role::Audio, RoleTarget::New(1))],
            },
        ],
        trial: true,
    };
    let op = Uuid::new_v4();
    let OperationResult::Prepared(prepared) = journal
        .prepare(op, batch, deadline())
        .expect("queue")
        .wait()
        .await
        .expect("prepare")
    else {
        panic!("prepared result")
    };
    assert_eq!(prepared.obligations.len(), 2);
    let store = storage::Storage::open(&options(
        root.path(),
        identity,
        InstallationContext::Existing,
    ))
    .expect("read fixture");
    let snapshot = transaction::snapshot(&store.connection, &store.witness).expect("full snapshot");
    assert_eq!(snapshot.total, vector(3));
    assert_eq!(snapshot.families.len(), 3);
    assert_eq!(snapshot.trial_groups.len(), 1);
    journal.acknowledge(op).expect("ack prepare");
    journal.drain().expect("drain");
}
#[cfg(unix)]
#[tokio::test]
async fn caller_drop_cannot_cancel_committed_arm_or_recovered_charge() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let (journal, _) = CustodyJournal::open(options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("open");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let member = new.manifest.members[0].member_id;
    let batch = PrepareBatch {
        obligations: vec![new],
        families: vec![FamilyRequest {
            family: journal.new_family(),
            roles: vec![(Role::Video, RoleTarget::New(0))],
        }],
        trial: true,
    };
    let OperationResult::Prepared(prepared) = journal
        .prepare(Uuid::new_v4(), batch, deadline())
        .expect("queue")
        .wait()
        .await
        .expect("prepare")
    else {
        panic!("prepared")
    };
    let key = TransitionKey {
        handle: prepared.obligations[0].clone(),
        operation_id: Uuid::new_v4(),
        expected_sequence: prepared.sequence,
    };
    drop(
        journal
            .change(
                key,
                Mutation::Arm {
                    members: vec![member],
                },
                deadline(),
            )
            .expect("enqueue transfers custody"),
    );
    journal.drain().expect("drains cancelled waiter operation");
    let (restarted, snapshot) =
        CustodyJournal::open(options(root.path(), owner(), InstallationContext::Existing))
            .expect("restart");
    assert_eq!(snapshot.total, vector(1));
    assert_eq!(snapshot.obligations[0].phase, Phase::AcquisitionPossible);
    assert_eq!(snapshot.trial_groups.len(), 1);
    restarted.drain().expect("drain restart");
}
#[cfg(unix)]
#[tokio::test]
async fn prepared_cancel_commits_before_refund_and_exact_retry_is_idempotent() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let (journal, _) = CustodyJournal::open(options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("open");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let batch = PrepareBatch {
        obligations: vec![new],
        families: vec![FamilyRequest {
            family: journal.new_family(),
            roles: vec![(Role::Video, RoleTarget::New(0))],
        }],
        trial: true,
    };
    let OperationResult::Prepared(prepared) = journal
        .prepare(Uuid::new_v4(), batch, deadline())
        .expect("queue")
        .wait()
        .await
        .expect("prepare")
    else {
        panic!("prepared")
    };
    let op = Uuid::new_v4();
    let key = TransitionKey {
        handle: prepared.obligations[0].clone(),
        operation_id: op,
        expected_sequence: prepared.sequence,
    };
    let OperationResult::Changed(first) = journal
        .change(key.clone(), Mutation::CancelPrepared, deadline())
        .expect("cancel")
        .wait()
        .await
        .expect("commit")
    else {
        panic!("cancel ack")
    };
    let OperationResult::Changed(second) = journal
        .retry(op, deadline())
        .expect("exact retry")
        .wait()
        .await
        .expect("same commit")
    else {
        panic!("retry ack")
    };
    assert_eq!(first.sequence, second.sequence);
    assert_eq!(first.phase, Phase::Settled);
    assert!(journal
        .change(
            TransitionKey {
                operation_id: Uuid::new_v4(),
                ..key
            },
            Mutation::Arm {
                members: Vec::new()
            },
            deadline()
        )
        .expect("enqueue stale")
        .wait()
        .await
        .is_err());
    journal.drain().expect("drain");
    let (restarted, snapshot) =
        CustodyJournal::open(options(root.path(), owner(), InstallationContext::Existing))
            .expect("restart");
    assert_eq!(snapshot.total, ResourceVector::default());
    assert!(snapshot.obligations.is_empty());
    assert!(snapshot.trial_groups.is_empty());
    restarted.drain().expect("drain restart");
}
#[cfg(unix)]
#[tokio::test]
async fn invalid_atomic_family_transaction_exposes_no_half_family() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let (journal, _) = CustodyJournal::open(options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("open");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let batch = PrepareBatch {
        obligations: vec![new],
        families: vec![FamilyRequest {
            family: journal.new_family(),
            roles: vec![
                (Role::Video, RoleTarget::New(0)),
                (Role::Audio, RoleTarget::New(999)),
            ],
        }],
        trial: false,
    };
    assert!(journal
        .prepare(Uuid::new_v4(), batch, deadline())
        .expect("queue")
        .wait()
        .await
        .is_err());
    journal.drain().expect("drain");
    let (restarted, snapshot) =
        CustodyJournal::open(options(root.path(), owner(), InstallationContext::Existing))
            .expect("restart");
    assert!(snapshot.obligations.is_empty());
    assert!(snapshot.families.is_empty());
    assert_eq!(snapshot.total, ResourceVector::default());
    restarted.drain().expect("drain restart");
}
