//! The offline worker's claim policy (K-10 §3.1).
//!
//! The worker used to run the replicated claim `UPDATE offline_packages …
//! RETURNING` every two seconds on every voter. That statement is a Raft
//! proposal whether or not a package is queued, so an idle cluster paid
//! three proposals every two seconds for nothing — measured at 14.0% of
//! every replicated log entry on the idle fleet
//! (docs/cluster/REPLICATED-WRITE-RATE-HYGIENE-M0.md §3).
//!
//! This module decides, one pass at a time, whether the authority is worth
//! asking. It owns no timer and no task, so the daemon's loop and the
//! three-voter Store contract drive the same code and count the same writes.
//!
//! What it never does is decide *which* package is produced. The local queue
//! read is a hint in both directions: `false` can be a replica that has not
//! applied a re-home or a re-enable yet, `true` can be a package this node
//! has since claimed. Only
//! [`OfflinePackageStore::claim_next_offline_package`]'s replicated claim binds
//! a package to a producer, and a hint that keeps saying "nothing" is
//! overruled at least once every [`HINT_FORCE_INTERVAL`] so a replica that
//! stopped applying cannot strand a queued package.
//!
//! [`OfflinePackageStore::claim_next_offline_package`]: super::OfflinePackageStore::claim_next_offline_package

use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::Store;
use crate::domain::OfflinePackage;
use crate::error::StoreError;

/// Cadence while there is, or may be, work: the worker's old fixed poll.
pub const BASE_TICK: Duration = Duration::from_secs(2);
/// Ceiling of the idle backoff (K-03's value, reused).
pub const IDLE_TICK_MAX: Duration = Duration::from_secs(10);
/// The longest a hint that keeps saying "nothing" may suppress the real
/// claim (K-03's value, reused).
pub const HINT_FORCE_INTERVAL: Duration = Duration::from_secs(30);

/// Why one pass did or did not claim. The daemon exports these as the four
/// fixed values of `plurx_offline_claim_ticks_total{outcome}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The local hint saw nothing queued and the forced claim was not owed.
    SkippedHint,
    /// A claim was owed but the worker's own gates refused it (offline
    /// disabled, not a committed voter, or a restart draining this node).
    Gated,
    /// The replicated claim returned a package.
    Claimed,
    /// The replicated claim ran and returned nothing.
    EmptyClaim,
}

impl ClaimOutcome {
    pub const ALL: [Self; 4] = [
        Self::SkippedHint,
        Self::Gated,
        Self::Claimed,
        Self::EmptyClaim,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkippedHint => "skipped_hint",
            Self::Gated => "gated",
            Self::Claimed => "claimed",
            Self::EmptyClaim => "empty_claim",
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::SkippedHint => 0,
            Self::Gated => 1,
            Self::Claimed => 2,
            Self::EmptyClaim => 3,
        }
    }
}

/// The two Store calls one pass can make, named by what they cost.
#[async_trait]
pub trait OfflineClaimStore: Send + Sync {
    /// One local, non-consensus read. A hint, never an authorization.
    async fn queue_hint(&self, node_id: &str) -> Result<bool, StoreError>;
    /// The replicated claim: one Raft proposal whether or not a package is
    /// queued.
    async fn claim(&self, node_id: &str) -> Result<Option<OfflinePackage>, StoreError>;
}

#[async_trait]
impl<T: Store + ?Sized> OfflineClaimStore for T {
    async fn queue_hint(&self, node_id: &str) -> Result<bool, StoreError> {
        self.offline_queue_hint(node_id).await
    }

    async fn claim(&self, node_id: &str) -> Result<Option<OfflinePackage>, StoreError> {
        self.claim_next_offline_package(node_id).await
    }
}

/// Per-process claim state. Monotonic `Instant`s are passed in so the loop
/// can use Tokio's clock and the tests can step a virtual one.
#[derive(Debug)]
pub struct OfflineClaimPolicy {
    last_claim: Option<Instant>,
    /// Claim on the next pass whatever the hint says.
    force_next: bool,
    /// What the last hint said, so the claim that follows it can tell
    /// replication lag (hint yes, authority no) from an idle queue.
    hinted: bool,
    delay: Duration,
}

