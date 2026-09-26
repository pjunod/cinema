//! The offline expiry sweep's schedule (K-10 M4).
//!
//! Every node used to run [`OfflinePackageStore::expire_offline_packages`]
//! once a minute: a two-statement replicated `txn`, so a Raft proposal
//! whether or not any package had lapsed. Four nodes × 1,440 minutes is
//! ≈ 5,800 proposals a day on a cluster whose offline packages live for
//! days (docs/cluster/REPLICATED-WRITE-RATE-HYGIENE-II.md §3.3, §5.4).
//!
//! This module decides, one pass at a time, whether the authority is worth
//! asking. It owns no timer and no task, so the daemon's loop and the
//! three-voter Store contract drive the same code and count the same writes.
//!
//! The local read is a hint and nothing more. It asks the node's own replica
//! whether any package has `expires_at <= now` — exactly the predicate of the
//! sweep's `DELETE`, which is the only statement that can match a pin or a
//! package — and the sweep still runs on the authority with that same
//! predicate. So the hint can make the sweep run for nothing (a replica that
//! has not applied a renewal yet) but can never delete a package early: the
//! authority's row decides. A hint that keeps saying "nothing" is overruled
//! at least once every [`FORCED_SWEEP_INTERVAL`], so a replica that stopped
//! applying cannot keep a lapsed package alive for longer than that.
//!
//! [`OfflinePackageStore::expire_offline_packages`]: super::OfflinePackageStore::expire_offline_packages

use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::Store;
use crate::error::StoreError;

/// How often a pass runs: the sweep's old fixed cadence, so the bound on a
/// lapsed package the replica shows is unchanged.
pub const SWEEP_TICK: Duration = Duration::from_secs(60);
/// The longest a hint that keeps saying "nothing" may suppress the sweep.
pub const FORCED_SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// What one pass did. The daemon exports these as the three fixed values of
/// `plurx_offline_expiry_ticks_total{outcome}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepOutcome {
    /// The local hint saw nothing lapsed and the forced sweep was not owed.
    SkippedHint,
    /// The replicated sweep ran and expired at least one package.
    Swept,
    /// The replicated sweep ran and expired nothing.
    EmptySweep,
}

impl SweepOutcome {
    pub const ALL: [Self; 3] = [Self::SkippedHint, Self::Swept, Self::EmptySweep];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkippedHint => "skipped_hint",
            Self::Swept => "swept",
            Self::EmptySweep => "empty_sweep",
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::SkippedHint => 0,
            Self::Swept => 1,
            Self::EmptySweep => 2,
        }
    }
}

/// The two Store calls one pass can make, named by what they cost.
#[async_trait]
pub trait OfflineExpiryStore: Send + Sync {
    /// One local, non-consensus read. A hint, never an authorization.
    async fn expiry_hint(&self, now_unix: i64) -> Result<bool, StoreError>;
    /// The replicated sweep: one Raft proposal whether or not anything lapsed.
    async fn expire(&self, now_unix: i64) -> Result<u64, StoreError>;
}

#[async_trait]
impl<T: Store + ?Sized> OfflineExpiryStore for T {
    async fn expiry_hint(&self, now_unix: i64) -> Result<bool, StoreError> {
        self.offline_expiry_hint(now_unix).await
    }

    async fn expire(&self, now_unix: i64) -> Result<u64, StoreError> {
        self.expire_offline_packages(now_unix).await
    }
}

/// Per-process sweep state. The monotonic `Instant` is passed in so the loop
/// can use Tokio's clock and the tests can step a virtual one; `now_unix` is
/// the wall clock both the hint and the sweep compare `expires_at` against.
#[derive(Debug, Default)]
pub struct OfflineExpiryPolicy {
    last_sweep: Option<Instant>,
}

impl OfflineExpiryPolicy {
    /// A fresh process sweeps on its first pass, as the old loop did.
    #[must_use]
    pub fn new() -> Self {
        Self { last_sweep: None }
    }

