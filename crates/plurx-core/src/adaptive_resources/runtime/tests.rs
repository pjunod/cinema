//! Authored runtime integration regressions; compile only until final review.
use super::*;
use crate::adaptive_resources::{CleanupAuthority, PhysicalCloseReceipt, WorkloadDemand};
use crate::resource_custody::{
    Adapter, DaemonLockGuard, FamilyEdge, FamilyRequest, InstallationContext, Manifest, MemberKind,
    NewObligation, ObligationRecord, OperationResult, PlannedMember, PrepareBatch, RecoveryMethod,
    RoleTarget,
};
use std::time::Duration;

fn vector(value: u64) -> ResourceVector {
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

fn manifest(owner: Uuid) -> Manifest {
    Manifest {
        version: 1,
        owner_id: owner,
        graph_id: Uuid::new_v4(),
        attempt_id: None,
        members: vec![PlannedMember {
            member_id: Uuid::new_v4(),
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: "runtime-fixture".into(),
            locator: "unexecuted-root-slot".into(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "no positive fixture backend".into(),
            },
            owned_by: None,
            depends_on: Vec::new(),
            maximum_cardinality: 1,
            protected_paths: Vec::new(),
        }],
    }
}

fn snapshot(phases: &[Phase], trial: bool) -> RecoverySnapshot {
    let owner = daemon_owner("fixture-node".into(), "fixture-cluster".into());
    let trial_id = trial.then(Uuid::new_v4);
    let family = Uuid::new_v4();
    let obligations: Vec<_> = phases
        .iter()
        .map(|phase| {
            let manifest = manifest(owner.owner_id);
            ObligationRecord {
                obligation_id: Uuid::new_v4(),
                graph_id: manifest.graph_id,
                owner_id: owner.owner_id,
                allocation_epoch: u64::MAX,
                resources: vector(2),
                memory_claim: crate::adaptive_resources::MemoryClaim::default(),
                trial_group_id: trial_id,
                phase: *phase,
                sequence: 1,
                inventory: crate::resource_custody::ObligationInventory::LegacyV1 {
                    manifest,
                    members: Vec::new(),
                },
                hold_reason: None,
                cleanup_only: true,
            }
        })
        .collect();
    RecoverySnapshot {
        journal_id: Uuid::new_v4(),
        installation_id: Uuid::new_v4(),
        families: obligations
            .iter()
            .enumerate()
            .map(|(index, record)| FamilyEdge {
                family_id: if index < 3 { family } else { Uuid::new_v4() },
                role: [Role::Video, Role::Audio, Role::Companion][index % 3],
                obligation_id: record.obligation_id,
            })
            .collect(),
        total: vector(u64::try_from(phases.len()).expect("fixture count") * 2),
        obligations,
        trial_groups: trial_id.into_iter().collect(),
        protected_paths: Vec::new(),
        recovered_s_holders: Vec::new(),
        recovered_r_holders: Vec::new(),
        owners: vec![owner],
        retained_assets: Vec::new(),
    }
}

#[test]
fn every_persisted_phase_restores_full_cleanup_only_unknown_held_above_new_cap() {
    let recovered = snapshot(
        &[
            Phase::Prepared,
            Phase::AcquisitionPossible,
            Phase::Sealed,
            Phase::UnknownHeld,
        ],
        false,
    );
    assert!(recovered
        .obligations
        .iter()
        .all(|record| record.allocation_epoch == u64::MAX));
    let (ledger, bindings) = restore_accounting(10, Metric::Known(vector(1)), &recovered)
        .expect("over-cap holds must not be clamped or discarded");
    let held = ledger.snapshot().expect("held snapshot");
    assert_eq!(held.used, vector(8));
    assert_eq!(held.held, 4);
    assert_eq!(held.unknown_held, 4);
    assert_eq!(bindings.len(), 4);
    assert!(ledger
        .retained_physical_graphs()
        .expect("no synthetic graph")
        .is_empty());
    assert!(ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(vector(1)),
            limit_epoch: 10,
            trial: false,
        })
        .is_err());
    assert!(ledger
        .inner
        .lock()
        .expect("private state")
        .entries
        .values()
        .all(|entry| {
            entry.binding.allocation_epoch == 10
                && entry.state == ReservationState::UnknownHeld
                && entry.custody.is_none()
        }));
}

