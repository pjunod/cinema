use super::*;
use crate::adaptive_resources::ResourceReservation;
use crate::process::observation::Metric;

fn source_fixture(
    trial: bool,
) -> (
    ResourceLedger,
    ResourceReservation,
    ReservationBinding,
    Uuid,
    ResourceVector,
) {
    source_fixture_with_claim(trial, None)
}

fn source_fixture_with_claim(
    trial: bool,
    claim: Option<MemoryClaim>,
) -> (
    ResourceLedger,
    ResourceReservation,
    ReservationBinding,
    Uuid,
    ResourceVector,
) {
    let resources = ResourceVector {
        cpu_ns_per_second: 90,
        peak_memory_bytes: 512,
        handles: 12,
        processes: 2,
        hardware_slots: 1,
        tuner_slots: 0,
        scratch_bytes: 800,
    };
    let ledger = ResourceLedger::new(
        7,
        Metric::Known(ResourceVector {
            cpu_ns_per_second: 1000,
            peak_memory_bytes: 1000,
            handles: 100,
            processes: 20,
            hardware_slots: 10,
            tuner_slots: 10,
            scratch_bytes: 2000,
        }),
    )
    .expect("ledger fixture");
    let demand = crate::adaptive_resources::WorkloadDemand {
        resources: Metric::Known(resources),
        limit_epoch: 7,
        trial,
    };
    let roles = [(None, demand, None)];
    let (reservation, _) = match claim {
        Some(claim) => ledger
            .reserve_roles_claiming(&roles, Some(&[claim]))
            .expect("claimed source reservation"),
        None => ledger
            .reserve_roles_claiming(&roles, None)
            .expect("source reservation"),
    }
    .pop()
    .expect("single source reservation");
    let binding = reservation.custody_binding();
    let durable = Uuid::new_v4();
    {
        let mut state = ledger.inner.lock().expect("ledger state");
        let entry = state
            .entries
            .get_mut(&binding.reservation)
            .expect("source entry");
        entry.durable = Some(durable);
        entry.state = ReservationState::Reaping;
    }
    (ledger, reservation, binding, durable, resources)
}

#[test]
fn known_commit_swap_releases_unknown_claim_but_preserves_resident_memory() {
    let (ledger, reservation, source, durable, resources) =
        source_fixture_with_claim(false, Some(MemoryClaim::UnknownProducerPeak));
    let before_swap = reserve_roles_for_claim(
        &ledger,
        MemoryClaim::UnknownProducerPeak,
        ResourceVector {
            cpu_ns_per_second: 10,
            peak_memory_bytes: 128,
            handles: 2,
            processes: 1,
            scratch_bytes: 100,
            ..ResourceVector::default()
        },
    );
    assert!(matches!(
        before_swap,
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    let snapshot = ledger.snapshot().expect("unknown source charge");
    assert_eq!(snapshot.used, resources);
    assert_eq!(snapshot.unknown_peak_producers, 1);

    let target_durable = Uuid::new_v4();
    let retained = ResourceVector {
        cpu_ns_per_second: 0,
        peak_memory_bytes: resources.peak_memory_bytes,
        handles: resources.handles,
        processes: 0,
        hardware_slots: 0,
        tuner_slots: 0,
        scratch_bytes: resources.scratch_bytes,
    };
    let mut swap = ledger
        .prepare_retained_swap(source, durable, target_durable, retained)
        .expect("prepare exact resident charge");
    // Accounting-only adapter fixture: this exercises claim transfer after a
    // known journal result, not physical retirement acceptance.
    let proof = receipt(source, swap.target_binding());
    let _authority = ledger
        .commit_retained_swap(&mut swap, &proof)
        .expect("accounting fixture known commit");
    drop(reservation);

    let snapshot = ledger.snapshot().expect("retained S charge");
    assert_eq!(
        snapshot.used,
        ResourceVector {
            peak_memory_bytes: resources.peak_memory_bytes,
            handles: resources.handles,
            scratch_bytes: resources.scratch_bytes,
            ..ResourceVector::default()
        }
    );
    assert_eq!(snapshot.held, 1);
    assert_eq!(snapshot.unknown_peak_producers, 0);
    {
        let state = ledger.inner.lock().expect("ledger state");
        let target = state
            .entries
            .get(&swap.target_binding().reservation)
            .expect("original S resident entry");
        assert_eq!(target.resources, retained);
        assert_eq!(
            target.memory_claim,
            MemoryClaim::KnownBounded {
                active_producer: false
            }
        );
    }
    let added = reserve_roles_for_claim(
        &ledger,
        MemoryClaim::UnknownProducerPeak,
        ResourceVector {
            cpu_ns_per_second: 10,
            peak_memory_bytes: 128,
            handles: 2,
            processes: 1,
            scratch_bytes: 100,
            ..ResourceVector::default()
        },
    )
    .expect("unknown producer fits beside resident S");
    let after = ledger.snapshot().expect("S and new producer");
    assert_eq!(
        after.used.peak_memory_bytes,
        resources.peak_memory_bytes + 128
    );
    assert_eq!(after.held, 2);
    assert_eq!(after.unknown_peak_producers, 1);
    drop(added);
}

fn reserve_roles_for_claim(
    ledger: &ResourceLedger,
    claim: MemoryClaim,
    resources: ResourceVector,
) -> Result<Vec<(ResourceReservation, CleanupAuthority)>, AdmissionRefusal> {
    let demand = crate::adaptive_resources::WorkloadDemand {
        resources: Metric::Known(resources),
        limit_epoch: 7,
        trial: false,
    };
    ledger.reserve_roles_claiming(&[(None, demand, None)], Some(&[claim]))
}

fn retained_from(source: ResourceVector) -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: 0,
        peak_memory_bytes: source.peak_memory_bytes / 2,
        handles: source.handles / 2,
        processes: 0,
        hardware_slots: 0,
        tuner_slots: 0,
        scratch_bytes: source.scratch_bytes / 2,
    }
}

