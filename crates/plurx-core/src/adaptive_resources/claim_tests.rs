use super::*;
use crate::process::observation::{Metric, UnknownReason};

fn capacity(cpu: Metric<u64>, memory: Metric<u64>) -> ResourceCapacity {
    ResourceCapacity {
        cpu_ns_per_second: cpu,
        memory_bytes: memory,
        handles: Metric::Known(100),
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