#[test]
fn shared_family_edges_count_once_and_all_trial_members_keep_the_latch() {
    let mut recovered = snapshot(&[Phase::Sealed, Phase::Prepared], true);
    recovered.families.push(FamilyEdge {
        family_id: Uuid::new_v4(),
        role: Role::Audio,
        obligation_id: recovered.obligations[1].obligation_id,
    });
    let (ledger, _) =
        restore_accounting(1, Metric::Known(vector(100)), &recovered).expect("shared full edges");
    let held = ledger.snapshot().expect("snapshot");
    assert_eq!(held.used, vector(4));
    assert_eq!(held.held, 2);
    assert!(held.trial_held);
    assert_eq!(
        ledger
            .inner
            .lock()
            .expect("trial state")
            .trial
            .as_ref()
            .expect("latch")
            .len(),
        2
    );
    assert!(matches!(
        ledger.reserve(WorkloadDemand {
            resources: Metric::Known(vector(1)),
            limit_epoch: 1,
            trial: true,
        }),
        Err(AdmissionRefusal::TrialAlreadyHeld)
    ));
}

#[test]
fn truncated_totals_trials_and_family_edges_never_construct_empty_accounting() {
    let good = snapshot(&[Phase::Prepared, Phase::Sealed], true);
    let mut bad = good.clone();
    bad.total = ResourceVector::default();
    assert!(restore_accounting(1, Metric::Known(vector(100)), &bad).is_err());
    bad = good.clone();
    bad.trial_groups.clear();
    assert!(restore_accounting(1, Metric::Known(vector(100)), &bad).is_err());
    bad = good.clone();
    bad.obligations[0].cleanup_only = false;
    assert!(restore_accounting(1, Metric::Known(vector(100)), &bad).is_err());
    bad = good.clone();
    bad.families.pop();
    assert!(restore_accounting(1, Metric::Known(vector(100)), &bad).is_err());
    bad = good.clone();
    bad.obligations[0].phase = Phase::Settled;
    assert!(restore_accounting(1, Metric::Known(vector(100)), &bad).is_err());
    bad = good.clone();
    bad.obligations[1].resources = vector(u64::MAX);
    assert!(restore_accounting(1, Metric::Known(vector(100)), &bad).is_err());
    bad = good;
    bad.families.push(bad.families[0].clone());
    assert!(restore_accounting(1, Metric::Known(vector(100)), &bad).is_err());
}

#[test]
fn restored_binding_is_fresh_and_an_old_receipt_cannot_refund_it() {
    let recovered = snapshot(&[Phase::Sealed], false);
    let (old, old_bindings) =
        restore_accounting(0, Metric::Known(vector(100)), &recovered).expect("old");
    let (new, new_bindings) =
        restore_accounting(0, Metric::Known(vector(1)), &recovered).expect("new");
    let obligation = recovered.obligations[0].obligation_id;
    assert_eq!(recovered.obligations[0].allocation_epoch, u64::MAX);
    assert_eq!(old_bindings[&obligation].allocation_epoch, 0);
    assert_eq!(new_bindings[&obligation].allocation_epoch, 0);
    assert_ne!(old_bindings[&obligation], new_bindings[&obligation]);
    let cleanup = CleanupAuthority {
        ledger: new.clone(),
        binding: new_bindings[&obligation],
    };
    assert_eq!(
        cleanup
            .close(PhysicalCloseReceipt {
                binding: old_bindings[&obligation],
                #[cfg(target_os = "linux")]
                _recovered_r: None,
            })
            .map_err(|rejected| rejected.reason),
        Err(AdmissionRefusal::ReceiptMismatch)
    );
    assert_eq!(new.snapshot().expect("new still held").used, vector(2));
    assert_eq!(old.snapshot().expect("old unchanged").used, vector(2));
}

#[test]
fn daemon_owner_uses_actual_graph_process_instance_without_continuity_claim() {
    let owner = daemon_owner("fixture-node".into(), "fixture-cluster".into());
    let ledger = ResourceLedger::new(1, Metric::Known(vector(100))).expect("ledger");
    let (reservation, _) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(vector(2)),
            limit_epoch: 1,
            trial: false,
        })
        .expect("reservation");
    let graph = reservation.into_physical_graph().expect("graph");
    assert_eq!(owner.owner_id, graph.identity().process_instance);
    assert_eq!(owner.daemon_process.pid, std::process::id());
    assert!(owner.platform_domain.host_identity.is_none());
    assert!(owner.platform_domain.continuity_provider.is_none());
}

