use super::*;

fn class(index: u8) -> Class {
    ([index; 32], [index; 32])
}

fn family(index: u64) -> FamilyIdentity {
    FamilyIdentity {
        ledger: 1,
        family: index,
    }
}

fn sample(base: Instant, second: u64, generation: u64, rate: u64, phase: WorkPhase) -> WorkSample {
    WorkSample {
        generation,
        phase,
        unit: ServiceUnit::MediaMilliseconds,
        completed: second * rate,
        deadline_slack_ms: Metric::Unknown(UnknownReason::InsufficientEvidence),
        queued_bytes: Metric::Unknown(UnknownReason::InsufficientEvidence),
        at: base + Duration::from_secs(second),
    }
}

#[test]
fn finite_trial_adapter_keeps_original_preparation_cutoff_and_unknown_bounds() {
    let now = Instant::now();
    let cutoff = now + Duration::from_secs(30);
    let budget = finite_trial_budget(MemoryClaim::UnknownProducerPeak, cutoff);
    assert_eq!(budget.original_deadline, cutoff);
    assert_eq!(
        budget.activation_allowance,
        Duration::ZERO,
        "activation was reserved by create before this original cutoff"
    );
    assert_eq!(budget.allocate(now), Err(TrialRefusal::UnknownMemory));
    // A bounded memory component alone cannot certify startup/task cleanup.
    assert_eq!(
        finite_trial_budget(
            MemoryClaim::KnownBounded {
                active_producer: true
            },
            cutoff
        )
        .allocate(now),
        Err(TrialRefusal::UnknownBounds)
    );
}

#[tokio::test]
async fn finite_trial_refusal_uses_same_live_continuous_cutoff_and_refuses_missing_moved_or_expired_deadline(
) {
    let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/finite-policy-fixtures");
    std::fs::create_dir_all(&parent).expect("protected policy fixture parent");
    let parent = std::fs::canonicalize(parent).expect("canonical policy fixture ancestry");
    let root = tempfile::Builder::new()
        .prefix("continuous-cutoff-")
        .tempdir_in(parent)
        .expect("real custody policy adapter fixture");
    let runtime = NodeResourceRuntime::open(super::super::tests::options(
        root.path(),
        super::super::daemon_owner("finite-policy-node".into(), "finite-policy-cluster".into()),
        crate::resource_custody::InstallationContext::FirstInstallation,
    ))
    .expect("real original node runtime");
    let clock = std::sync::Arc::new(crate::cluster::clock::ClusterClockGuard::new(false));
    let original = clock
        .capture_continuous()
        .expect("actual Linux continuous clock")
        .deadline_after(Duration::from_secs(30))
        .expect("original local preparation cutoff");
    let cutoff = original.monotonic_cutoff().expect("paired original cutoff");
    let now = Instant::now();
    let refusal = |original, cutoff, now| {
        let from_profile =
            finite_trial_refusal_for(MemoryClaim::UnknownProducerPeak, original, cutoff, now);
        let before_profile = runtime.finite_unprofiled_trial_refusal(original, cutoff, now);
        assert_eq!(before_profile, from_profile);
        before_profile
    };
    assert_eq!(
        refusal(Some(&original), cutoff, now),
        TrialRefusal::UnknownMemory
    );
    assert_eq!(refusal(None, cutoff, now), TrialRefusal::DeadlineBudget);
    assert_eq!(
        refusal(Some(&original), cutoff + Duration::from_secs(1), now),
        TrialRefusal::DeadlineBudget
    );
    assert_eq!(
        refusal(Some(&original), cutoff, cutoff),
        TrialRefusal::DeadlineBudget
    );

    let before = Instant::now();
    let expiring = clock
        .capture_continuous()
        .expect("actual second capture")
        .deadline_after(Duration::from_millis(20))
        .expect("short original deadline");
    let paired = expiring
        .monotonic_cutoff()
        .expect("fixed scheduling cutoff");
    std::thread::sleep(Duration::from_millis(40));
    // Even an old scheduling timestamp cannot hide real BOOTTIME expiry.
    assert_eq!(
        refusal(Some(&expiring), paired, before),
        TrialRefusal::DeadlineBudget
    );
    assert_eq!(
        runtime.accounting().expect("same unmodified ledger").used,
        super::super::ResourceVector::default()
    );
    runtime.drain().expect("original journal owner drained");
}

