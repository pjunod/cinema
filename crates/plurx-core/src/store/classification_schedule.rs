//! When the metadata-classification pass runs (K-10 §3.2).
//!
//! The worker used to take the cluster lease `metadata-classification`
//! around every 32-entry page, once a second, on every voter: an
//! `acquire_lease` and a `release_lease` per page from the winner and an
//! `acquire_lease` per try from every loser, each a Raft proposal whether it
//! won or not, whether or not the library had changed — measured at 41.6% of
//! every replicated log entry on the idle fleet
//! (docs/cluster/REPLICATED-WRITE-RATE-HYGIENE-M0.md §3).
//!
//! The worker now holds the lease for a whole pass. This module decides when
//! a pass is worth starting, from two **local** reads that never authorize
//! anything:
//!
//! - the lease row ([`CoordinationStore::lease_expiry_hint`]): visibly live
//!   means a peer is walking the library, so there is nothing to contest; a
//!   released or expired row records when the last pass anywhere ended;
//! - the pass hint ([`ClassificationStore::classification_hint`]): whether
//!   any entry is one the pass would write or re-check.
//!
//! A pass starts when the hint fires (and this node's own gap since its last
//! pass has run out), or when [`FORCE_PASS_INTERVAL`] has passed since the
//! last pass anywhere, or on a fresh process that has seen no pass since it
//! started. The lease is still taken on the authority, every page is still
//! read on the authority, and every write is still fenced, so a stale replica
//! can delay a pass but never admit a second one.
//!
//! [`CoordinationStore::lease_expiry_hint`]: super::CoordinationStore::lease_expiry_hint
//! [`ClassificationStore::classification_hint`]: super::classification::ClassificationStore::classification_hint

use std::time::Duration;

use async_trait::async_trait;

use super::Store;
use crate::error::StoreError;

/// The cluster lease one pass holds from its first page to its last.
pub const RESOURCE: &str = "metadata-classification";
/// Cadence between pages while a pass holds the lease (unchanged).
pub const PAGE_TICK: Duration = Duration::from_secs(1);
/// This node's wait after a pass that did work before the hint may start
/// another (the old between-pass sleep).
pub const PASS_GAP: Duration = Duration::from_secs(30);
/// First wait after the hint says nothing.
pub const IDLE_TICK: Duration = Duration::from_secs(30);
/// Ceiling of the idle backoff: the longest a new or changed item waits for
/// the hint to be read again.
pub const IDLE_TICK_MAX: Duration = Duration::from_secs(120);
/// The longest the hint may keep a pass from running, counted from the last
/// pass anywhere in the cluster.
pub const FORCE_PASS_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// How often a node looks at a lease a peer visibly holds.
pub const LEASE_RETRY: Duration = Duration::from_secs(15);

/// What one decision was. The daemon exports these as the five fixed values
/// of `plurx_classification_ticks_total{outcome}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleOutcome {
    /// A peer visibly holds the lease.
    NotOwner,
    /// The hint says nothing is due and no forced pass is owed.
    Idle,
    /// The hint fired but this node's gap since its last pass has not run out.
    Gap,
    /// A pass is due because the hint fired.
    HintedPass,
    /// A pass is due whatever the hint says.
    ForcedPass,
}

impl ScheduleOutcome {
    pub const ALL: [Self; 5] = [
        Self::NotOwner,
        Self::Idle,
        Self::Gap,
        Self::HintedPass,
        Self::ForcedPass,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotOwner => "not_owner",
            Self::Idle => "idle",
            Self::Gap => "gap",
            Self::HintedPass => "hinted_pass",
            Self::ForcedPass => "forced_pass",
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::NotOwner => 0,
            Self::Idle => 1,
            Self::Gap => 2,
            Self::HintedPass => 3,
            Self::ForcedPass => 4,
        }
    }
}

/// One decision: run a pass now, or wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Take the lease and walk the library.
    Pass(ScheduleOutcome),
    /// Look again after this long.
    Wait(Duration, ScheduleOutcome),
}

impl Decision {
    #[must_use]
    pub const fn outcome(self) -> ScheduleOutcome {
        match self {
            Self::Pass(outcome) | Self::Wait(_, outcome) => outcome,
        }
    }
}

/// The two local reads a decision makes. Neither is a proposal or a
/// consistent read.
#[async_trait]
pub trait ClassificationScheduleStore: Send + Sync {
    async fn lease_hint(&self) -> Result<Option<i64>, StoreError>;
    async fn pass_hint(&self, now_unix: i64) -> Result<bool, StoreError>;
}

