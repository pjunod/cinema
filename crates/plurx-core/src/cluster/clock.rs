//! Node-local clock evidence and typed consumer admission. No HTTP or Store calls.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CLOCK_OFFSET_REFUSAL_MS: i64 = 2_000;
pub const CLOCK_OBSERVATION_MAX_AGE: Duration = Duration::from_secs(25);
pub const CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS: i64 = 250;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerClockOffset {
    Bounded {
        offset_us: i64,
        uncertainty_us: i64,
        observed_at: Instant,
    },
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounded(now: Instant) -> BTreeMap<String, PeerClockOffset> {
        BTreeMap::from([(
            "peer".into(),
            PeerClockOffset::Bounded {
                offset_us: 0,
                uncertainty_us: 1000,
                observed_at: now,
            },
        )])
    }

    #[test]
    fn local_discontinuity_invalidates_generation() {
        // Before the next probe, during t1/t4, after a decision ticket, and
        // a common-mode peer step all use the same serialized generation fence.
        for stage in [
            "before probe",
            "during exchange",
            "after ticket",
            "common mode",
        ] {
            let guard = ClusterClockGuard::new(true);
            let ticket = guard.roster(&["peer".into()]);
            assert!(guard.publish(ticket, bounded(Instant::now())));
            let old = guard.ticket();
            {
                let mut inner = guard.inner.lock().expect("clock state lock");
                let before = Instant::now();
                let current_wall = inner.anchor_wall_ms.expect("fixture anchor") + 15_000;
                ClusterClockGuard::continuity(&mut inner, before, Some(current_wall), before);
                assert_eq!(
                    inner.snapshot.clock_generation,
                    old.clock_generation + 1,
                    "{stage}"
                );
                assert!(matches!(
                    inner.snapshot.state,
                    ClusterClockState::Incomplete { .. }
                ));
                assert_eq!(inner.snapshot.peers["peer"], PeerClockOffset::Unknown);
            }
            assert!(!guard.publish(old, bounded(Instant::now())), "{stage}");
        }
    }

    #[test]
    fn fixed_anchor_and_unrepresentable_wall_invalidate() {
        let guard = ClusterClockGuard::new(true);
        let mut inner = guard.inner.lock().expect("clock state lock");
        let mono = inner.anchor_mono;
        let wall = inner.anchor_wall_ms.expect("fixture anchor");
        for delta in [100, 200] {
            ClusterClockGuard::continuity(&mut inner, mono, Some(wall + delta), mono);
            assert_eq!(inner.snapshot.clock_generation, 0);
        }
        ClusterClockGuard::continuity(&mut inner, mono, Some(wall + 300), mono);
        assert_eq!(inner.snapshot.clock_generation, 1);
        ClusterClockGuard::continuity(&mut inner, mono, None, mono);
        assert_eq!(inner.snapshot.clock_generation, 2);
    }

    #[test]
    fn roster_failure_expiry_and_passive_metrics_preserve_unknown() {
        let guard = ClusterClockGuard::new(true);
        assert!(matches!(
            guard.snapshot().state,
            ClusterClockState::Incomplete { .. }
        ));
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(ticket, bounded(Instant::now())));
        assert!(guard
            .prometheus()
            .contains("plurx_cluster_clock_offset_seconds{peer=\"peer\"} 0\n"));
        let old = guard.ticket();
        guard.roster_failed();
        assert!(!guard.publish(old, bounded(Instant::now())));
        assert!(!guard
            .prometheus()
            .contains("plurx_cluster_clock_offset_seconds{peer=\"peer\"}"));
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(ticket, bounded(Instant::now() - Duration::from_secs(26))));
        assert!(matches!(
            guard.snapshot().state,
            ClusterClockState::Incomplete { .. }
        ));
        let ticket = guard.roster(&[]);
        assert!(guard.publish(ticket, BTreeMap::new()));
        assert_eq!(guard.snapshot().state, ClusterClockState::NoPeers);
        assert!(!guard.prometheus().contains("peer=\"peer\""));
        assert_eq!(
            ClusterClockGuard::new(false).snapshot().state,
            ClusterClockState::NoPeers
        );
    }

    fn publish_offset(guard: &ClusterClockGuard, offset_us: i64, uncertainty_us: i64) {
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(
            ticket,
            BTreeMap::from([(
                "peer".into(),
                PeerClockOffset::Bounded {
                    offset_us,
                    uncertainty_us,
                    observed_at: Instant::now(),
                },
            )])
        ));
    }

    #[test]
    fn consumer_refusals_count_only_typed_admission_not_policy_or_scrapes() {
        let guard = ClusterClockGuard::new(true);
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        assert_eq!(
            guard.acquire_for(ClockDecision::ExpiryScan).err(),
            Some(ClockRefusal::Unknown)
        );
        publish_offset(&guard, 0, 1_000);
        let acquired = guard
            .acquire_for(ClockDecision::Takeover)
            .expect("safe acquisition");
        let original_now = acquired.now_ms();
        guard.roster_failed();
        assert_eq!(
            guard.revalidate_for(ClockDecision::Takeover, &acquired),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(acquired.now_ms(), original_now);
        let first = guard.prometheus();
        let second = guard.prometheus();
        assert_eq!(first, second, "scraping must not count another refusal");
        assert!(first.contains(
            "plurx_cluster_clock_refusals_total{decision=\"expiry_scan\",cause=\"unknown\"} 1\n"
        ));
        assert!(first.contains(
            "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"generation_changed\"} 1\n"
        ));
        assert!(first.contains(
            "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"unknown\"} 0\n"
        ));
        assert_eq!(
            first
                .lines()
                .filter(|line| line.starts_with("plurx_cluster_clock_refusals_total{"))
                .count(),
            12,
            "metric cardinality is exactly three decisions by four causes"
        );
    }

    #[test]
    fn acquisition_current_missing_peer_invalidates_atomically_but_stale_round_does_not() {
        let guard = ClusterClockGuard::new(true);
        publish_offset(&guard, 0, 1_000);
        let safe = guard.acquire().expect("safe prior evidence");
        let round = guard.roster(&["peer".into()]);
        assert!(!guard.publish(round, BTreeMap::new()));
        assert_eq!(
            guard.revalidate(&safe),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));

        let stale = guard.roster(&["peer".into()]);
        publish_offset(&guard, 0, 1_000);
        let newer = guard.acquire().expect("newer safe evidence");
        assert!(!guard.publish(stale, BTreeMap::new()));
        assert_eq!(guard.revalidate(&newer), Ok(()));
        assert!(guard.acquire().is_ok());
    }

    #[test]
    fn acquisition_requires_exact_fresh_roster_and_safe_upper_bound() {
        let standalone = ClusterClockGuard::new(false);
        let ticket = standalone.acquire().expect("standalone proof");
        assert!(ticket.now_ms() > 0);
        assert_eq!(standalone.revalidate(&ticket), Ok(()));

        let guard = ClusterClockGuard::new(true);
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        guard.roster(&[]);
        let empty = guard.acquire().expect("proved empty committed roster");
        assert_eq!(guard.revalidate(&empty), Ok(()));
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            inner.roster_observed_at = Some(Instant::now() - Duration::from_secs(26));
        }
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        assert_eq!(
            guard.revalidate(&empty),
            Err(ClockRefusal::GenerationChanged)
        );

        // Exact threshold includes uncertainty, on either side of zero.
        for offset in [1_999_000, -1_999_000] {
            publish_offset(&guard, offset, 1_000);
            assert!(guard.acquire().is_ok());
        }
        for (offset, uncertainty) in [(1_500_000, 601_000), (-2_500_000, 1_000)] {
            publish_offset(&guard, offset, uncertainty);
            assert_eq!(guard.acquire().err(), Some(ClockRefusal::Offset));
        }
        for (offset, uncertainty) in [(i64::MIN, 1_000), (0, -1)] {
            publish_offset(&guard, offset, uncertainty);
            assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        }
        publish_offset(&guard, 0, 1_000);
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            inner.snapshot.peers = bounded(Instant::now() - Duration::from_secs(26));
        }
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        publish_offset(&guard, 0, 1_000);
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            inner.snapshot.peers = bounded(Instant::now() + Duration::from_secs(1));
        }
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        guard.roster_failed();
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
    }

    #[test]
    fn acquisition_ticket_is_guard_bound_and_rechecks_both_generations() {
        let guard = ClusterClockGuard::new(true);
        publish_offset(&guard, 0, 1_000);
        let ticket = guard.acquire().expect("safe initial ticket");
        let original_now = ticket.now_ms();
        let other = ClusterClockGuard::new(true);
        publish_offset(&other, 0, 1_000);
        assert_eq!(
            other.revalidate(&ticket),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(guard.revalidate(&ticket), Ok(()));
        assert_eq!(ticket.now_ms(), original_now);

        publish_offset(&guard, 0, 1_000);
        assert_eq!(
            guard.revalidate(&ticket),
            Err(ClockRefusal::GenerationChanged)
        );
        let ticket = guard.acquire().expect("new safe ticket");
        guard.roster(&["peer".into(), "new-peer".into()]);
        assert_eq!(
            guard.revalidate(&ticket),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));

        publish_offset(&guard, 0, 1_000);
        let ticket = guard.acquire().expect("safe before failed round");
        let round = guard.roster(&["peer".into()]);
        assert!(guard.publish(
            round,
            BTreeMap::from([("peer".into(), PeerClockOffset::Unknown)])
        ));
        assert_eq!(
            guard.revalidate(&ticket),
            Err(ClockRefusal::GenerationChanged)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
    }

    #[test]
    fn acquisition_refuses_common_mode_step_and_unrepresentable_wall() {
        let guard = ClusterClockGuard::new(true);
        publish_offset(&guard, 0, 1_000);
        let ticket = guard.acquire().expect("safe before common-mode step");
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            // Relative peer offsets still say zero. Only synchronous local
            // continuity at the actual policy read can invalidate this proof.
            *inner.anchor_wall_ms.as_mut().expect("fixture anchor") += 15_000;
        }
        assert_eq!(
            guard.revalidate(&ticket),
            Err(ClockRefusal::LocalDiscontinuity)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        assert!(!guard.snapshot().readiness.is_unbounded());
        publish_offset(&guard, 0, 1_000);
        {
            let mut inner = guard.inner.lock().expect("clock state lock");
            inner.anchor_wall_ms = None;
        }
        assert_eq!(
            guard.acquire().err(),
            Some(ClockRefusal::LocalDiscontinuity)
        );
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
    }

    #[test]
    fn readiness_counts_completed_positive_rounds_and_resets_on_invalid_evidence() {
        let guard = ClusterClockGuard::new(true);
        assert!(!guard.snapshot().readiness.is_unbounded());
        publish_offset(&guard, 2_500_000, 1_000);
        for _ in 0..10 {
            assert!(
                !guard.snapshot().readiness.is_unbounded(),
                "polls are not rounds"
            );
        }
        let invalid = guard.roster(&["peer".into()]);
        assert!(!guard.publish(invalid, BTreeMap::new()));
        assert!(!guard.snapshot().readiness.is_unbounded());
        publish_offset(&guard, -2_500_000, 1_000);
        assert!(!guard.snapshot().readiness.is_unbounded());
        publish_offset(&guard, -2_500_000, 1_000);
        assert!(guard.snapshot().readiness.is_unbounded());
        for reset in [
            "safe",
            "unknown",
            "failure",
            "expiry",
            "roster",
            "continuity",
        ] {
            match reset {
                "safe" => publish_offset(&guard, 0, 1_000),
                "unknown" => {
                    let round = guard.roster(&["peer".into()]);
                    assert!(guard.publish(
                        round,
                        BTreeMap::from([("peer".into(), PeerClockOffset::Unknown)])
                    ));
                }
                "failure" => guard.roster_failed(),
                "expiry" => {
                    let mut inner = guard.inner.lock().expect("clock state lock");
                    inner.snapshot.peers = bounded(Instant::now() - Duration::from_secs(26));
                }
                "roster" => {
                    guard.roster(&[]);
                }
                "continuity" => {
                    let mut inner = guard.inner.lock().expect("clock state lock");
                    inner.anchor_wall_ms = None;
                    drop(inner);
                    assert_eq!(
                        guard.acquire().err(),
                        Some(ClockRefusal::LocalDiscontinuity)
                    );
                }
                _ => unreachable!("listed fixture reset"),
            }
            assert!(!guard.snapshot().readiness.is_unbounded(), "{reset}");
            publish_offset(&guard, 2_500_000, 1_000);
            assert!(
                !guard.snapshot().readiness.is_unbounded(),
                "first round after {reset}"
            );
            publish_offset(&guard, 2_500_000, 1_000);
            assert!(
                guard.snapshot().readiness.is_unbounded(),
                "second round after {reset}"
            );
        }
        let metrics = guard.prometheus();
        assert!(metrics.contains("measurement-only emits zero"));
        assert!(metrics.contains(
            "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"offset\"} 0"
        ));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterClockState {
    NoPeers,
    Bounded {
        worst_abs_upper_us: i64,
    },
    Incomplete {
        unknown_peers: usize,
        worst_abs_upper_us: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDecisionTicket {
    pub clock_generation: u64,
    pub state_generation: u64,
    pub now_ms: i64,
}

/// Typed local refusal; it never establishes a submitted proposal's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClockRefusal {
    #[error("clock coverage is incomplete or expired")]
    Unknown,
    #[error("clock offset upper bound exceeds the safety limit")]
    Offset,
    #[error("local wall clock continuity changed")]
    LocalDiscontinuity,
    #[error("clock decision evidence changed")]
    GenerationChanged,
}

/// Closed metric vocabulary: no target, session, or caller-supplied labels.
#[derive(Clone, Copy, Debug)]
pub enum ClockDecision {
    Takeover,
    MembershipChange,
    ExpiryScan,
}

impl ClockDecision {
    const ALL: [Self; 3] = [Self::Takeover, Self::MembershipChange, Self::ExpiryScan];

    const fn index(self) -> usize {
        match self {
            Self::Takeover => 0,
            Self::MembershipChange => 1,
            Self::ExpiryScan => 2,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Takeover => "takeover",
            Self::MembershipChange => "membership_change",
            Self::ExpiryScan => "expiry_scan",
        }
    }
}

impl ClockRefusal {
    const ALL: [Self; 4] = [
        Self::Offset,
        Self::Unknown,
        Self::LocalDiscontinuity,
        Self::GenerationChanged,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Offset => 0,
            Self::Unknown => 1,
            Self::LocalDiscontinuity => 2,
            Self::GenerationChanged => 3,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Offset => "offset",
            Self::Unknown => "unknown",
            Self::LocalDiscontinuity => "local_discontinuity",
            Self::GenerationChanged => "generation_changed",
        }
    }
}

/// An acquisition proof belongs to this exact shared guard, not another node's
/// equal-looking generations. Unlike a measurement ticket it cannot be forged.
#[derive(Clone, Copy)]
pub struct ClockAcquisitionTicket<'guard> {
    guard: &'guard ClusterClockGuard,
    decision: ClockDecisionTicket,
}

impl ClockAcquisitionTicket<'_> {
    /// Bind this original value to the caller's expiry query/proposal. A later
    /// revalidation never replaces it with a fresh wall reading.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.decision.now_ms
    }
}

