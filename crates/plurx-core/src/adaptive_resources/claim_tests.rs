use super::*;
use crate::process::observation::{Metric, UnknownReason};

fn capacity(cpu: Metric<u64>, memory: Metric<u64>) -> ResourceCapacity {
    ResourceCapacity {
        cpu_ns_per_second: cpu,
        memory_bytes: memory,
        handles: Metric::Known(100),
        kernel_tasks: Metric::Known(100),
        processes: Metric::Known(100),
        hardware_slots: Metric::Known(100),
        tuner_slots: Metric::Known(100),
        scratch_bytes: Metric::Known(100),
    }
}

fn vector(cpu: u64, memory: u64) -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: cpu,
        peak_memory_bytes: memory,
        handles: 1,
        processes: 1,
        ..ResourceVector::default()
    }
}

fn demand(epoch: u64, cpu: u64, memory: u64) -> WorkloadDemand {
    WorkloadDemand {
        resources: Metric::Known(vector(cpu, memory)),
        limit_epoch: epoch,
        trial: false,
    }
}

fn one_role<'a>(
    ledger: &'a ResourceLedger,
    role: FamilyRole,
    demand: WorkloadDemand,
) -> (FamilyIdentity, [FamilyRoleDemand<'a>; 1]) {
    (
        ledger.new_family().expect("family identity"),
        [FamilyRoleDemand {
            role,
            demand,
            retained: None,
        }],
    )
}

fn reserve_single(
    ledger: &ResourceLedger,
    role: FamilyRole,
    demand: WorkloadDemand,
    claim: Option<MemoryClaim>,
) -> Result<Vec<(ResourceReservation, CleanupAuthority)>, AdmissionRefusal> {
    let (identity, roles) = one_role(ledger, role, demand);
    let families = [FamilyDemand {
        identity,
        roles: &roles,
    }];
    match claim {
        Some(claim) => ledger.reserve_families_claiming(&families, Some(&[claim])),
        None => ledger.reserve_families_claiming(&families, None),
    }
}

fn ledger(cpu: Metric<u64>, memory: Metric<u64>) -> ResourceLedger {
    ResourceLedger::with_capacity(1, capacity(cpu, memory)).expect("ledger")
}

#[test]
fn unknown_peak_batch_is_rejected_atomically() {
    let ledger = ledger(
        Metric::Known(100),
        Metric::Unknown(UnknownReason::InsufficientEvidence),
    );
    let (first_family, first_roles) = one_role(&ledger, FamilyRole::Video, demand(1, 1, 8));
    let (second_family, second_roles) = one_role(&ledger, FamilyRole::Audio, demand(1, 1, 8));
    let families = [
        FamilyDemand {
            identity: first_family,
            roles: &first_roles,
        },
        FamilyDemand {
            identity: second_family,
            roles: &second_roles,
        },
    ];
    let claims = [MemoryClaim::UnknownProducerPeak; 2];

    let result = ledger.reserve_families_claiming(&families, Some(&claims));

    assert!(matches!(
        result,
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    let snapshot = ledger.snapshot().expect("unchanged ledger");
    assert_eq!(snapshot.used, ResourceVector::default());
    assert_eq!(snapshot.held, 0);
    assert_eq!(snapshot.unknown_peak_producers, 0);
}

#[test]
fn unknown_and_bounded_active_claims_exclude_each_other_in_both_orders() {
    let cap = || capacity(Metric::Known(100), Metric::Known(100));
    let unknown = MemoryClaim::UnknownProducerPeak;

    let unknown_first = ResourceLedger::with_capacity(1, cap()).expect("ledger");
    let mut held = reserve_single(
        &unknown_first,
        FamilyRole::Video,
        demand(1, 1, 8),
        Some(unknown),
    )
    .expect("serialized cold producer");
    assert!(matches!(
        reserve_single(&unknown_first, FamilyRole::Audio, demand(1, 1, 8), None),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(held.pop());

    let bounded_first = ResourceLedger::with_capacity(1, cap()).expect("ledger");
    let mut held = reserve_single(&bounded_first, FamilyRole::Video, demand(1, 1, 8), None)
        .expect("bounded active producer");
    assert!(matches!(
        reserve_single(
            &bounded_first,
            FamilyRole::Audio,
            demand(1, 1, 8),
            Some(unknown)
        ),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(held.pop());
}

#[test]
fn dropping_spawn_possible_unknown_reservation_keeps_claim_and_charge() {
    let ledger = ledger(
        Metric::Known(100),
        Metric::Unknown(UnknownReason::InsufficientEvidence),
    );
    let mut held = reserve_single(
        &ledger,
        FamilyRole::Video,
        demand(1, 1, 8),
        Some(MemoryClaim::UnknownProducerPeak),
    )
    .expect("serialized cold producer");
    let (mut reservation, cleanup) = held.pop().expect("reservation");
    reservation
        .mark_spawn_possible()
        .expect("spawn ownership remains uncertain");
    drop(reservation);

    let snapshot = ledger.snapshot().expect("unknown hold remains");
    assert_eq!(snapshot.used, vector(1, 8));
    assert_eq!(snapshot.held, 1);
    assert_eq!(snapshot.unknown_held, 1);
    assert_eq!(snapshot.unknown_peak_producers, 1);
    assert_eq!(snapshot.active_bounded_producers, 0);
    assert!(matches!(
        reserve_single(&ledger, FamilyRole::Audio, demand(1, 1, 8), None),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(cleanup);
    assert_eq!(ledger.snapshot().expect("still held").used, vector(1, 8));
}

#[test]
fn resident_claim_cannot_be_requested_without_retirement_swap() {
    let ledger = ledger(Metric::Known(100), Metric::Known(100));
    assert!(matches!(
        reserve_single(
            &ledger,
            FamilyRole::Video,
            demand(1, 1, 8),
            Some(MemoryClaim::KnownBounded {
                active_producer: false
            })
        ),
        Err(AdmissionRefusal::InvalidBundle)
    ));
    let snapshot = ledger.snapshot().expect("no residency minted");
    assert_eq!(snapshot.used, ResourceVector::default());
    assert_eq!(snapshot.held, 0);
}

#[test]
fn unknown_memory_keeps_known_cpu_hard_limit_effective() {
    let ledger = ledger(
        Metric::Known(4),
        Metric::Unknown(UnknownReason::InsufficientEvidence),
    );
    assert!(matches!(
        reserve_single(
            &ledger,
            FamilyRole::Video,
            demand(1, 5, 8),
            Some(MemoryClaim::UnknownProducerPeak)
        ),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::Cpu))
    ));
}

#[test]
fn unknown_nonmemory_capacity_refuses_serialized_cold_work() {
    let ledger = ledger(
        Metric::Unknown(UnknownReason::InsufficientEvidence),
        Metric::Unknown(UnknownReason::InsufficientEvidence),
    );
    assert!(matches!(
        reserve_single(
            &ledger,
            FamilyRole::Video,
            demand(1, 1, 8),
            Some(MemoryClaim::UnknownProducerPeak)
        ),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    let snapshot = ledger.snapshot().expect("refusal keeps ledger empty");
    assert_eq!(snapshot.used, ResourceVector::default());
    assert_eq!(snapshot.held, 0);
}

#[test]
fn unknown_kernel_task_peak_serializes_process_roots_in_both_orders() {
    let cap = || capacity(Metric::Known(100), Metric::Known(100));
    let first = ledger(Metric::Known(100), Metric::Known(100));
    let mut first_hold = reserve_single(&first, FamilyRole::Video, demand(1, 1, 8), None)
        .expect("first process root");
    assert!(matches!(
        reserve_single(&first, FamilyRole::Audio, demand(1, 1, 8), None),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    let snapshot = first.snapshot().expect("task claim snapshot");
    assert_eq!(snapshot.unknown_task_peak_producers, 1);
    assert_eq!(
        snapshot.managed_kernel_task_peak,
        Metric::Unknown(UnknownReason::Unbounded)
    );
    drop(first_hold.pop());
    assert_eq!(
        first
            .snapshot()
            .expect("no-spawn reservation releases its derived task claim")
            .unknown_task_peak_producers,
        0
    );

    let second = ResourceLedger::with_capacity(1, cap()).expect("ledger");
    let mut second_hold = reserve_single(&second, FamilyRole::Audio, demand(1, 1, 8), None)
        .expect("first process root");
    assert!(matches!(
        reserve_single(&second, FamilyRole::Video, demand(1, 1, 8), None),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(second_hold.pop());
}

#[test]
fn zero_kernel_task_headroom_refuses_a_new_process_root() {
    let mut cap = capacity(Metric::Known(100), Metric::Known(100));
    cap.kernel_tasks = Metric::Known(0);
    let ledger = ResourceLedger::with_capacity(1, cap).expect("ledger");
    assert!(matches!(
        reserve_single(&ledger, FamilyRole::Video, demand(1, 1, 8), None),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::KernelTasks))
    ));
    assert_eq!(ledger.snapshot().expect("empty after refusal").held, 0);
}

#[test]
fn unknown_kernel_task_batch_is_rejected_atomically() {
    let ledger = ledger(Metric::Known(100), Metric::Known(100));
    let (video_family, video_roles) = one_role(&ledger, FamilyRole::Video, demand(1, 1, 8));
    let (audio_family, audio_roles) = one_role(&ledger, FamilyRole::Audio, demand(1, 1, 8));
    assert!(matches!(
        ledger.reserve_families(&[
            FamilyDemand {
                identity: video_family,
                roles: &video_roles,
            },
            FamilyDemand {
                identity: audio_family,
                roles: &audio_roles,
            },
        ]),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    let snapshot = ledger.snapshot().expect("no partial task admission");
    assert_eq!(snapshot.held, 0);
    assert_eq!(snapshot.used, ResourceVector::default());
    assert_eq!(snapshot.unknown_task_peak_producers, 0);
}

#[test]
fn stale_observed_headroom_refuses_without_erasing_existing_hold() {
    let ledger = ledger(Metric::Known(100), Metric::Known(100));
    let mut held =
        reserve_single(&ledger, FamilyRole::Video, demand(1, 1, 8), None).expect("original hold");
    let before = ledger.snapshot().unwrap().used;
    let old = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        ledger.observe_capacity(capacity(Metric::Known(100), Metric::Known(100)), old),
        Ok(1)
    );
    assert!(matches!(
        reserve_single(&ledger, FamilyRole::Audio, demand(1, 1, 8), None),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    let snapshot = ledger.snapshot().unwrap();
    assert_eq!(snapshot.used, before);
    assert_eq!(snapshot.held, 1);
    drop(held.pop());
}

#[test]
fn older_concurrent_sample_cannot_replace_newer_lower_headroom() {
    let ledger = ledger(Metric::Known(100), Metric::Known(100));
    let now = std::time::Instant::now();
    let old = now
        .checked_sub(std::time::Duration::from_millis(10))
        .unwrap();
    ledger
        .observe_capacity(capacity(Metric::Known(1), Metric::Known(100)), now)
        .unwrap();
    ledger
        .observe_capacity(capacity(Metric::Known(100), Metric::Known(100)), old)
        .unwrap();
    assert!(matches!(
        reserve_single(&ledger, FamilyRole::Video, demand(1, 2, 8), None),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::Cpu))
    ));
    assert_eq!(ledger.snapshot().unwrap().held, 0);
}

fn limits(memory: u64, tasks: u32) -> crate::process::containment::NativeDomainLimits {
    crate::process::containment::NativeDomainLimits::new(memory, tasks)
        .expect("finite fixture limits")
}

/// A leaf inside a delegated parent large enough that only node capacity
/// decides; the parent fit has its own tests.
fn contained(memory: u64, tasks: u32) -> ContainedProducerBound {
    ContainedProducerBound::new(limits(memory, tasks), limits(1000, 1000))
}

fn reserve_contained(
    ledger: &ResourceLedger,
    demand: WorkloadDemand,
    bound: ContainedProducerBound,
) -> Result<Vec<(ResourceReservation, CleanupAuthority)>, AdmissionRefusal> {
    let (identity, roles) = one_role(ledger, FamilyRole::Video, demand);
    ledger.reserve_families_claiming_contained(
        &[FamilyDemand {
            identity,
            roles: &roles,
        }],
        Some(&[MemoryClaim::UnknownProducerPeak]),
        demand.trial,
        Some(bound),
    )
}

fn trial_demand(epoch: u64, cpu: u64, memory: u64) -> WorkloadDemand {
    WorkloadDemand {
        trial: true,
        ..demand(epoch, cpu, memory)
    }
}

#[test]
fn contained_producers_coexist_within_known_task_headroom() {
    let ledger = ledger(Metric::Known(100), Metric::Known(100));
    let mut first =
        reserve_contained(&ledger, demand(1, 1, 8), contained(4, 40)).expect("first contained");
    let mut second =
        reserve_contained(&ledger, demand(1, 1, 8), contained(4, 40)).expect("second contained");
    let snapshot = ledger.snapshot().expect("known claims");
    assert_eq!(snapshot.held, 2);
    assert_eq!(snapshot.unknown_peak_producers, 0);
    assert_eq!(snapshot.unknown_task_peak_producers, 0);
    assert_eq!(snapshot.active_bounded_producers, 2);
    assert_eq!(snapshot.managed_kernel_task_peak, Metric::Known(80));
    assert_eq!(snapshot.managed_memory_peak_bytes, Metric::Known(16));
    drop(second.pop());
    drop(first.pop());
    assert_eq!(ledger.snapshot().unwrap().held, 0);
}

#[test]
fn summed_contained_task_ceilings_over_headroom_refuse_atomically() {
    let ledger = ledger(Metric::Known(100), Metric::Known(100));
    let mut first =
        reserve_contained(&ledger, demand(1, 1, 8), contained(4, 60)).expect("first contained");
    assert!(matches!(
        reserve_contained(&ledger, demand(1, 1, 8), contained(4, 60)),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::KernelTasks))
    ));
    let snapshot = ledger.snapshot().expect("only the first remains");
    assert_eq!(snapshot.held, 1);
    assert_eq!(snapshot.used, vector(1, 8));
    assert_eq!(snapshot.managed_kernel_task_peak, Metric::Known(60));
    drop(first.pop());
}

#[test]
fn unknown_task_peak_still_serializes_against_contained_producers_in_both_orders() {
    let unknown_first = ledger(Metric::Known(100), Metric::Known(100));
    let mut held = reserve_single(
        &unknown_first,
        FamilyRole::Video,
        demand(1, 1, 8),
        Some(MemoryClaim::UnknownProducerPeak),
    )
    .expect("uncontained unknown producer");
    assert!(matches!(
        reserve_contained(&unknown_first, demand(1, 1, 8), contained(4, 4)),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(held.pop());

    // A default bounded-memory process root still has an unknown task peak.
    let bounded_root_first = ledger(Metric::Known(100), Metric::Known(100));
    let mut held = reserve_single(
        &bounded_root_first,
        FamilyRole::Audio,
        demand(1, 1, 8),
        None,
    )
    .expect("uncontained process root");
    assert!(matches!(
        reserve_contained(&bounded_root_first, demand(1, 1, 8), contained(4, 4)),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(held.pop());

    let contained_first = ledger(Metric::Known(100), Metric::Known(100));
    let mut held = reserve_contained(&contained_first, demand(1, 1, 8), contained(4, 4))
        .expect("contained producer");
    for claim in [None, Some(MemoryClaim::UnknownProducerPeak)] {
        assert!(matches!(
            reserve_single(&contained_first, FamilyRole::Audio, demand(1, 1, 8), claim),
            Err(AdmissionRefusal::InsufficientEvidence)
        ));
    }
    assert_eq!(contained_first.snapshot().unwrap().held, 1);
    drop(held.pop());
}

#[test]
fn contained_bound_without_known_capacity_keeps_the_serialized_unknown_claims() {
    let ledger = ledger(
        Metric::Known(100),
        Metric::Unknown(UnknownReason::InsufficientEvidence),
    );
    let mut held = reserve_contained(&ledger, demand(1, 1, 8), contained(4, 4))
        .expect("serialized cold producer");
    let snapshot = ledger.snapshot().expect("unknown claims");
    assert_eq!(snapshot.unknown_peak_producers, 1);
    assert_eq!(snapshot.unknown_task_peak_producers, 1);
    assert!(matches!(
        reserve_contained(&ledger, demand(1, 1, 8), contained(4, 4)),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(held.pop());
}

#[test]
fn contained_bound_must_describe_one_paid_process_root() {
    let ledger = ledger(Metric::Known(100), Metric::Known(100));
    // The demand must already pay the kernel memory ceiling.
    assert!(matches!(
        reserve_contained(&ledger, demand(1, 1, 8), contained(9, 4)),
        Err(AdmissionRefusal::InvalidBundle)
    ));
    let (identity, roles) = one_role(&ledger, FamilyRole::Video, demand(1, 1, 8));
    assert!(matches!(
        ledger.reserve_families_claiming_contained(
            &[FamilyDemand {
                identity,
                roles: &roles,
            }],
            None,
            false,
            Some(contained(4, 4)),
        ),
        Err(AdmissionRefusal::InvalidBundle)
    ));
    assert_eq!(ledger.snapshot().unwrap().held, 0);
}

#[test]
fn contained_trial_is_admitted_beside_a_contained_incumbent_only() {
    let ledger = ledger(Metric::Known(100), Metric::Known(100));
    let mut incumbent =
        reserve_contained(&ledger, demand(1, 1, 8), contained(4, 40)).expect("contained incumbent");
    assert_eq!(
        ledger.contained_incumbents(Some(contained(4, 40))),
        Ok(true)
    );
    let mut trial = reserve_contained(&ledger, trial_demand(1, 1, 8), contained(4, 40))
        .expect("contained trial beside the incumbent");
    let snapshot = ledger.snapshot().expect("both held");
    assert!(snapshot.trial_held);
    assert_eq!(snapshot.held, 2);
    // Trial slot uniqueness is unchanged.
    assert!(matches!(
        reserve_contained(&ledger, trial_demand(1, 1, 8), contained(4, 10)),
        Err(AdmissionRefusal::TrialAlreadyHeld)
    ));
    drop(trial.pop());
    drop(incumbent.pop());

    let ledger = self::ledger(Metric::Known(100), Metric::Known(100));
    let mut incumbent = reserve_single(
        &ledger,
        FamilyRole::Audio,
        demand(1, 1, 8),
        Some(MemoryClaim::UnknownProducerPeak),
    )
    .expect("unknown incumbent");
    assert_eq!(
        ledger.contained_incumbents(Some(contained(4, 4))),
        Err(AdmissionRefusal::InsufficientEvidence)
    );
    assert!(matches!(
        reserve_contained(&ledger, trial_demand(1, 1, 8), contained(4, 4)),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    assert!(!ledger.snapshot().unwrap().trial_held);
    drop(incumbent.pop());
    assert_eq!(ledger.contained_incumbents(None), Ok(false));
}

#[test]
fn contained_trial_with_unknown_capacity_still_requires_no_active_producer() {
    let ledger = ledger(
        Metric::Known(100),
        Metric::Unknown(UnknownReason::InsufficientEvidence),
    );
    let mut incumbent =
        reserve_contained(&ledger, demand(1, 1, 8), contained(4, 4)).expect("serialized incumbent");
    assert!(matches!(
        reserve_contained(&ledger, trial_demand(1, 1, 8), contained(4, 4)),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(incumbent.pop());
    let mut trial = reserve_contained(&ledger, trial_demand(1, 1, 8), contained(4, 4))
        .expect("serialized trial alone");
    assert!(ledger.snapshot().unwrap().trial_held);
    drop(trial.pop());
}

#[test]
fn contained_siblings_must_fit_their_delegated_parent_together() {
    // Leaves sized like the parent itself can starve or OOM each other.
    let ledger = self::ledger(Metric::Known(100), Metric::Known(100));
    let whole = ContainedProducerBound::new(limits(4, 40), limits(4, 40));
    let mut incumbent =
        reserve_contained(&ledger, demand(1, 1, 8), whole).expect("one leaf fills its parent");
    assert_eq!(
        ledger.contained_incumbents(Some(whole)),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::Memory))
    );
    assert!(matches!(
        reserve_contained(&ledger, trial_demand(1, 1, 8), whole),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::Memory))
    ));
    assert!(!ledger.snapshot().unwrap().trial_held);
    drop(incumbent.pop());

    // Half-parent leaves coexist; a third does not.
    let half = ContainedProducerBound::new(limits(2, 20), limits(4, 40));
    let mut first = reserve_contained(&ledger, demand(1, 1, 8), half).expect("first half");
    assert_eq!(ledger.contained_incumbents(Some(half)), Ok(true));
    let mut second =
        reserve_contained(&ledger, trial_demand(1, 1, 8), half).expect("trial in the second half");
    assert!(matches!(
        reserve_contained(&ledger, demand(1, 1, 8), half),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::Memory))
    ));
    assert_eq!(ledger.snapshot().unwrap().held, 2);
    drop(second.pop());
    drop(first.pop());

    // The parent's pids.max is checked independently of node headroom.
    let tasks = ContainedProducerBound::new(limits(1, 30), limits(100, 40));
    let mut first = reserve_contained(&ledger, demand(1, 1, 8), tasks).expect("first leaf");
    assert!(matches!(
        reserve_contained(&ledger, demand(1, 1, 8), tasks),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::KernelTasks))
    ));
    // A leaf of another delegated parent is not comparable evidence.
    let foreign = ContainedProducerBound::new(limits(1, 1), limits(100, 100));
    assert!(matches!(
        reserve_contained(&ledger, demand(1, 1, 8), foreign),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    drop(first.pop());
    assert_eq!(ledger.snapshot().unwrap().held, 0);
}
