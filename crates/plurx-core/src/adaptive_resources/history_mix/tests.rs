use super::*;
use crate::resource_custody::CommittedRetirementReceipt;

struct Fixture {
    ledger: ResourceLedger,
    selected_family: FamilyIdentity,
    selected: ResourceReservation,
}

fn vector(memory: u64, processes: u64) -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: if processes == 0 { 0 } else { 10 },
        peak_memory_bytes: memory,
        handles: 4,
        processes,
        scratch_bytes: 32,
        ..ResourceVector::default()
    }
}

fn demand(resources: ResourceVector) -> WorkloadDemand {
    WorkloadDemand {
        resources: crate::process::observation::Metric::Known(resources),
        limit_epoch: 1,
        trial: false,
    }
}

fn fixture(retained_memory: u64) -> Fixture {
    use crate::adaptive_resources::MemoryClaim;
    use crate::process::observation::Metric;
    let ledger = ResourceLedger::new(
        1,
        Metric::Known(ResourceVector {
            cpu_ns_per_second: 10_000,
            peak_memory_bytes: 10_000,
            handles: 10_000,
            processes: 10_000,
            hardware_slots: 10_000,
            tuner_slots: 10_000,
            scratch_bytes: 10_000,
        }),
    )
    .expect("ledger");

    let source_resources = vector(retained_memory, 1);
    let mut source = ledger
        .reserve_roles_claiming(
            &[(None, demand(source_resources), None)],
            Some(&[MemoryClaim::UnknownProducerPeak]),
        )
        .expect("source reservation");
    let (source_reservation, _cleanup) = source.pop().expect("source role");
    let source_binding = source_reservation.custody_binding();
    let source_durable = Uuid::new_v4();
    {
        let mut state = ledger.inner.lock().expect("ledger state");
        let entry = state
            .entries
            .get_mut(&source_binding.reservation)
            .expect("source entry");
        entry.durable = Some(source_durable);
        entry.state = ReservationState::Reaping;
    }
    let retained = vector(retained_memory, 0);
    let mut swap = ledger
        .prepare_retained_swap(source_binding, source_durable, Uuid::new_v4(), retained)
        .expect("retained replacement");
    let receipt = crate::adaptive_resources::PhysicalRetirementReceipt::adapter_fixture(
        source_binding,
        swap.target_binding(),
    );
    let committed = CommittedRetirementReceipt::adapter_fixture(receipt);
    let _authority = ledger
        .commit_retained_swap(&mut swap, &committed)
        .expect("accounting-only retained swap fixture");
    drop(source_reservation);

    let selected_family = ledger.new_family().expect("selected family");
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Video,
        demand: demand(vector(128, 1)),
        retained: None,
    }];
    let families = [FamilyDemand {
        identity: selected_family,
        roles: &roles,
    }];
    let mut selected = ledger
        .reserve_families_claiming(&families, Some(&[MemoryClaim::UnknownProducerPeak]))
        .expect("selected serialized producer");
    let (selected, _cleanup) = selected.pop().expect("selected video role");
    Fixture {
        ledger,
        selected_family,
        selected,
    }
}

#[test]
fn actual_mix_changes_with_the_committed_retained_resource_vector() {
    let smaller = fixture(256);
    let larger = fixture(512);
    let small_mix = smaller
        .ledger
        .actual_history_mix_for(smaller.selected_family)
        .expect("one selected producer plus retained S");
    let large_mix = larger
        .ledger
        .actual_history_mix_for(larger.selected_family)
        .expect("one selected producer plus retained S");
    assert_ne!(small_mix, large_mix);
    let _keep_reservations_alive = (smaller.selected, larger.selected);
}

#[test]
fn actual_mix_refuses_another_active_role_and_unknown_held_state() {
    use crate::adaptive_resources::MemoryClaim;
    let active = fixture(256);
    let other_family = active.ledger.new_family().expect("other family");
    {
        // This intentionally injects an inconsistent active row after normal
        // admission to exercise the mix reader's fail-closed boundary. It is
        // not a reservation path or physical-closure fixture.
        let mut state = active.ledger.inner.lock().expect("ledger state");
        let reservation = state.next_reservation;
        state.next_reservation += 1;
        let binding = ReservationBinding {
            ledger: active.ledger.id,
            reservation,
            allocation_epoch: state.epoch,
        };
        let resources = vector(16, 1);
        state.used = state.used.checked_add(resources).expect("fixture totals");
        state.entries.insert(
            reservation,
            Entry {
                binding,
                family_role: Some((other_family, FamilyRole::Audio)),
                resources,
                memory_claim: MemoryClaim::KnownBounded {
                    active_producer: true,
                },
                state: ReservationState::Reserved,
                custody: None,
                durable: None,
            },
        );
    }
    assert_eq!(
        active.ledger.actual_history_mix_for(active.selected_family),
        Err(AdmissionRefusal::InsufficientEvidence)
    );

    let unknown = fixture(256);
    {
        let mut state = unknown.ledger.inner.lock().expect("ledger state");
        let entry = state
            .entries
            .get_mut(&unknown.selected.binding.reservation)
            .expect("selected entry");
        entry.state = ReservationState::UnknownHeld;
    }
    assert_eq!(
        unknown
            .ledger
            .actual_history_mix_for(unknown.selected_family),
        Err(AdmissionRefusal::InsufficientEvidence)
    );
}