/// Passive facts from completed observation rounds, not readiness polls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockReadiness {
    consecutive_violating_rounds: u8,
}

impl ClockReadiness {
    #[must_use]
    pub fn is_unbounded(self) -> bool {
        self.consecutive_violating_rounds >= 2
    }
}

#[derive(Clone, Debug)]
pub struct ClockSnapshot {
    pub state: ClusterClockState,
    pub peers: BTreeMap<String, PeerClockOffset>,
    pub clock_generation: u64,
    pub state_generation: u64,
    pub discontinuities: u64,
    pub unknown_rounds: u64,
    pub readiness: ClockReadiness,
}

struct ClockInner {
    snapshot: ClockSnapshot,
    roster_proved: bool,
    anchor_wall_ms: Option<i64>,
    anchor_mono: Instant,
    standalone: bool,
    roster_observed_at: Option<Instant>,
}

pub struct ClusterClockGuard {
    inner: Mutex<ClockInner>,
    authority_reads: AtomicU64,
    refusals: [[AtomicU64; 4]; 3],
}

fn wall_ms() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
}

impl ClusterClockGuard {
    #[must_use]
    pub fn new(replicated: bool) -> Self {
        let mono = Instant::now();
        Self {
            authority_reads: AtomicU64::new(0),
            refusals: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            inner: Mutex::new(ClockInner {
                snapshot: ClockSnapshot {
                    state: if replicated {
                        ClusterClockState::Incomplete {
                            unknown_peers: 1,
                            worst_abs_upper_us: 0,
                        }
                    } else {
                        ClusterClockState::NoPeers
                    },
                    peers: BTreeMap::new(),
                    clock_generation: 0,
                    state_generation: 0,
                    discontinuities: 0,
                    unknown_rounds: 0,
                    readiness: ClockReadiness::default(),
                },
                roster_proved: !replicated,
                anchor_wall_ms: wall_ms(),
                anchor_mono: mono,
                standalone: !replicated,
                roster_observed_at: None,
            }),
        }
    }