    /// One pass: sweep when the forced sweep is owed or the local replica
    /// shows a lapsed package, otherwise read nothing on the authority.
    /// Returns what the pass did and how many packages the sweep expired.
    pub async fn pass<S>(
        &mut self,
        store: &S,
        now_unix: i64,
        now: Instant,
    ) -> Result<(SweepOutcome, u64), StoreError>
    where
        S: OfflineExpiryStore + ?Sized,
    {
        let forced = self
            .last_sweep
            .is_none_or(|at| now.saturating_duration_since(at) >= FORCED_SWEEP_INTERVAL);
        // A forced sweep does not need the hint's answer. A failed hint is
        // "something may have lapsed": the hint only ever saves work, it
        // must never be the reason a package outlives its expiry.
        if !forced && !store.expiry_hint(now_unix).await.unwrap_or(true) {
            return Ok((SweepOutcome::SkippedHint, 0));
        }
        // A failed sweep leaves `last_sweep` alone, so an owed forced sweep
        // stays owed and a hinted one is retried on the next pass.
        let expired = store.expire(now_unix).await?;
        self.last_sweep = Some(now);
        let outcome = if expired > 0 {
            SweepOutcome::Swept
        } else {
            SweepOutcome::EmptySweep
        };
        Ok((outcome, expired))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A store whose hint is scripted and whose sweeps are counted.
    #[derive(Default)]
    struct Scripted {
        hint: AtomicBool,
        fail_hint: AtomicBool,
        hints: AtomicUsize,
        sweeps: AtomicUsize,
    }

    #[async_trait]
    impl OfflineExpiryStore for Scripted {
        async fn expiry_hint(&self, _: i64) -> Result<bool, StoreError> {
            self.hints.fetch_add(1, Ordering::SeqCst);
            if self.fail_hint.load(Ordering::SeqCst) {
                return Err(StoreError::Database("injected hint failure".to_owned()));
            }
            Ok(self.hint.load(Ordering::SeqCst))
        }
        async fn expire(&self, _: i64) -> Result<u64, StoreError> {
            self.sweeps.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
    }

    /// Drive the policy for `ticks` passes at the daemon's cadence.
    async fn run(policy: &mut OfflineExpiryPolicy, store: &Scripted, start: Instant, ticks: u32) {
        for tick in 0..ticks {
            let _ = policy.pass(store, 0, start + SWEEP_TICK * tick).await;
        }
    }

    #[tokio::test]
    async fn an_idle_cluster_sweeps_only_on_the_forced_interval() {
        let store = Scripted::default();
        let mut policy = OfflineExpiryPolicy::new();
        // One hour of passes: forced at 0, 10, 20, 30, 40 and 50 minutes,
        // where the fixed loop swept 60 times.
        run(&mut policy, &store, Instant::now(), 60).await;
        assert_eq!(store.sweeps.load(Ordering::SeqCst), 6);
        assert_eq!(
            store.hints.load(Ordering::SeqCst),
            54,
            "no hint on a forced pass"
        );
    }

    #[tokio::test]
    async fn a_hint_sweeps_on_the_same_pass() {
        let store = Scripted::default();
        let mut policy = OfflineExpiryPolicy::new();
        let start = Instant::now();
        run(&mut policy, &store, start, 3).await;
        assert_eq!(store.sweeps.load(Ordering::SeqCst), 1);
        store.hint.store(true, Ordering::SeqCst);
        let (outcome, _) = policy
            .pass(&store, 0, start + SWEEP_TICK * 3)
            .await
            .expect("pass");
        assert_eq!(outcome, SweepOutcome::EmptySweep);
        assert_eq!(store.sweeps.load(Ordering::SeqCst), 2, "hinted");
    }

    #[tokio::test]
    async fn a_silent_hint_is_overruled_at_exactly_the_forced_interval() {
        let store = Scripted::default();
        let mut policy = OfflineExpiryPolicy::new();
        let start = Instant::now();
        let _ = policy.pass(&store, 0, start).await;
        let before = FORCED_SWEEP_INTERVAL - Duration::from_millis(1);
        let (skipped, _) = policy.pass(&store, 0, start + before).await.expect("pass");
        assert_eq!(skipped, SweepOutcome::SkippedHint);
        let (forced, _) = policy
            .pass(&store, 0, start + FORCED_SWEEP_INTERVAL)
            .await
            .expect("pass");
        assert_eq!(forced, SweepOutcome::EmptySweep);
        assert_eq!(store.sweeps.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failed_hint_sweeps_rather_than_skips() {
        let store = Scripted::default();
        let mut policy = OfflineExpiryPolicy::new();
        let start = Instant::now();
        let _ = policy.pass(&store, 0, start).await;
        store.fail_hint.store(true, Ordering::SeqCst);
        let _ = policy.pass(&store, 0, start + SWEEP_TICK).await;
        assert_eq!(store.sweeps.load(Ordering::SeqCst), 2);
    }
}