pub(super) fn options(
    root: &Path,
    owner: OwnerIdentity,
    installation: InstallationContext,
) -> OpenOptions {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("fixture.lock"))
        .expect("fixture lock");
    OpenOptions {
        data_dir: std::fs::canonicalize(root).expect("canonical fixture"),
        excluded_roots: Vec::new(),
        installation,
        owner,
        daemon_lock: DaemonLockGuard::from_held_lock(file),
        queue_capacity: 4,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn actual_journal_reopen_retains_prepared_trial_cost_paths_and_owner_data() {
    let root = tempfile::tempdir().expect("durable TMPDIR fixture");
    let mut owner = daemon_owner("fixture-node".into(), "fixture-cluster".into());
    // Model a former instance. Reusing this process's real singleton UUID on
    // reopen must conflict: only actual daemon restart creates a new instance.
    owner.owner_id = Uuid::new_v4();
    let (journal, _) = CustodyJournal::open(options(
        root.path(),
        owner.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("real journal");
    let ledger = ResourceLedger::new(1, Metric::Known(vector(100))).expect("fixture ledger");
    let (reservation, _) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(vector(2)),
            limit_epoch: 1,
            trial: true,
        })
        .expect("fixture binding");
    let protected = root.path().join("pending-source-scratch");
    let mut manifest = manifest(owner.owner_id);
    manifest.members[0]
        .protected_paths
        .push(protected.to_str().expect("fixture path").into());
    let operation = Uuid::new_v4();
    let result = journal
        .prepare(
            operation,
            PrepareBatch {
                obligations: vec![NewObligation {
                    runtime_binding: reservation.custody_binding(),
                    allocation_epoch: 1,
                    resources: vector(2),
                    manifest,
                }],
                families: vec![FamilyRequest {
                    family: journal.new_family(),
                    roles: vec![(Role::Video, RoleTarget::New(0))],
                }],
                trial: true,
            },
            Instant::now() + Duration::from_secs(30),
        )
        .expect("prepare enqueued")
        .wait()
        .await
        .expect("prepare committed");
    assert!(matches!(result, OperationResult::Prepared(_)));
    journal.acknowledge(operation).expect("ack");
    journal
        .drain()
        .expect("actual DB owner joined before reopen");
    let runtime = NodeResourceRuntime::open(options(
        root.path(),
        daemon_owner("fixture-node".into(), "fixture-cluster".into()),
        InstallationContext::Existing,
    ))
    .expect("composed real reconstruction");
    assert!(runtime.has_recovered_obligations());
    assert_eq!(runtime.accounting().expect("full charge").used, vector(2));
    assert_eq!(runtime.accounting().expect("full hold").unknown_held, 1);
    assert!(runtime.accounting().expect("trial").trial_held);
    assert_eq!(runtime.recovery().obligations[0].phase, Phase::Prepared);
    assert_eq!(
        runtime.protected_paths().collect::<Vec<_>>(),
        vec![protected.as_path()]
    );
    assert_eq!(runtime.recovery().owners.len(), 1);
    assert_eq!(runtime.recovery().owners[0].owner_id, owner.owner_id);
    assert_ne!(runtime.owner().owner_id, owner.owner_id);
    runtime.drain().expect("actual composed DB owner joined");
    std::fs::remove_file(root.path().join("resource-custody/installation.json"))
        .expect("fault fixture only");
    assert!(NodeResourceRuntime::open(options(
        root.path(),
        daemon_owner("fixture-node".into(), "fixture-cluster".into()),
        InstallationContext::Existing
    ))
    .is_err());
    assert!(root
        .path()
        .join("resource-custody/obligations.sqlite3")
        .exists());
}

#[test]
fn recovered_unknown_peak_keeps_full_charge_and_excludes_new_producers() {
    let mut recovered = snapshot(&[Phase::Sealed], false);
    recovered.obligations[0].memory_claim =
        crate::adaptive_resources::MemoryClaim::UnknownProducerPeak;
    let (ledger, _) = restore_accounting(2, Metric::Known(vector(100)), &recovered)
        .expect("restore the original complete hold");
    let current = ledger.snapshot().expect("restored accounting");
    assert_eq!(current.used, vector(2));
    assert_eq!(current.unknown_held, 1);
    assert_eq!(current.unknown_peak_producers, 1);
    assert!(matches!(
        current.managed_memory_peak_bytes,
        Metric::Unknown(_)
    ));
    assert!(matches!(
        ledger.reserve(WorkloadDemand {
            resources: Metric::Known(vector(1)),
            limit_epoch: 2,
            trial: false,
        }),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    assert_eq!(
        ledger.snapshot().expect("held after refusal").used,
        vector(2)
    );
}

#[test]
fn conflicting_recovered_claims_keep_every_hold_and_fence_new_work() {
    let mut recovered = snapshot(&[Phase::Sealed, Phase::Prepared], false);
    recovered.obligations[0].memory_claim =
        crate::adaptive_resources::MemoryClaim::UnknownProducerPeak;
    let (ledger, _) = restore_accounting(2, Metric::Known(vector(100)), &recovered)
        .expect("preserve inconsistent accounting without empty authority");
    let current = ledger
        .snapshot()
        .expect("all restored holds remain visible");
    assert_eq!(current.used, vector(4));
    assert_eq!(current.held, 2);
    assert_eq!(current.unknown_held, 2);
    assert!(matches!(
        ledger.reserve(WorkloadDemand {
            resources: Metric::Known(vector(1)),
            limit_epoch: 2,
            trial: false,
        }),
        Err(AdmissionRefusal::Unavailable)
    ));
    assert_eq!(
        ledger.snapshot().expect("conservative holds").used,
        vector(4)
    );
}
