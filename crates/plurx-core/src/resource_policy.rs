//! Portable advisory trial policy. This never grants a permit or releases a
//! resource: the physical ledger and process owners retain that authority.
//! All timing uses the original monotonic create deadline, not per-attempt SLAs.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::process::observation::Metric;
use crate::resource_history::CostSummary;
use crate::resource_observation::{ServiceUnit, ServiceWindow, WorkPhase, WorkSample};

const PEER_FLOOR: Duration = Duration::from_secs(3);
const TRIAL_CEILING: Duration = Duration::from_secs(5);
const USEFUL_WINDOW: Duration = Duration::from_secs(2);
const RUNWAY_FLOOR: Duration = Duration::from_secs(20);
const FRESHNESS: Duration = Duration::from_secs(15);
const COOLDOWN: Duration = Duration::from_secs(60);
const STABILITY: Duration = Duration::from_secs(30);
const MAX_PROTECTED: usize = 100;

/// Comparison of previously observed durable-unit service windows with this
/// request's remaining deadline. It is advisory: process startup, native
/// peaks, future scenes and cleanup are not bounded by these summaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiniteObservedService {
    Unknown,
    PriorWithinRemaining(Duration),
    PriorAboveRemaining(Duration),
}

pub fn assess_finite_observed_service(
    startup: Option<CostSummary>,
    steady: Option<CostSummary>,
    durable_units: u32,
    remaining: Duration,
) -> FiniteObservedService {
    let Some(startup) = startup else {
        return FiniteObservedService::Unknown;
    };
    if durable_units == 0 || startup.samples == 0 || startup.max_elapsed_ns_per_unit == 0 {
        return FiniteObservedService::Unknown;
    }
    let rest = durable_units - 1;
    let steady_ns = if rest == 0 {
        0
    } else {
        let Some(steady) = steady else {
            return FiniteObservedService::Unknown;
        };
        if steady.samples == 0 || steady.max_elapsed_ns_per_unit == 0 {
            return FiniteObservedService::Unknown;
        }
        steady.max_elapsed_ns_per_unit
    };
    let Some(observed_ns) = steady_ns
        .checked_mul(u64::from(rest))
        .and_then(|ns| ns.checked_add(startup.max_elapsed_ns_per_unit))
    else {
        return FiniteObservedService::Unknown;
    };
    let observed = Duration::from_nanos(observed_ns);
    if observed > remaining {
        FiniteObservedService::PriorAboveRemaining(observed)
    } else {
        FiniteObservedService::PriorWithinRemaining(observed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialRefusal {
    PeerFirst,
    DeadlineBudget,
    UnknownBounds,
    UnknownMemory,
    UnprotectedIncumbent,
    PacedCommitment,
    InvalidEvidence,
    InsufficientEvidence,
    Cooldown,
    GrowthNotStable,
}

/// Existing create authority supplies A, the original deadline and conservative
/// VERIFIED startup/cleanup bounds. An estimate of a peer's preparation latency
/// reserves time, but is never proof that it can finish within that time.
#[derive(Clone, Copy, Debug)]
pub struct TrialBudget {
    pub original_deadline: Instant,
    pub activation_allowance: Duration,
    pub peer_prepare_estimate: Metric<Duration>,
    pub known_capacity_peer: bool,
    pub startup_bound: Metric<Duration>,
    pub cleanup_bound: Metric<Duration>,
    pub memory_bound_known: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrialAllocation {
    /// Deadline for requesting newcomer termination; cleanup has its own bound.
    pub stop_by: Instant,
    /// Deadline by which cleanup must be proved; not a release receipt.
    pub cleanup_by: Instant,
    pub peer_reserved: Duration,
    pub activation_reserved: Duration,
    pub useful_budget: Duration,
    pub cleanup_bound: Duration,
}

impl TrialBudget {
    pub fn allocate(self, now: Instant) -> Result<TrialAllocation, TrialRefusal> {
        if self.known_capacity_peer {
            return Err(TrialRefusal::PeerFirst);
        }
        if !self.memory_bound_known {
            return Err(TrialRefusal::UnknownMemory);
        }
        let (Metric::Known(startup), Metric::Known(cleanup)) =
            (self.startup_bound, self.cleanup_bound)
        else {
            return Err(TrialRefusal::UnknownBounds);
        };
        let remaining = self
            .original_deadline
            .checked_duration_since(now)
            .ok_or(TrialRefusal::DeadlineBudget)?;
        let peer = match self.peer_prepare_estimate {
            Metric::Known(estimate) => estimate.max(PEER_FLOOR),
            Metric::Unknown(_) => PEER_FLOOR,
        };
        let available = remaining
            .checked_sub(self.activation_allowance)
            .and_then(|value| value.checked_sub(peer))
            .ok_or(TrialRefusal::DeadlineBudget)?;
        let total = TRIAL_CEILING.min(remaining / 2).min(available);
        let required = startup
            .checked_add(cleanup)
            .and_then(|value| value.checked_add(USEFUL_WINDOW))
            .ok_or(TrialRefusal::DeadlineBudget)?;
        if total < required {
            return Err(TrialRefusal::DeadlineBudget);
        }
        let useful_budget = total
            .checked_sub(cleanup)
            .ok_or(TrialRefusal::DeadlineBudget)?;
        Ok(TrialAllocation {
            stop_by: now
                .checked_add(useful_budget)
                .ok_or(TrialRefusal::DeadlineBudget)?,
            cleanup_by: now.checked_add(total).ok_or(TrialRefusal::DeadlineBudget)?,
            peer_reserved: peer,
            activation_reserved: self.activation_allowance,
            useful_budget,
            cleanup_bound: cleanup,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitmentKind {
    BufferedVod,
    Capture,
    UnbufferedBytes,
}

/// The adapter must supply EVERY affected incumbent, with generation-bound,
/// fresh client runway clamped to its verified server-produced frontier. Missing
/// or untrusted client telemetry is Unknown, never permission to speculate.
#[derive(Clone, Copy, Debug)]
pub struct ProtectedWork {
    pub owner: u64,
    pub generation: u64,
    pub kind: CommitmentKind,
    pub segment_duration: Duration,
    pub trusted_runway: Metric<Duration>,
    pub observed_at: Instant,
    pub phase: WorkPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialAbort {
    HardPressure,
    DeadlineMiss,
    RunwayLost,
    ProducingMargin,
    EvidenceLost,
    BudgetExpired,
}

impl TrialAbort {
    pub fn capacity_failure(self) -> bool {
        matches!(
            self,
            Self::HardPressure | Self::DeadlineMiss | Self::RunwayLost | Self::ProducingMargin
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialProgress {
    Observing,
    Sufficient,
    Abort(TrialAbort),
}

struct Incumbent {
    generation: u64,
    runway_floor: Duration,
    runway: Duration,
    last_at: Instant,
    phase: WorkPhase,
    window: ServiceWindow,
    below_margin: u8,
}

/// One node's trial owner retains this monitor alongside its ledger reservation.
/// An Abort is a request to stop/reap the newcomer, NEVER proof of release.
/// Losing the monitor itself cannot remove ledger trial custody.
pub struct TrialMonitor {
    allocation: TrialAllocation,
    candidate_generation: u64,
    candidate: ServiceWindow,
    candidate_last_at: Option<Instant>,
    candidate_below_margin: u8,
    incumbents: BTreeMap<u64, Incumbent>,
    abort: Option<TrialAbort>,
}

impl TrialMonitor {
    pub fn start(
        now: Instant,
        allocation: TrialAllocation,
        candidate_generation: u64,
        protected: &[ProtectedWork],
    ) -> Result<Self, TrialRefusal> {
        if allocation.stop_by <= now
            || allocation.cleanup_by < allocation.stop_by
            || candidate_generation == 0
            || protected.len() > MAX_PROTECTED
        {
            return Err(TrialRefusal::InvalidEvidence);
        }
        let mut incumbents = BTreeMap::new();
        for work in protected {
            if work.kind != CommitmentKind::BufferedVod || work.phase == WorkPhase::InputPaced {
                return Err(TrialRefusal::PacedCommitment);
            }
            let age = now
                .checked_duration_since(work.observed_at)
                .ok_or(TrialRefusal::InvalidEvidence)?;
            if age > FRESHNESS || work.owner == 0 || work.generation == 0 {
                return Err(TrialRefusal::UnprotectedIncumbent);
            }
            let floor = allocation
                .useful_budget
                .checked_add(allocation.cleanup_bound)
                .and_then(|value| {
                    work.segment_duration
                        .checked_mul(2)
                        .and_then(|segments| value.checked_add(segments))
                })
                .ok_or(TrialRefusal::InvalidEvidence)?
                .max(RUNWAY_FLOOR);
            if work.segment_duration.is_zero()
                || !matches!(work.trusted_runway, Metric::Known(runway) if runway.checked_sub(age).is_some_and(|current| current >= floor))
            {
                return Err(TrialRefusal::UnprotectedIncumbent);
            }
            let Metric::Known(runway) = work.trusted_runway else {
                return Err(TrialRefusal::UnprotectedIncumbent);
            };
            if incumbents
                .insert(
                    work.owner,
                    Incumbent {
                        generation: work.generation,
                        runway_floor: floor,
                        runway: runway.saturating_sub(age),
                        last_at: now,
                        phase: work.phase,
                        window: ServiceWindow::default(),
                        below_margin: 0,
                    },
                )
                .is_some()
            {
                return Err(TrialRefusal::InvalidEvidence);
            }
        }
        Ok(Self {
            allocation,
            candidate_generation,
            candidate: ServiceWindow::default(),
            candidate_last_at: None,
            candidate_below_margin: 0,
            incumbents,
            abort: None,
        })
    }

    fn stop(&mut self, reason: TrialAbort) -> TrialProgress {
        let reason = *self.abort.get_or_insert(reason);
        TrialProgress::Abort(reason)
    }

    pub fn poll(&mut self, now: Instant, hard_pressure: bool) -> TrialProgress {
        if let Some(reason) = self.abort {
            return TrialProgress::Abort(reason);
        }
        if hard_pressure {
            return self.stop(TrialAbort::HardPressure);
        }
        if now >= self.allocation.stop_by {
            return self.stop(TrialAbort::BudgetExpired);
        }
        if self.incumbents.values().any(|work| {
            now.checked_duration_since(work.last_at)
                .is_none_or(|age| age > FRESHNESS)
        }) {
            return self.stop(TrialAbort::EvidenceLost);
        }
        if self.incumbents.values().any(|work| {
            work.phase != WorkPhase::Paused
                && work
                    .runway
                    .checked_sub(now.duration_since(work.last_at))
                    .is_none_or(|runway| runway < work.runway_floor)
        }) {
            return self.stop(TrialAbort::RunwayLost);
        }
        let candidate_ready = self.candidate_last_at.is_some_and(|at| {
            now.checked_duration_since(at)
                .is_some_and(|age| age <= FRESHNESS)
        }) && self.candidate.snapshot(now).is_some_and(|observation| {
            observation.unit == ServiceUnit::MediaMilliseconds
                && matches!(observation.units_per_second, Metric::Known(rate) if rate >= 1250)
        });
        let incumbents_ready = self.incumbents.values().all(|work| {
            work.phase == WorkPhase::Paused || work.phase == WorkPhase::BufferPaced
                || (work.phase == WorkPhase::Producing && work.window.snapshot(now).is_some_and(|observation| {
                    matches!(observation.units_per_second, Metric::Known(rate) if rate >= 1250)
                }))
        });
        if candidate_ready && incumbents_ready {
            TrialProgress::Sufficient
        } else {
            TrialProgress::Observing
        }
    }

    pub fn candidate_sample(&mut self, sample: WorkSample, now: Instant) -> TrialProgress {
        if sample.generation != self.candidate_generation
            || sample.unit != ServiceUnit::MediaMilliseconds
            || now
                .checked_duration_since(sample.at)
                .is_none_or(|age| age > FRESHNESS)
        {
            return self.stop(TrialAbort::EvidenceLost);
        }
        if self.candidate_last_at.is_some_and(|last| sample.at <= last) {
            return self.poll(now, false);
        }
        if matches!(sample.deadline_slack_ms, Metric::Known(slack) if slack < 0) {
            return self.stop(TrialAbort::DeadlineMiss);
        }
        if let Some(observation) = self.candidate.observe(sample) {
            self.candidate_last_at = Some(sample.at);
            match observation.units_per_second {
                Metric::Known(rate) if sample.phase == WorkPhase::Producing && rate < 1250 => {
                    self.candidate_below_margin += 1;
                }
                _ => self.candidate_below_margin = 0,
            }
            if self.candidate_below_margin >= 2 {
                return self.stop(TrialAbort::ProducingMargin);
            }
        }
        self.poll(now, false)
    }

    pub fn incumbent_sample(
        &mut self,
        owner: u64,
        sample: WorkSample,
        trusted_runway: Metric<Duration>,
        now: Instant,
    ) -> TrialProgress {
        let Some(work) = self.incumbents.get_mut(&owner) else {
            return self.stop(TrialAbort::EvidenceLost);
        };
        if sample.generation != work.generation
            || sample.unit != ServiceUnit::MediaMilliseconds
            || now
                .checked_duration_since(sample.at)
                .is_none_or(|age| age > FRESHNESS)
        {
            return self.stop(TrialAbort::EvidenceLost);
        }
        if sample.at < work.last_at
            || (sample.at == work.last_at && work.window.snapshot(sample.at).is_some())
        {
            return self.poll(now, false);
        }
        if matches!(sample.deadline_slack_ms, Metric::Known(slack) if slack < 0) {
            return self.stop(TrialAbort::DeadlineMiss);
        }
        match trusted_runway {
            Metric::Unknown(_) => return self.stop(TrialAbort::EvidenceLost),
            Metric::Known(runway)
                if runway
                    .checked_sub(now.duration_since(sample.at))
                    .is_none_or(|current| current < work.runway_floor) =>
            {
                return self.stop(TrialAbort::RunwayLost);
            }
            Metric::Known(_) => {}
        }
        if let Some(observation) = work.window.observe(sample) {
            work.last_at = sample.at;
            if let Metric::Known(runway) = trusted_runway {
                work.runway = runway;
            }
            work.phase = sample.phase;
            if sample.phase == WorkPhase::Producing {
                match observation.units_per_second {
                    Metric::Known(rate) if rate < 1250 => work.below_margin += 1,
                    _ => work.below_margin = 0,
                }
            } else {
                // Pacing isn't failure. On resumption ServiceWindow resets and
                // cannot supply positive evidence until three fresh samples.
                work.below_margin = 0;
            }
            if work.below_margin >= 2 {
                return self.stop(TrialAbort::ProducingMargin);
            }
        }
        self.poll(now, false)
    }
}

/// Per-node/class hysteresis only. Protocol errors, missing samples, intentional
/// pacing and deadline-budget exhaustion do not manufacture capacity failures.
#[derive(Default)]
pub struct GrowthState {
    failed_at: Option<Instant>,
    stable_since: Option<Instant>,
    last_stable: Option<Instant>,
    last_observed: Option<Instant>,
}

impl GrowthState {
    pub fn failure(&mut self, now: Instant, reason: TrialAbort) {
        if self.last_observed.is_some_and(|last| now < last) {
            return;
        }
        self.last_observed = Some(now);
        self.stable_since = None;
        self.last_stable = None;
        if reason.capacity_failure() {
            self.failed_at = Some(now);
        }
    }

    pub fn observe(&mut self, now: Instant, stable: bool) {
        if self.last_observed.is_some_and(|last| now <= last) {
            return;
        }
        self.last_observed = Some(now);
        if !stable {
            self.stable_since = None;
            self.last_stable = None;
            return;
        }
        if self
            .last_stable
            .is_none_or(|last| now.duration_since(last) > FRESHNESS)
        {
            self.stable_since = Some(now);
        }
        self.last_stable = Some(now);
    }

    pub fn can_expand(&self, now: Instant) -> Result<(), TrialRefusal> {
        if self.failed_at.is_some_and(|failed| {
            now.checked_duration_since(failed)
                .is_none_or(|age| age < COOLDOWN)
        }) {
            return Err(TrialRefusal::Cooldown);
        }
        if !self.last_stable.is_some_and(|last| {
            now.checked_duration_since(last)
                .is_some_and(|age| age <= FRESHNESS)
        }) || !self.stable_since.is_some_and(|since| {
            now.checked_duration_since(since)
                .is_some_and(|age| age >= STABILITY)
        }) {
            return Err(TrialRefusal::GrowthNotStable);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::observation::UnknownReason;

    #[test]
    fn finite_history_advice_never_fills_missing_phase_or_overflow_with_zero() {
        let startup = CostSummary {
            samples: 1,
            max_cpu_ns_per_unit: 0,
            max_elapsed_ns_per_unit: 2_000_000_000,
            max_observed_resident_bytes: 1,
            last_unix_ms: 1,
        };
        assert_eq!(
            assess_finite_observed_service(Some(startup), None, 2, Duration::from_secs(10)),
            FiniteObservedService::Unknown
        );
        assert_eq!(
            assess_finite_observed_service(
                Some(startup),
                Some(CostSummary {
                    max_elapsed_ns_per_unit: u64::MAX,
                    ..startup
                }),
                3,
                Duration::from_secs(10),
            ),
            FiniteObservedService::Unknown
        );
        assert_eq!(
            assess_finite_observed_service(Some(startup), Some(startup), 3, Duration::from_secs(5)),
            FiniteObservedService::PriorAboveRemaining(Duration::from_secs(6))
        );
        assert_eq!(
            assess_finite_observed_service(Some(startup), Some(startup), 3, Duration::from_secs(7)),
            FiniteObservedService::PriorWithinRemaining(Duration::from_secs(6))
        );
    }

    fn budget(now: Instant) -> TrialBudget {
        TrialBudget {
            original_deadline: now + Duration::from_secs(10),
            activation_allowance: Duration::from_millis(250),
            peer_prepare_estimate: Metric::Unknown(UnknownReason::InsufficientEvidence),
            known_capacity_peer: false,
            startup_bound: Metric::Known(Duration::from_millis(500)),
            cleanup_bound: Metric::Known(Duration::from_millis(500)),
            memory_bound_known: true,
        }
    }
    fn protected(now: Instant, phase: WorkPhase) -> ProtectedWork {
        ProtectedWork {
            owner: 1,
            generation: 4,
            kind: CommitmentKind::BufferedVod,
            segment_duration: Duration::from_secs(4),
            trusted_runway: Metric::Known(Duration::from_secs(30)),
            observed_at: now,
            phase,
        }
    }
    fn sample(now: Instant, generation: u64, phase: WorkPhase, completed: u64) -> WorkSample {
        WorkSample {
            at: now,
            generation,
            phase,
            unit: ServiceUnit::MediaMilliseconds,
            completed,
            deadline_slack_ms: Metric::Known(1000),
            queued_bytes: Metric::Known(0),
        }
    }
    fn monitor(now: Instant, work: &[ProtectedWork]) -> TrialMonitor {
        TrialMonitor::start(
            now,
            budget(now).allocate(now).expect("verified bounds fit"),
            9,
            work,
        )
        .expect("protected trial")
    }

    #[test]
    fn local_trial_preserves_original_peer_and_activation_allowances() {
        let now = Instant::now();
        let budget = budget(now);
        let allocation = budget.allocate(now).expect("trial");
        assert!(
            allocation.cleanup_by + allocation.peer_reserved + allocation.activation_reserved
                <= budget.original_deadline
        );
        assert_eq!(allocation.peer_reserved, Duration::from_secs(3));
        assert!(matches!(
            budget.allocate(now + Duration::from_secs(5)),
            Err(TrialRefusal::DeadlineBudget)
        ));
        assert!(matches!(
            TrialBudget {
                known_capacity_peer: true,
                ..budget
            }
            .allocate(now),
            Err(TrialRefusal::PeerFirst)
        ));
        assert!(matches!(
            TrialBudget {
                memory_bound_known: false,
                ..budget
            }
            .allocate(now),
            Err(TrialRefusal::UnknownMemory)
        ));
        assert!(matches!(
            TrialBudget {
                cleanup_bound: Metric::Unknown(UnknownReason::Inaccessible),
                ..budget
            }
            .allocate(now),
            Err(TrialRefusal::UnknownBounds)
        ));
    }

    #[test]
    fn capture_unbuffered_unknown_and_untrusted_runway_never_protect_speculation() {
        let now = Instant::now();
        let allocation = budget(now).allocate(now).expect("budget");
        for kind in [CommitmentKind::Capture, CommitmentKind::UnbufferedBytes] {
            assert!(matches!(
                TrialMonitor::start(
                    now,
                    allocation,
                    9,
                    &[ProtectedWork {
                        kind,
                        ..protected(now, WorkPhase::InputPaced)
                    }]
                ),
                Err(TrialRefusal::PacedCommitment)
            ));
        }
        assert!(matches!(
            TrialMonitor::start(
                now,
                allocation,
                9,
                &[ProtectedWork {
                    trusted_runway: Metric::Unknown(UnknownReason::Inaccessible),
                    ..protected(now, WorkPhase::Producing)
                }]
            ),
            Err(TrialRefusal::UnprotectedIncumbent)
        ));
        assert!(matches!(
            TrialMonitor::start(now, allocation, 9, &[protected(now, WorkPhase::Paused); 2]),
            Err(TrialRefusal::InvalidEvidence)
        ));
    }

    #[test]
    fn buffer_paced_zero_speed_and_resume_require_fresh_windows_not_false_failure() {
        let now = Instant::now();
        let mut trial = monitor(now, &[protected(now, WorkPhase::BufferPaced)]);
        for second in 0..3 {
            let at = now + Duration::from_secs(second);
            assert!(!matches!(
                trial.incumbent_sample(
                    1,
                    sample(at, 4, WorkPhase::BufferPaced, 30_000),
                    Metric::Known(Duration::from_secs(30)),
                    at
                ),
                TrialProgress::Abort(_)
            ));
            trial.candidate_sample(sample(at, 9, WorkPhase::Producing, second * 2000), at);
        }
        assert_eq!(
            trial.poll(now + Duration::from_secs(2), false),
            TrialProgress::Sufficient
        );
        let at = now + Duration::from_secs(3);
        assert_eq!(
            trial.incumbent_sample(
                1,
                sample(at, 4, WorkPhase::Producing, 30_000),
                Metric::Known(Duration::from_secs(30)),
                at
            ),
            TrialProgress::Observing
        );
        assert_eq!(
            trial.poll(at, true),
            TrialProgress::Abort(TrialAbort::HardPressure)
        );
        assert_eq!(
            trial.poll(at, false),
            TrialProgress::Abort(TrialAbort::HardPressure)
        );
    }

    #[test]
    fn real_margin_and_runway_failures_abort_newcomer_without_claiming_release() {
        let now = Instant::now();
        let mut trial = monitor(now, &[protected(now, WorkPhase::Producing)]);
        for second in 0..4 {
            let at = now + Duration::from_secs(second);
            let result = trial.incumbent_sample(
                1,
                sample(at, 4, WorkPhase::Producing, second * 1000),
                Metric::Known(Duration::from_secs(30)),
                at,
            );
            if second < 3 {
                assert_eq!(result, TrialProgress::Observing);
            } else {
                assert_eq!(result, TrialProgress::Abort(TrialAbort::ProducingMargin));
            }
        }
        let mut trial = monitor(now, &[protected(now, WorkPhase::BufferPaced)]);
        assert_eq!(
            trial.incumbent_sample(
                1,
                sample(now, 4, WorkPhase::BufferPaced, 0),
                Metric::Known(Duration::from_secs(19)),
                now
            ),
            TrialProgress::Abort(TrialAbort::RunwayLost)
        );
        let mut trial = monitor(now, &[]);
        assert_eq!(
            trial.candidate_sample(sample(now, 10, WorkPhase::Producing, 0), now),
            TrialProgress::Abort(TrialAbort::EvidenceLost)
        );
    }

    #[test]
    fn cooldown_only_records_actual_failures_and_growth_requires_current_stability() {
        let now = Instant::now();
        let mut growth = GrowthState::default();
        for second in 0..=30 {
            growth.observe(now + Duration::from_secs(second), true);
        }
        assert_eq!(growth.can_expand(now + Duration::from_secs(30)), Ok(()));
        growth.failure(now + Duration::from_secs(30), TrialAbort::BudgetExpired);
        assert_eq!(
            growth.can_expand(now + Duration::from_secs(30)),
            Err(TrialRefusal::GrowthNotStable)
        );
        growth.failure(now + Duration::from_secs(31), TrialAbort::DeadlineMiss);
        for second in 32..=91 {
            growth.observe(now + Duration::from_secs(second), true);
        }
        assert_eq!(
            growth.can_expand(now + Duration::from_secs(90)),
            Err(TrialRefusal::Cooldown)
        );
        assert_eq!(growth.can_expand(now + Duration::from_secs(91)), Ok(()));
        assert_eq!(
            growth.can_expand(now + Duration::from_secs(107)),
            Err(TrialRefusal::GrowthNotStable)
        );
    }

    #[test]
    fn missing_runway_is_not_capacity_failure_and_poll_rechecks_consumer_drain() {
        let now = Instant::now();
        let work = protected(now, WorkPhase::BufferPaced);
        let mut trial = monitor(now, &[work]);
        assert_eq!(
            trial.incumbent_sample(
                1,
                sample(now, 4, work.phase, 0),
                Metric::Unknown(UnknownReason::Inaccessible),
                now
            ),
            TrialProgress::Abort(TrialAbort::EvidenceLost)
        );
        assert!(!TrialAbort::EvidenceLost.capacity_failure());
        let mut trial = monitor(
            now,
            &[ProtectedWork {
                trusted_runway: Metric::Known(Duration::from_secs(21)),
                ..work
            }],
        );
        assert_eq!(
            trial.poll(now + Duration::from_secs(2), false),
            TrialProgress::Abort(TrialAbort::RunwayLost)
        );
    }
}