#[async_trait]
impl<T: Store + ?Sized> ClassificationScheduleStore for T {
    async fn lease_hint(&self) -> Result<Option<i64>, StoreError> {
        self.lease_expiry_hint(RESOURCE).await
    }

    async fn pass_hint(&self, now_unix: i64) -> Result<bool, StoreError> {
        self.classification_hint(now_unix).await
    }
}

/// Per-process schedule. Times are Unix milliseconds because the lease row
/// it reads is; the daemon passes its clock in so tests can step it.
#[derive(Debug)]
pub struct ClassificationSchedule {
    started_ms: i64,
    /// When this node's last completed pass ended.
    own_end_ms: Option<i64>,
    /// The latest pass end seen anywhere: this node's, or a peer's released
    /// or expired lease row.
    seen_end_ms: Option<i64>,
    gap: Duration,
    idle_tick: Duration,
    lease_retry: Duration,
}

impl ClassificationSchedule {
    #[must_use]
    pub fn new(now_ms: i64) -> Self {
        Self::with_lease_retry(now_ms, LEASE_RETRY)
    }

    /// The same schedule with a shorter look at a held lease, for a
    /// failover test that uses a shortened lease.
    #[must_use]
    pub fn with_lease_retry(now_ms: i64, lease_retry: Duration) -> Self {
        Self {
            started_ms: now_ms,
            own_end_ms: None,
            seen_end_ms: None,
            gap: PASS_GAP,
            idle_tick: IDLE_TICK,
            lease_retry,
        }
    }

    /// Decide whether to run a pass now. Two local reads at most.
    pub async fn decide<S>(&mut self, store: &S, now_ms: i64) -> Decision
    where
        S: ClassificationScheduleStore + ?Sized,
    {
        // A failed local read is treated as "no row": the acquire that may
        // follow decides on the authority, so the hint only ever saves work.
        if let Some(expires_ms) = store.lease_hint().await.ok().flatten() {
            if expires_ms > now_ms {
                return Decision::Wait(self.lease_retry, ScheduleOutcome::NotOwner);
            }
            // A released lease's expiry is its release time; an abandoned
            // one's is its last renewal plus the TTL. Either way no pass has
            // run since.
            self.seen_end_ms = self.seen_end_ms.max(Some(expires_ms));
        }
        let last_ms = self.own_end_ms.max(self.seen_end_ms);
        let forced = match last_ms {
            // A fresh process trusts no hint until a pass has run since it
            // started, here or on a peer.
            Some(last) if last >= self.started_ms => {
                now_ms.saturating_sub(last) >= millis(FORCE_PASS_INTERVAL)
            }
            _ => true,
        };
        if forced {
            return Decision::Pass(ScheduleOutcome::ForcedPass);
        }
        // A failed hint is "something may be due", for the same reason.
        let hint = store
            .pass_hint(now_ms.div_euclid(1_000))
            .await
            .unwrap_or(true);
        let until_forced = last_ms
            .map(|last| last + millis(FORCE_PASS_INTERVAL) - now_ms)
            .unwrap_or(0)
            .max(0);
        if hint {
            let ready_ms = self.own_end_ms.map_or(now_ms, |end| end + millis(self.gap));
            if now_ms >= ready_ms {
                return Decision::Pass(ScheduleOutcome::HintedPass);
            }
            let wait = (ready_ms - now_ms).min(until_forced).max(1);
            return Decision::Wait(from_millis(wait), ScheduleOutcome::Gap);
        }
        let wait = from_millis(millis(self.idle_tick).min(until_forced).max(1));
        self.idle_tick = self.idle_tick.saturating_mul(2).min(IDLE_TICK_MAX);
        Decision::Wait(wait, ScheduleOutcome::Idle)
    }