#[test]
fn finite_growth_requires_fresh_service_and_resets_on_owner_generation_and_pacing() {
    let mut policy = FinitePolicyRegistry::default();
    let base = Instant::now();
    assert_eq!(policy.diagnostics(base).accepted_samples, 0);
    assert_eq!(
        policy.diagnostics(base).growth_refusal,
        Some(TrialRefusal::InsufficientEvidence)
    );
    for second in 0..=32 {
        let event = sample(base, second, 1, 1500, WorkPhase::Producing);
        policy.observe(class(1), family(1), 1, event, event.at);
    }
    let last = base + Duration::from_secs(32);
    assert_eq!(policy.readiness(class(1), last), Ok(()));
    assert_eq!(policy.diagnostics(last).accepted_samples, 33);
    // A poll cannot turn the original event into another service interval.
    let duplicate = sample(base, 32, 1, 1500, WorkPhase::Producing);
    policy.observe(
        class(1),
        family(1),
        1,
        duplicate,
        last + Duration::from_secs(1),
    );
    assert_eq!(policy.diagnostics(last).accepted_samples, 33);
    assert_eq!(
        policy.diagnostics(last).last_sample.map(|sample| sample.at),
        Some(last)
    );
    assert_eq!(
        policy.readiness(class(1), last + Duration::from_secs(16)),
        Err(TrialRefusal::GrowthNotStable)
    );

    let replacement = sample(base, 33, 1, 1500, WorkPhase::Producing);
    policy.observe(class(1), family(2), 1, replacement, replacement.at);
    assert_eq!(
        policy.readiness(class(1), replacement.at),
        Err(TrialRefusal::GrowthNotStable)
    );
    let generation = sample(base, 34, 2, 1500, WorkPhase::Producing);
    policy.observe(class(1), family(2), 2, generation, generation.at);
    assert_eq!(
        policy.readiness(class(1), generation.at),
        Err(TrialRefusal::GrowthNotStable)
    );
    let paced = sample(base, 35, 2, 1500, WorkPhase::BufferPaced);
    policy.observe(class(1), family(2), 2, paced, paced.at);
    assert_eq!(
        policy.readiness(class(1), paced.at),
        Err(TrialRefusal::GrowthNotStable)
    );
    assert_ne!(
        policy.readiness(class(1), paced.at),
        Err(TrialRefusal::Cooldown)
    );
    // Epoch/class changes never inherit this producer's positive evidence.
    assert_eq!(
        policy.readiness(class(2), paced.at),
        Err(TrialRefusal::InsufficientEvidence)
    );
}

#[test]
fn finite_growth_preserves_class_cooldown_across_replacement_and_registry_exhaustion() {
    let mut policy = FinitePolicyRegistry::default();
    let base = Instant::now();
    for second in 0..=4 {
        let event = sample(base, second, 1, 900, WorkPhase::Producing);
        policy.observe(class(1), family(1), 1, event, event.at);
    }
    let failed = base + Duration::from_secs(4);
    assert_eq!(
        policy.readiness(class(1), failed),
        Err(TrialRefusal::Cooldown)
    );
    for second in 5..=38 {
        let event = sample(base, second, 1, 1500, WorkPhase::Producing);
        policy.observe(class(1), family(2), 1, event, event.at);
    }
    let now = base + Duration::from_secs(38);
    assert_eq!(policy.readiness(class(1), now), Err(TrialRefusal::Cooldown));
    policy.invalidate(class(1), now);
    assert_eq!(
        policy.readiness(class(1), now),
        Err(TrialRefusal::Cooldown),
        "driver termination clears service, not its class failure"
    );
    for index in 2..=MAX_CLASSES + 1 {
        let event = sample(base, 38, 1, 1500, WorkPhase::Producing);
        policy.observe(class(index as u8), family(index as u64), 1, event, now);
    }
    assert_eq!(policy.entries.iter().flatten().count(), MAX_CLASSES);
    assert_eq!(
        policy.readiness(class((MAX_CLASSES + 1) as u8), now),
        Err(TrialRefusal::InsufficientEvidence)
    );
    assert_eq!(policy.readiness(class(1), now), Err(TrialRefusal::Cooldown));
}

#[test]
fn finite_growth_rejects_stale_wrong_generation_and_nonmedia_events_without_false_cooldown() {
    let mut policy = FinitePolicyRegistry::default();
    let base = Instant::now();
    for second in 0..=32 {
        let event = sample(base, second, 1, 1500, WorkPhase::Producing);
        policy.observe(class(1), family(1), 1, event, event.at);
    }
    let stale = sample(base, 33, 1, 1500, WorkPhase::Producing);
    let now = stale.at + Duration::from_secs(16);
    policy.observe(class(1), family(1), 1, stale, now);
    assert_eq!(policy.diagnostics(now).accepted_samples, 33);
    assert_eq!(
        policy.readiness(class(1), now),
        Err(TrialRefusal::GrowthNotStable)
    );
    let wrong = sample(base, 50, 2, 1500, WorkPhase::Producing);
    policy.observe(class(1), family(1), 1, wrong, wrong.at);
    let mut bytes = sample(base, 51, 1, 1500, WorkPhase::Producing);
    bytes.unit = ServiceUnit::Bytes;
    policy.observe(class(1), family(1), 1, bytes, bytes.at);
    assert_eq!(policy.diagnostics(bytes.at).accepted_samples, 33);
    assert_eq!(
        policy.readiness(class(1), bytes.at),
        Err(TrialRefusal::GrowthNotStable)
    );
}