fn receipt(source: ReservationBinding, target: ReservationBinding) -> CommittedRetirementReceipt {
    CommittedRetirementReceipt::adapter_fixture(PhysicalRetirementReceipt { source, target })
}

#[test]
fn preparation_keeps_the_full_durable_source_charge() {
    let (ledger, _reservation, source, durable, resources) = source_fixture(true);
    let before = ledger.snapshot().expect("before snapshot");
    let retained = retained_from(resources);
    let swap = ledger
        .prepare_retained_swap(source, durable, Uuid::new_v4(), retained)
        .expect("prepare swap");
    let after = ledger.snapshot().expect("after snapshot");
    assert_eq!(after.used, resources);
    assert_eq!(after.held, before.held);
    assert_eq!(after.trial_held, before.trial_held);
    assert_ne!(swap.target_binding(), source);
    assert!(!swap.target_durable().is_nil());
}

#[test]
fn commit_conserves_retained_charge_releases_cpu_and_preserves_trial() {
    let (ledger, _reservation, source, durable, resources) = source_fixture(true);
    let target_durable = Uuid::new_v4();
    let retained = retained_from(resources);
    let mut swap = ledger
        .prepare_retained_swap(source, durable, target_durable, retained)
        .expect("prepare swap");
    let proof = receipt(source, swap.target_binding());
    let authority = ledger
        .commit_retained_swap(&mut swap, &proof)
        .expect("commit swap");
    let snapshot = ledger.snapshot().expect("committed snapshot");
    assert_eq!(snapshot.used, retained);
    assert_eq!(snapshot.held, 1);
    assert!(snapshot.trial_held);
    assert_eq!(authority.binding, swap.target_binding());
    let state = ledger.inner.lock().expect("ledger state");
    let target = state
        .entries
        .get(&swap.target_binding().reservation)
        .expect("retained target");
    assert_eq!(target.durable, Some(target_durable));
    assert_eq!(target.resources, retained);
    assert_eq!(target.state, ReservationState::Reaping);
    assert!(!state.entries.contains_key(&source.reservation));
    assert!(state.trial.as_ref().is_some_and(|members| {
        members.contains(&swap.target_binding().reservation)
            && !members.contains(&source.reservation)
    }));
}

#[test]
fn mismatched_receipt_and_changed_source_leave_source_charge_intact() {
    let (ledger, _reservation, source, durable, resources) = source_fixture(false);
    let retained = retained_from(resources);
    let mut swap = ledger
        .prepare_retained_swap(source, durable, Uuid::new_v4(), retained)
        .expect("prepare swap");
    let wrong = receipt(
        source,
        ReservationBinding {
            reservation: swap.target_binding().reservation + 1,
            ..swap.target_binding()
        },
    );
    assert!(matches!(
        ledger.commit_retained_swap(&mut swap, &wrong),
        Err(AdmissionRefusal::ReceiptMismatch)
    ));
    {
        let mut state = ledger.inner.lock().expect("ledger state");
        let extra = ResourceVector {
            scratch_bytes: 1,
            ..ResourceVector::default()
        };
        state.used = state.used.checked_add(extra).expect("fixture charge");
        let entry = state.entries.get_mut(&source.reservation).expect("source");
        entry.resources = entry.resources.checked_add(extra).expect("fixture charge");
    }
    let correct = receipt(source, swap.target_binding());
    assert!(matches!(
        ledger.commit_retained_swap(&mut swap, &correct),
        Err(AdmissionRefusal::ReceiptMismatch)
    ));
    assert_eq!(
        ledger.snapshot().expect("unchanged source").used,
        ResourceVector {
            scratch_bytes: resources.scratch_bytes + 1,
            ..resources
        }
    );
    let state = ledger.inner.lock().expect("ledger state");
    assert!(state.entries.contains_key(&source.reservation));
    assert!(!state
        .entries
        .contains_key(&swap.target_binding().reservation));
}

#[test]
fn repeated_commit_returns_the_same_target_without_double_refund() {
    let (ledger, _reservation, source, durable, resources) = source_fixture(false);
    let target_durable = Uuid::new_v4();
    let retained = retained_from(resources);
    let mut swap = ledger
        .prepare_retained_swap(source, durable, target_durable, retained)
        .expect("prepare swap");
    let proof = receipt(source, swap.target_binding());
    let first = ledger
        .commit_retained_swap(&mut swap, &proof)
        .expect("first commit");
    let after_first = ledger.snapshot().expect("first snapshot");
    let second = ledger
        .commit_retained_swap(&mut swap, &proof)
        .expect("idempotent commit");
    let after_second = ledger.snapshot().expect("second snapshot");
    assert_eq!(first.binding, second.binding);
    assert_eq!(first.binding, swap.target_binding());
    assert_eq!(after_second.used, after_first.used);
    assert_eq!(after_second.held, after_first.held);
}