    /// Synchronously check fixed wall/monotonic continuity, serialized with evidence.
    #[must_use]
    pub fn ticket(&self) -> ClockDecisionTicket {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = Instant::now();
        let wall = wall_ms();
        let after = Instant::now();
        Self::continuity(&mut inner, before, wall, after)
    }

    /// Prepare a pure acquisition proof from one serialized continuity/state
    /// read. Future consumers must revalidate immediately before submission.
    /// This does not submit, count a production refusal, or cancel an existing
    /// commit-unknown proposal.
    pub fn acquire(&self) -> Result<ClockAcquisitionTicket<'_>, ClockRefusal> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = inner.snapshot.clock_generation;
        let before = Instant::now();
        let decision = Self::continuity(&mut inner, before, wall_ms(), Instant::now());
        if decision.clock_generation != generation {
            return Err(ClockRefusal::LocalDiscontinuity);
        }
        Self::acquisition_policy(&inner)?;
        Ok(ClockAcquisitionTicket {
            guard: self,
            decision,
        })
    }

    /// Close both generation races after awaited preparation. This is a local
    /// synchronous check only; it cannot establish whether a submitted CAS won.
    pub fn revalidate(&self, ticket: &ClockAcquisitionTicket<'_>) -> Result<(), ClockRefusal> {
        if !std::ptr::eq(self, ticket.guard) {
            return Err(ClockRefusal::GenerationChanged);
        }
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = Instant::now();
        let current = Self::continuity(&mut inner, before, wall_ms(), Instant::now());
        if current.clock_generation != ticket.decision.clock_generation {
            return Err(ClockRefusal::LocalDiscontinuity);
        }
        if current.state_generation != ticket.decision.state_generation {
            return Err(ClockRefusal::GenerationChanged);
        }
        Self::acquisition_policy(&inner)
    }

    /// Actual consumer entry only; pure policy inspection remains uncounted.
    pub fn acquire_for(
        &self,
        decision: ClockDecision,
    ) -> Result<ClockAcquisitionTicket<'_>, ClockRefusal> {
        self.acquire().inspect_err(|cause| {
            self.refusals[decision.index()][cause.index()].fetch_add(1, Ordering::Relaxed);
        })
    }

    /// Preserve the original caller time across awaits, counting only a real
    /// refusal to submit. Never use this to abandon a commit-unknown proposal.
    pub fn revalidate_for(
        &self,
        decision: ClockDecision,
        ticket: &ClockAcquisitionTicket<'_>,
    ) -> Result<(), ClockRefusal> {
        self.revalidate(ticket).inspect_err(|cause| {
            self.refusals[decision.index()][cause.index()].fetch_add(1, Ordering::Relaxed);
        })
    }

    fn acquisition_policy(inner: &ClockInner) -> Result<(), ClockRefusal> {
        match inner.snapshot.state {
            ClusterClockState::NoPeers if inner.roster_proved => Ok(()),
            ClusterClockState::Bounded { worst_abs_upper_us }
                if worst_abs_upper_us <= CLOCK_OFFSET_REFUSAL_MS * 1_000 =>
            {
                Ok(())
            }
            ClusterClockState::Bounded { .. } => Err(ClockRefusal::Offset),
            _ => Err(ClockRefusal::Unknown),
        }
    }

    fn continuity(
        inner: &mut ClockInner,
        before: Instant,
        wall: Option<i64>,
        after: Instant,
    ) -> ClockDecisionTicket {
        let valid = inner
            .anchor_wall_ms
            .zip(wall)
            .is_some_and(|(anchor, current)| {
                let delta = i128::from(current) - i128::from(anchor);
                let lower = before
                    .saturating_duration_since(inner.anchor_mono)
                    .as_millis() as i128
                    - i128::from(CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS);
                let upper = after
                    .saturating_duration_since(inner.anchor_mono)
                    .as_millis() as i128
                    + i128::from(CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS);
                (lower..=upper).contains(&delta)
            });
        if !valid {
            inner.snapshot.clock_generation += 1;
            inner.snapshot.state_generation += 1;
            inner.snapshot.discontinuities += 1;
            for peer in inner.snapshot.peers.values_mut() {
                *peer = PeerClockOffset::Unknown;
            }
            inner.roster_proved = false;
            inner.roster_observed_at = None;
            inner.snapshot.readiness = ClockReadiness::default();
            inner.anchor_wall_ms = wall;
            inner.anchor_mono = before;
        }
        Self::expire(inner, after);
        Self::recompute(inner);
        ClockDecisionTicket {
            clock_generation: inner.snapshot.clock_generation,
            state_generation: inner.snapshot.state_generation,
            now_ms: wall.unwrap_or_default(),
        }
    }

    fn expire(inner: &mut ClockInner, now: Instant) {
        let mut changed = false;
        if !inner.standalone
            && inner.roster_proved
            && !inner.roster_observed_at.is_some_and(|observed| {
                now.checked_duration_since(observed)
                    .is_some_and(|age| age <= CLOCK_OBSERVATION_MAX_AGE)
            })
        {
            inner.roster_proved = false;
            changed = true;
        }
        for peer in inner.snapshot.peers.values_mut() {
            if matches!(peer, PeerClockOffset::Bounded { observed_at, .. } if !now.checked_duration_since(*observed_at).is_some_and(|age| age <= CLOCK_OBSERVATION_MAX_AGE))
            {
                *peer = PeerClockOffset::Unknown;
                changed = true;
            }
        }
        if changed {
            inner.snapshot.state_generation += 1;
            inner.snapshot.readiness = ClockReadiness::default();
        }
    }

    fn recompute(inner: &mut ClockInner) {
        let mut unknown = 0;
        let mut worst = 0;
        for peer in inner.snapshot.peers.values() {
            match peer {
                PeerClockOffset::Unknown => unknown += 1,
                PeerClockOffset::Bounded {
                    offset_us,
                    uncertainty_us,
                    ..
                } => {
                    match offset_us
                        .checked_abs()
                        .and_then(|value| value.checked_add(*uncertainty_us))
                    {
                        Some(value) if *uncertainty_us >= 0 => worst = worst.max(value),
                        _ => unknown += 1,
                    }
                }
            }
        }
        inner.snapshot.state = if !inner.roster_proved || unknown > 0 {
            ClusterClockState::Incomplete {
                unknown_peers: unknown.max(usize::from(!inner.roster_proved)),
                worst_abs_upper_us: worst,
            }
        } else if inner.snapshot.peers.is_empty() {
            ClusterClockState::NoPeers
        } else {
            ClusterClockState::Bounded {
                worst_abs_upper_us: worst,
            }
        };
    }

    /// Install the exact committed roster before starting fanout. New members are Unknown.
    pub fn roster(&self, peers: &[String]) -> ClockDecisionTicket {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = Instant::now();
        let wall = wall_ms();
        let ticket = Self::continuity(&mut inner, before, wall, Instant::now());
        let changed = !inner.roster_proved || inner.snapshot.peers.keys().ne(peers.iter());
        if changed {
            inner.snapshot.peers.retain(|id, _| peers.contains(id));
            for id in peers {
                inner
                    .snapshot
                    .peers
                    .entry(id.clone())
                    .or_insert(PeerClockOffset::Unknown);
            }
            inner.roster_proved = true;
            inner.snapshot.state_generation += 1;
            inner.snapshot.readiness = ClockReadiness::default();
        }
        inner.roster_observed_at = Some(Instant::now());
        Self::recompute(&mut inner);
        ClockDecisionTicket {
            state_generation: inner.snapshot.state_generation,
            ..ticket
        }
    }

    /// A round publishes atomically only if continuity and roster remained identical.
    pub fn publish(
        &self,
        ticket: ClockDecisionTicket,
        peers: BTreeMap<String, PeerClockOffset>,
    ) -> bool {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = Instant::now();
        let wall = wall_ms();
        let current = Self::continuity(&mut inner, before, wall, Instant::now());
        if current.clock_generation != ticket.clock_generation
            || current.state_generation != ticket.state_generation
            || inner.snapshot.clock_generation != ticket.clock_generation
            || inner.snapshot.state_generation != ticket.state_generation
        {
            // An obsolete round has no authority to invalidate newer evidence.
            return false;
        }
        if inner.snapshot.peers.keys().ne(peers.keys()) {
            // A failed current round must invalidate proofs before releasing the lock.
            inner.roster_proved = false;
            inner.roster_observed_at = None;
            for peer in inner.snapshot.peers.values_mut() {
                *peer = PeerClockOffset::Unknown;
            }
            inner.snapshot.state_generation += 1;
            inner.snapshot.unknown_rounds += 1;
            inner.snapshot.readiness = ClockReadiness::default();
            Self::recompute(&mut inner);
            return false;
        }
        let failed = peers
            .values()
            .any(|peer| matches!(peer, PeerClockOffset::Unknown));
        inner.snapshot.peers = peers;
        if failed {
            inner.snapshot.unknown_rounds += 1;
        }
        // Every completed round invalidates tickets, including a newly failed peer.
        inner.snapshot.state_generation += 1;
        Self::expire(&mut inner, Instant::now());
        Self::recompute(&mut inner);
        inner.snapshot.readiness.consecutive_violating_rounds = match inner.snapshot.state {
            ClusterClockState::Bounded { worst_abs_upper_us }
                if worst_abs_upper_us > CLOCK_OFFSET_REFUSAL_MS * 1_000 =>
            {
                inner
                    .snapshot
                    .readiness
                    .consecutive_violating_rounds
                    .saturating_add(1)
                    .min(2)
            }
            _ => 0,
        };
        true
    }

    pub fn roster_failed(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.roster_proved = false;
        inner.roster_observed_at = None;
        inner.snapshot.readiness = ClockReadiness::default();
        for peer in inner.snapshot.peers.values_mut() {
            *peer = PeerClockOffset::Unknown;
        }
        inner.snapshot.state_generation += 1;
        inner.snapshot.unknown_rounds += 1;
        Self::recompute(&mut inner);
    }

    #[must_use]
    pub fn snapshot(&self) -> ClockSnapshot {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::expire(&mut inner, Instant::now());
        Self::recompute(&mut inner);
        inner.snapshot.clone()
    }

    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn record_authority_read(&self) {
        self.authority_reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Passive scrape: no continuity sampling, membership lookup or Store.
    #[must_use]
    pub fn prometheus(&self) -> String {
        let snapshot = self.snapshot();
        let mut out = String::from("# HELP plurx_cluster_clock_observation_state Per-peer offset observation state.\n# TYPE plurx_cluster_clock_observation_state gauge\n# HELP plurx_cluster_clock_offset_seconds Measured clock offset to a committed peer.\n# TYPE plurx_cluster_clock_offset_seconds gauge\n# HELP plurx_cluster_clock_offset_uncertainty_seconds Filtered network half-delay plus quantization.\n# TYPE plurx_cluster_clock_offset_uncertainty_seconds gauge\n");
        for (id, observation) in &snapshot.peers {
            let peer = id
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            let bounded = matches!(observation, PeerClockOffset::Bounded { .. });
            out.push_str(&format!("plurx_cluster_clock_observation_state{{peer=\"{peer}\",state=\"bounded\"}} {}\nplurx_cluster_clock_observation_state{{peer=\"{peer}\",state=\"unknown\"}} {}\n", u8::from(bounded), u8::from(!bounded)));
            if let PeerClockOffset::Bounded {
                offset_us,
                uncertainty_us,
                ..
            } = observation
            {
                out.push_str(&format!("plurx_cluster_clock_offset_seconds{{peer=\"{peer}\"}} {}\nplurx_cluster_clock_offset_uncertainty_seconds{{peer=\"{peer}\"}} {}\n", *offset_us as f64 / 1_000_000.0, *uncertainty_us as f64 / 1_000_000.0));
            }
        }
        out.push_str(&format!("# HELP plurx_cluster_clock_discontinuities_total Local wall/monotonic discontinuities observed.\n# TYPE plurx_cluster_clock_discontinuities_total counter\nplurx_cluster_clock_discontinuities_total {}\n# HELP plurx_cluster_clock_unknown_rounds_total Completed failed or incomplete observation rounds.\n# TYPE plurx_cluster_clock_unknown_rounds_total counter\nplurx_cluster_clock_unknown_rounds_total {}\n# HELP plurx_cluster_clock_authority_reads_total Consistent authority reads attributable to inbound clock requests.\n# TYPE plurx_cluster_clock_authority_reads_total counter\nplurx_cluster_clock_authority_reads_total {}\n# HELP plurx_cluster_clock_refusals_total Decisions refused because the clock could not be bounded (measurement-only emits zero).\n# TYPE plurx_cluster_clock_refusals_total counter\n", snapshot.discontinuities, snapshot.unknown_rounds, self.authority_reads.load(Ordering::Relaxed)));
        for decision in ClockDecision::ALL {
            for cause in ClockRefusal::ALL {
                let count = self.refusals[decision.index()][cause.index()].load(Ordering::Relaxed);
                let decision = decision.label();
                let cause = cause.label();
                out.push_str(&format!("plurx_cluster_clock_refusals_total{{decision=\"{decision}\",cause=\"{cause}\"}} {count}\n"));
            }
        }
        out
    }
}