    /// A pass ran to the end of the library. `did_work` is whether it wrote
    /// an entry or made a provider request: a pass that did neither doubles
    /// this node's gap (up to the forced interval), so a hint that keeps
    /// firing for nothing costs one pass per gap, not one per 30 s.
    pub fn pass_finished(&mut self, now_ms: i64, did_work: bool) {
        self.own_end_ms = Some(now_ms);
        self.seen_end_ms = self.seen_end_ms.max(Some(now_ms));
        self.idle_tick = IDLE_TICK;
        self.gap = if did_work {
            PASS_GAP
        } else {
            self.gap.saturating_mul(2).min(FORCE_PASS_INTERVAL)
        };
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn from_millis(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms.max(0)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

    struct Scripted {
        lease: AtomicI64,
        hint: AtomicBool,
    }

    impl Scripted {
        fn new() -> Self {
            Self {
                lease: AtomicI64::new(i64::MIN),
                hint: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl ClassificationScheduleStore for Scripted {
        async fn lease_hint(&self) -> Result<Option<i64>, StoreError> {
            let expires = self.lease.load(Ordering::SeqCst);
            Ok((expires != i64::MIN).then_some(expires))
        }
        async fn pass_hint(&self, _: i64) -> Result<bool, StoreError> {
            Ok(self.hint.load(Ordering::SeqCst))
        }
    }

    const T0: i64 = 1_000_000_000;

    #[tokio::test]
    async fn a_fresh_process_forces_one_pass_then_idles_on_a_silent_hint() {
        let store = Scripted::new();
        let mut schedule = ClassificationSchedule::new(T0);
        assert_eq!(
            schedule.decide(&store, T0).await,
            Decision::Pass(ScheduleOutcome::ForcedPass)
        );
        schedule.pass_finished(T0 + 5_000, false);
        let mut now = T0 + 5_000;
        let mut waits = Vec::new();
        while now < T0 + 5_000 + millis(FORCE_PASS_INTERVAL) {
            match schedule.decide(&store, now).await {
                Decision::Wait(wait, ScheduleOutcome::Idle) => {
                    waits.push(wait);
                    now += millis(wait);
                }
                other => panic!("an idle library must not run a pass: {other:?}"),
            }
        }
        assert_eq!(&waits[..3], &[IDLE_TICK, IDLE_TICK * 2, IDLE_TICK_MAX]);
        assert_eq!(
            schedule.decide(&store, now).await,
            Decision::Pass(ScheduleOutcome::ForcedPass),
            "the forced pass is owed exactly at the interval"
        );
    }

    #[tokio::test]
    async fn a_held_lease_is_left_alone_and_a_peers_pass_counts() {
        let store = Scripted::new();
        let mut schedule = ClassificationSchedule::new(T0);
        store.lease.store(T0 + 90_000, Ordering::SeqCst);
        assert_eq!(
            schedule.decide(&store, T0).await,
            Decision::Wait(LEASE_RETRY, ScheduleOutcome::NotOwner)
        );
        // The peer releases: its row now records when its pass ended, which
        // is after this process started, so no forced pass is owed here.
        store.lease.store(T0 + 20_000, Ordering::SeqCst);
        assert_eq!(
            schedule.decide(&store, T0 + 30_000).await.outcome(),
            ScheduleOutcome::Idle
        );
    }

    #[tokio::test]
    async fn a_dead_peers_lease_is_taken_over_once_it_expires() {
        let store = Scripted::new();
        let mut schedule = ClassificationSchedule::new(T0);
        store.lease.store(T0 + 90_000, Ordering::SeqCst);
        store.hint.store(true, Ordering::SeqCst);
        assert_eq!(
            schedule.decide(&store, T0 + 89_999).await.outcome(),
            ScheduleOutcome::NotOwner
        );
        assert_eq!(
            schedule.decide(&store, T0 + 90_000).await,
            Decision::Pass(ScheduleOutcome::HintedPass),
            "the peer's unfinished work keeps the hint firing"
        );
    }

    #[tokio::test]
    async fn a_hint_that_fires_for_nothing_backs_off_to_the_forced_interval() {
        let store = Scripted::new();
        let mut schedule = ClassificationSchedule::new(T0);
        store.hint.store(true, Ordering::SeqCst);
        let mut now = T0;
        let mut passes = 0;
        while now < T0 + 4 * 3_600_000 {
            match schedule.decide(&store, now).await {
                Decision::Pass(_) => {
                    passes += 1;
                    now += 1_000;
                    schedule.pass_finished(now, false);
                }
                Decision::Wait(wait, _) => now += millis(wait),
            }
        }
        // Gaps of 1, 2, 4, 8 and 16 min, then one pass per 30 min: 12 passes
        // in four hours, where a hint trusted blindly would run one every
        // 30 s.
        assert_eq!(passes, 12);
        schedule.pass_finished(now, true);
        assert_eq!(schedule.gap, PASS_GAP, "real work resets the gap");
    }
}
