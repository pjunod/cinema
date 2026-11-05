use super::*;
use std::sync::atomic::AtomicUsize;

fn cap() -> ResourceCapacity {
    Metric::Known(ResourceVector {
        cpu_ns_per_second: 1_000_000,
        peak_memory_bytes: 1_000_000,
        handles: 100,
        processes: 100,
        hardware_slots: 100,
        tuner_slots: 100,
        scratch_bytes: 1_000_000,
    })
    .into()
}
fn ledger() -> ResourceLedger {
    ResourceLedger::with_capacity(1, cap()).expect("ledger")
}
fn fresh(ledger: &ResourceLedger) {
    ledger
        .observe_capacity(cap(), Instant::now())
        .expect("actual fixture observation");
}
fn install(ledger: &ResourceLedger) -> Result<SupportIdentity, AdmissionRefusal> {
    ledger
        .reserve_finalization_support(
            FinalizationSupportProfile::fixture(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            1,
            |_| Ok(()),
        )
        .map(|(id, ())| id)
}
fn demand(trial: bool) -> WorkloadDemand {
    WorkloadDemand {
        resources: Metric::Known(ResourceVector {
            peak_memory_bytes: 1,
            ..ResourceVector::default()
        }),
        limit_epoch: 1,
        trial,
    }
}

#[test]
fn support_requires_actual_fresh_nonfuture_known_ram_at_unchanged_epoch() {
    let ledger = ledger();
    let calls = AtomicUsize::new(0);
    for observation in [
        None,
        Some(Instant::now() - Duration::from_secs(2)),
        Some(Instant::now() + Duration::from_secs(2)),
    ] {
        ledger.inner.lock().expect("state").capacity_observed_at = observation;
        assert_eq!(
            ledger
                .reserve_finalization_support(
                    FinalizationSupportProfile::fixture(),
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    1,
                    |_| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    }
                )
                .err(),
            Some(AdmissionRefusal::InsufficientEvidence)
        );
        assert_eq!(
            ledger.snapshot().expect("snapshot").used,
            ResourceVector::default()
        );
    }
    {
        let mut state = ledger.inner.lock().expect("state");
        state.capacity_observed_at = None;
    }
    let mut unknown_ram = cap();
    unknown_ram.memory_bytes = Metric::Unknown(UnknownReason::InsufficientEvidence);
    ledger
        .observe_capacity(unknown_ram, Instant::now())
        .expect("same epoch observation");
    assert_eq!(
        install(&ledger),
        Err(AdmissionRefusal::InsufficientEvidence)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    fresh(&ledger);
    assert_eq!(ledger.snapshot().expect("snapshot").limit_epoch, 1);
    install(&ledger).expect("fresh known RAM now permits support");
}

#[test]
fn support_installation_checks_zero_root_producers_trial_and_all_preflight_failures() {
    let ledger = ledger();
    fresh(&ledger);
    let (first, _) = ledger
        .reserve(demand(false))
        .expect("first zero-root producer");
    let (second, _) = ledger
        .reserve(demand(false))
        .expect("bounded second producer with no support");
    assert_eq!(
        install(&ledger),
        Err(AdmissionRefusal::InsufficientEvidence)
    );
    assert!(ledger
        .snapshot()
        .expect("snapshot")
        .finalization_support
        .is_none());
    drop((first, second));
    let (trial, _) = ledger.reserve(demand(true)).expect("trial before support");
    assert_eq!(install(&ledger), Err(AdmissionRefusal::TrialAlreadyHeld));
    drop(trial);
    for failure in 0..5 {
        let mut state = ledger.inner.lock().expect("state");
        state.invalid = failure == 0;
        state.draining = failure == 1;
        state.native_overage_fenced = failure == 2;
        state.next_support_generation = if failure == 3 { u64::MAX } else { 1 };
        state.used.peak_memory_bytes = if failure == 4 { u64::MAX } else { 0 };
        drop(state);
        assert!(install(&ledger).is_err());
        assert!(ledger
            .snapshot()
            .expect("snapshot")
            .finalization_support
            .is_none());
    }
    let mut state = ledger.inner.lock().expect("state");
    state.used = ResourceVector::default();
    state.cap.memory_bytes = Metric::Known(1);
    drop(state);
    assert_eq!(
        install(&ledger),
        Err(AdmissionRefusal::HardLimit(ResourceDimension::Memory))
    );
}

#[test]
fn support_is_additive_unknown_without_fake_producers_roots_or_trial_authority() {
    let ledger = ledger();
    fresh(&ledger);
    let identity = install(&ledger).expect("support");
    let baseline = ledger.snapshot().expect("support snapshot");
    assert_eq!(baseline.used.peak_memory_bytes, 64 * 1024);
    assert_eq!(baseline.held, 0);
    assert_eq!(baseline.unknown_peak_producers, 0);
    assert_eq!(baseline.unknown_task_peak_producers, 0);
    assert_eq!(baseline.used.processes, 0);
    assert!(matches!(
        baseline.managed_memory_peak_bytes,
        Metric::Unknown(_)
    ));
    assert!(matches!(
        baseline.managed_kernel_task_peak,
        Metric::Unknown(_)
    ));
    let (first, _) = ledger
        .reserve(demand(false))
        .expect("one conservative producer");
    assert_eq!(
        ledger.assess(demand(false)).expect("assessment").refusal,
        Some(AdmissionRefusal::InsufficientEvidence)
    );
    assert_eq!(
        ledger.reserve(demand(false)).err(),
        Some(AdmissionRefusal::InsufficientEvidence)
    );
    assert_eq!(
        ledger.reserve(demand(true)).err(),
        Some(AdmissionRefusal::InsufficientEvidence)
    );
    assert_eq!(install(&ledger), Err(AdmissionRefusal::InvalidTransition));
    let mut foreign = identity;
    foreign.owner = Uuid::new_v4();
    assert_eq!(
        ledger.support_transition(
            foreign,
            FinalizationSupportPhase::ReservedUnallocated,
            FinalizationSupportPhase::Closing,
            || panic!("foreign identity must not reach census")
        ),
        Err(AdmissionRefusal::ReceiptMismatch)
    );
    drop(first);
    let after = ledger
        .snapshot()
        .expect("native retirement preserves support");
    assert_eq!(after.used, baseline.used);
    assert_eq!(after.held, 0);
    ledger.update_capacity(2, cap()).expect("capacity update");
    assert_eq!(
        ledger.snapshot().expect("support retained").used,
        baseline.used
    );
}
