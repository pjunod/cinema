use super::*;
use crate::adaptive_resources::PhysicalRetirementReceipt;
use crate::adaptive_resources::{
    FamilyDemand, FamilyRole, FamilyRoleDemand, MemoryClaim, ReservationBinding, ReservationState,
    ResourceLedger, ResourceReservation, ResourceVector, WorkloadDemand,
};
use crate::process::observation::Metric;
use crate::process::observation::ProcessIdentity;
use crate::resource_custody::CommittedRetirementReceipt;
use uuid::Uuid;

fn baseline(at: Instant, pid: u32, start_marker: u64, cpu_ns: u64, epoch: u64) -> Baseline {
    Baseline {
        at,
        counters: ProcessCounters {
            identity: ProcessIdentity { pid, start_marker },
            cpu_ns,
            resident_bytes: Metric::Known(4096),
        },
        control_epoch: epoch,
    }
}

#[test]
fn stale_and_cached_observations_cannot_form_a_cost_window() {
    let start = Instant::now();
    let before = baseline(start, 7, 11, 100, 2);
    assert_eq!(
        classify_window(before, baseline(start, 7, 11, 200, 2), start),
        WindowDecision::Stale
    );
    let stale = baseline(start + Duration::from_secs(1), 7, 11, 200, 2);
    assert_eq!(
        classify_window(before, stale, start + MAX_WINDOW + Duration::from_secs(2)),
        WindowDecision::Stale
    );
}

#[test]
fn identity_control_and_regressed_counters_rebase_without_a_sample() {
    let start = Instant::now();
    let before = baseline(start, 7, 11, 100, 2);
    let after = start + Duration::from_secs(2);
    assert_eq!(
        classify_window(before, baseline(after, 8, 12, 300, 2), after),
        WindowDecision::Rebase
    );
    assert_eq!(
        classify_window(before, baseline(after, 7, 11, 300, 3), after),
        WindowDecision::Rebase
    );
    assert_eq!(
        classify_window(before, baseline(after, 7, 11, 99, 2), after),
        WindowDecision::Rebase
    );
    assert_eq!(
        classify_window(before, baseline(after, 7, 11, 300, 2), after + MAX_WINDOW),
        WindowDecision::Record(Duration::from_secs(2))
    );
    let long_gap = start + MAX_WINDOW + Duration::from_secs(1);
    assert_eq!(
        classify_window(before, baseline(long_gap, 7, 11, 300, 2), long_gap),
        WindowDecision::Rebase
    );
}

#[test]
fn only_fresh_monotonic_windows_accumulate_or_record() {
    let start = Instant::now();
    let before = baseline(start, 7, 11, 100, 2);
    assert_eq!(
        classify_window(
            before,
            baseline(start + MIN_WINDOW / 2, 7, 11, 200, 2),
            start + MIN_WINDOW / 2,
        ),
        WindowDecision::Accumulate
    );
    assert!(matches!(
        classify_window(
            before,
            baseline(start + MIN_WINDOW, 7, 11, 200, 2),
            start + MIN_WINDOW,
        ),
        WindowDecision::Record(_)
    ));
}

fn resources(memory: u64, processes: u64) -> ResourceVector {
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
        resources: Metric::Known(resources),
        limit_epoch: 1,
        trial: false,
    }
}

struct LiveMixFixture {
    ledger: ResourceLedger,
    family: crate::adaptive_resources::FamilyIdentity,
    selected: ResourceReservation,
    retained_binding: ReservationBinding,
    retained_durable: Uuid,
}

fn live_mix_fixture() -> LiveMixFixture {
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
    let mut source = ledger
        .reserve_roles_claiming(
            &[(None, demand(resources(256, 1)), None)],
            Some(&[MemoryClaim::UnknownProducerPeak]),
        )
        .expect("producer reservation");
    let (source, _cleanup) = source.pop().expect("producer role");
    let source_binding = source.custody_binding();
    let source_durable = Uuid::new_v4();
    {
        let mut state = ledger.inner.lock().expect("ledger state");
        let entry = state
            .entries
            .get_mut(&source_binding.reservation)
            .expect("producer entry");
        entry.durable = Some(source_durable);
        entry.state = ReservationState::Reaping;
    }
    let retained_durable = Uuid::new_v4();
    let mut swap = ledger
        .prepare_retained_swap(
            source_binding,
            source_durable,
            retained_durable,
            resources(256, 0),
        )
        .expect("retained S replacement");
    let retained_binding = swap.target_binding();
    let receipt = PhysicalRetirementReceipt::adapter_fixture(source_binding, retained_binding);
    let committed = CommittedRetirementReceipt::adapter_fixture(receipt);
    let _authority = ledger
        .commit_retained_swap(&mut swap, &committed)
        .expect("accounting-only retained S fixture");
    drop(source);

    let family = ledger.new_family().expect("selected family");
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Video,
        demand: demand(resources(128, 1)),
        retained: None,
    }];
    let families = [FamilyDemand {
        identity: family,
        roles: &roles,
    }];
    let mut selected = ledger
        .reserve_families_claiming(&families, Some(&[MemoryClaim::UnknownProducerPeak]))
        .expect("selected serialized producer");
    let (selected, _cleanup) = selected.pop().expect("selected role");
    LiveMixFixture {
        ledger,
        family,
        selected,
        retained_binding,
        retained_durable,
    }
}

#[test]
fn actual_retained_mix_change_rekeys_and_rebases_window() {
    let actual = live_mix_fixture();
    let basis = HistoryBasis {
        runtime_epoch: [1; 32],
        workload: [2; 32],
        recipe: [3; 32],
    };
    let mut key = None;
    assert_eq!(
        refresh_history_key(
            &mut key,
            Some(basis),
            Some(&actual.ledger),
            Some(actual.family),
        ),
        MixRefresh::Changed
    );
    let old_key = key.expect("first live mixture key");
    let old_baseline = baseline(Instant::now(), 7, 11, 100, 2);
    let mut window = Some(old_baseline);
    let mut units = 3;
    let mut recorded_windows = 2;

    let next_durable = Uuid::new_v4();
    let mut swap = actual
        .ledger
        .prepare_retained_swap(
            actual.retained_binding,
            actual.retained_durable,
            next_durable,
            resources(512, 0),
        )
        .expect("prepare changed retained S");
    let receipt =
        PhysicalRetirementReceipt::adapter_fixture(actual.retained_binding, swap.target_binding());
    let committed = CommittedRetirementReceipt::adapter_fixture(receipt);
    let _authority = actual
        .ledger
        .commit_retained_swap(&mut swap, &committed)
        .expect("accounting-only retained replacement");

    let refresh = refresh_history_key(
        &mut key,
        Some(basis),
        Some(&actual.ledger),
        Some(actual.family),
    );
    assert_eq!(refresh, MixRefresh::Changed);
    let current = baseline(Instant::now() + Duration::from_secs(1), 7, 11, 200, 2);
    rebase_for_new_mix(&mut window, &mut units, &mut recorded_windows, current);
    assert_eq!(window.map(|sample| sample.at), Some(current.at));
    assert_eq!(units, 0);
    assert_eq!(recorded_windows, 0);
    let new_key = key.expect("rebound live mixture key");
    assert_ne!(old_key.concurrent_mix, new_key.concurrent_mix);
    assert_eq!(new_key.phase, CostPhase::Startup);
    assert_eq!(
        refresh_history_key(
            &mut key,
            Some(basis),
            Some(&actual.ledger),
            Some(actual.family),
        ),
        MixRefresh::Unchanged
    );
    let _keep_selected = actual.selected;
}