impl Default for OfflineClaimPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl OfflineClaimPolicy {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_claim: None,
            // A fresh worker (a restart that just requeued its interrupted
            // packages) asks the authority once before it trusts any hint.
            force_next: true,
            hinted: false,
            delay: Duration::ZERO,
        }
    }

    /// How long the loop should wait before the next pass.
    #[must_use]
    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// This node created a package for itself. The write returned from the
    /// authority but may not be applied to the local replica yet, so the
    /// next pass claims whatever the hint says, and runs at once.
    pub fn wake(&mut self) {
        self.force_next = true;
        self.delay = Duration::ZERO;
    }

    /// The first half of a pass: read the local hint and say whether the
    /// authority is worth asking now. `false` is a [`ClaimOutcome::SkippedHint`]
    /// and already scheduled; `true` means the caller runs its own gates and
    /// then [`Self::claim`] or [`Self::gated`].
    pub async fn should_claim<S>(&mut self, store: &S, node_id: &str, now: Instant) -> bool
    where
        S: OfflineClaimStore + ?Sized,
    {
        // A failed hint is treated as "something may be queued": the hint
        // only ever saves work, it must never be the reason a package waits.
        let hint = store.queue_hint(node_id).await.unwrap_or(true);
        self.hinted = hint;
        let forced = self.force_next
            || self
                .last_claim
                .is_none_or(|at| now.saturating_duration_since(at) >= HINT_FORCE_INTERVAL);
        if hint || forced {
            return true;
        }
        self.back_off();
        self.keep_force_deadline(now);
        false
    }

    /// The worker's gates refused a claim that [`Self::should_claim`] owed.
    /// Nothing could have been claimed, so this is scheduled like an empty
    /// claim: at the base cadence while a package is visibly queued, backing
    /// off otherwise. It is not a proposal.
    pub fn gated(&mut self, now: Instant) {
        self.force_next = false;
        self.last_claim = Some(now);
        if self.hinted {
            self.delay = BASE_TICK;
        } else {
            self.back_off();
            self.keep_force_deadline(now);
        }
    }

    /// The second half of a pass: the replicated claim.
    pub async fn claim<S>(
        &mut self,
        store: &S,
        node_id: &str,
        now: Instant,
    ) -> Result<(ClaimOutcome, Option<OfflinePackage>), StoreError>
    where
        S: OfflineClaimStore + ?Sized,
    {
        let claimed = match store.claim(node_id).await {
            Ok(claimed) => claimed,
            Err(error) => {
                // Owed claims stay owed; retry at the base cadence.
                self.delay = BASE_TICK;
                return Err(error);
            }
        };
        self.last_claim = Some(now);
        match claimed {
            Some(package) => {
                // The queue may hold more, and a requeue this node makes
                // after producing may not be applied locally yet: claim
                // again at once rather than wait for the hint.
                self.force_next = true;
                self.delay = Duration::ZERO;
                Ok((ClaimOutcome::Claimed, Some(package)))
            }
            None => {
                self.force_next = false;
                if self.hinted {
                    // The replica says something is queued and the authority
                    // disagrees: replication lag, or a disabled switch the
                    // claim itself refuses. Stay at the base cadence.
                    self.delay = BASE_TICK;
                } else {
                    self.back_off();
                    self.keep_force_deadline(now);
                }
                Ok((ClaimOutcome::EmptyClaim, None))
            }
        }
    }

    /// Both halves with no gate in between: what the Store contract drives.
    pub async fn pass<S>(
        &mut self,
        store: &S,
        node_id: &str,
        now: Instant,
    ) -> Result<(ClaimOutcome, Option<OfflinePackage>), StoreError>
    where
        S: OfflineClaimStore + ?Sized,
    {
        if !self.should_claim(store, node_id, now).await {
            return Ok((ClaimOutcome::SkippedHint, None));
        }
        self.claim(store, node_id, now).await
    }

    /// Never sleep past the forced claim: a package only the authority can
    /// see is claimed within [`HINT_FORCE_INTERVAL`], not that plus an idle
    /// tick.
    fn keep_force_deadline(&mut self, now: Instant) {
        if let Some(at) = self.last_claim {
            let remaining = (at + HINT_FORCE_INTERVAL).saturating_duration_since(now);
            if !remaining.is_zero() {
                self.delay = self.delay.min(remaining);
            }
        }
    }

    fn back_off(&mut self) {
        self.delay = self
            .delay
            .max(BASE_TICK / 2)
            .saturating_mul(2)
            .min(IDLE_TICK_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A store whose hint is scripted and whose claims are counted.
    #[derive(Default)]
    struct Scripted {
        hint: AtomicBool,
        claims: AtomicUsize,
    }

    #[async_trait]
    impl OfflineClaimStore for Scripted {
        async fn queue_hint(&self, _: &str) -> Result<bool, StoreError> {
            Ok(self.hint.load(Ordering::SeqCst))
        }
        async fn claim(&self, _: &str) -> Result<Option<OfflinePackage>, StoreError> {
            self.claims.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }

    /// Drive the policy for `seconds` of virtual time the way the daemon
    /// does: pass, then sleep the policy's delay.
    async fn run(policy: &mut OfflineClaimPolicy, store: &Scripted, start: Instant, seconds: u64) {
        let end = start + Duration::from_secs(seconds);
        let mut now = start;
        while now < end {
            let _ = policy.pass(store, "node", now).await;
            now += policy.delay().max(Duration::from_millis(1));
        }
    }

    #[tokio::test]
    async fn an_idle_queue_claims_only_on_the_forced_interval() {
        let store = Scripted::default();
        let mut policy = OfflineClaimPolicy::new();
        run(&mut policy, &store, Instant::now(), 600).await;
        // 0 s, then every 30 s up to 570 s: 20 claims in ten minutes where
        // the fixed 2 s poll made 300.
        assert_eq!(store.claims.load(Ordering::SeqCst), 20);
        assert_eq!(policy.delay(), IDLE_TICK_MAX);
    }

    #[tokio::test]
    async fn a_silent_hint_is_overruled_within_the_forced_interval() {
        let store = Scripted::default();
        let mut policy = OfflineClaimPolicy::new();
        let start = Instant::now();
        let _ = policy.pass(&store, "node", start).await;
        let mut now = start;
        let mut claimed_at = None;
        while now < start + Duration::from_secs(120) {
            now += policy.delay();
            let before = store.claims.load(Ordering::SeqCst);
            let _ = policy.pass(&store, "node", now).await;
            if store.claims.load(Ordering::SeqCst) > before {
                claimed_at = Some(now - start);
                break;
            }
        }
        assert_eq!(claimed_at, Some(HINT_FORCE_INTERVAL));
    }

    #[tokio::test]
    async fn a_hint_or_a_wake_claims_on_the_next_pass() {
        let store = Scripted::default();
        let mut policy = OfflineClaimPolicy::new();
        let start = Instant::now();
        run(&mut policy, &store, start, 20).await;
        let claims = store.claims.load(Ordering::SeqCst);
        policy.wake();
        assert_eq!(policy.delay(), Duration::ZERO);
        let _ = policy
            .pass(&store, "node", start + Duration::from_secs(21))
            .await;
        assert_eq!(store.claims.load(Ordering::SeqCst), claims + 1, "woken");
        store.hint.store(true, Ordering::SeqCst);
        let _ = policy
            .pass(&store, "node", start + Duration::from_secs(22))
            .await;
        assert_eq!(store.claims.load(Ordering::SeqCst), claims + 2, "hinted");
        assert_eq!(policy.delay(), BASE_TICK, "lag keeps the base cadence");
    }

    #[tokio::test]
    async fn a_gate_refusal_backs_off_like_an_empty_claim() {
        let store = Scripted::default();
        let mut policy = OfflineClaimPolicy::new();
        let start = Instant::now();
        assert!(policy.should_claim(&store, "node", start).await);
        policy.gated(start);
        assert_eq!(policy.delay(), BASE_TICK);
        assert!(!policy.should_claim(&store, "node", start + BASE_TICK).await);
        assert_eq!(store.claims.load(Ordering::SeqCst), 0);
    }
}
