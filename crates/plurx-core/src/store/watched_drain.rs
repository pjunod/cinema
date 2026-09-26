//! The watched outbox drain's tick policy (K-03 §3.1–§3.3).
//!
//! The drain used to run the replicated claim `UPDATE watched_outbox …
//! RETURNING` once a second on every voter. That statement is a Raft proposal
//! whether or not a row matches, so an empty outbox cost three proposals a
//! second on a three-voter cluster — measured at 28.6% of every replicated
//! log entry on the idle fleet (docs/cluster/REPLICATED-WRITE-RATE-HYGIENE-M0.md).
//!
//! This module decides, one pass at a time, whether the authority is worth
//! asking. It owns no timer and no task, so the daemon's loop and the
//! three-voter Store contract drive the same code and count the same writes.
//!
//! What it never does is decide *which* row is delivered. The local outbox
//! read is a hint in both directions: `false` can be a follower that has not
//! applied a recent enqueue yet, `true` can be a row a peer has since
//! settled. Only [`WatchedOutboxStore::due_watched`]'s replicated claim
//! selects rows, so two voters whose hints both fire still admit one claimant
//! per row, and a hint that says nothing is overruled at least once every
//! [`HINT_FORCE_INTERVAL`] so a replica that stopped applying cannot silence
//! the outbox.
//!
//! [`WatchedOutboxStore::due_watched`]: super::WatchedOutboxStore::due_watched

use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{keys, OutboxEntry, Store};
use crate::error::StoreError;

/// Cadence while there is, or may be, work.
pub const BASE_TICK: Duration = Duration::from_secs(1);
/// Ceiling of the idle backoff (coordinator decision, 2026-09-20).
pub const IDLE_TICK_MAX: Duration = Duration::from_secs(10);
/// The longest a hint that keeps saying "nothing" may suppress the real
/// claim (coordinator decision, 2026-09-20).
pub const HINT_FORCE_INTERVAL: Duration = Duration::from_secs(30);
/// How stale the cached Curator URL and key may become before the pair is
/// read again. A Curator configured on another node is seen within this
/// bound; a write on this node is seen at once through
/// [`WatchedDrain::settings_changed`].
pub const SETTINGS_REFRESH: Duration = Duration::from_secs(60);

/// Why one pass did or did not claim. The daemon exports these as the five
/// fixed values of `plurx_watched_outbox_ticks_total{outcome}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickOutcome {
    /// The local hint saw nothing due and the forced claim was not owed.
    SkippedHint,
    /// No Curator URL or API key and nothing visibly waiting: no claim.
    /// (Rows that *are* waiting are still claimed and failed permanently,
    /// as before, once the authority confirms the Curator is unconfigured.)
    SkippedUnconfigured,
    /// The replicated claim returned rows.
    Claimed,
    /// The replicated claim ran and returned nothing.
    EmptyClaim,
    /// Another node holds the drain's singleton lease.
    NotOwner,
}

impl TickOutcome {
    pub const ALL: [Self; 5] = [
        Self::SkippedHint,
        Self::SkippedUnconfigured,
        Self::Claimed,
        Self::EmptyClaim,
        Self::NotOwner,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkippedHint => "skipped_hint",
            Self::SkippedUnconfigured => "skipped_unconfigured",
            Self::Claimed => "claimed",
            Self::EmptyClaim => "empty_claim",
            Self::NotOwner => "not_owner",
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::SkippedHint => 0,
            Self::SkippedUnconfigured => 1,
            Self::Claimed => 2,
            Self::EmptyClaim => 3,
            Self::NotOwner => 4,
        }
    }
}

/// The Curator endpoint the drain delivers to, as last read from settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CuratorTarget {
    pub url: String,
    pub key: String,
}

impl CuratorTarget {
    #[must_use]
    pub fn configured(&self) -> bool {
        !self.url.is_empty() && !self.key.is_empty()
    }
}

/// The three Store calls one pass can make, named by what they cost.
#[async_trait]
pub trait WatchedDrainStore: Send + Sync {
    /// One authority read of the Curator URL and key.
    async fn curator_target(&self) -> Result<CuratorTarget, StoreError>;
    /// One local, non-consensus read. A hint, never an authorization.
    async fn outbox_hint(&self) -> Result<bool, StoreError>;
    /// The replicated claim: one Raft proposal whether or not a row matches.
    async fn claim_due(&self, limit: i64) -> Result<Vec<OutboxEntry>, StoreError>;
}

#[async_trait]
impl<T: Store + ?Sized> WatchedDrainStore for T {
    async fn curator_target(&self) -> Result<CuratorTarget, StoreError> {
        let (url, key) = self
            .get_setting_pair(keys::MONARR_URL, keys::MONARR_API_KEY)
            .await?;
        Ok(CuratorTarget {
            url: url.unwrap_or_default(),
            key: key.unwrap_or_default(),
        })
    }

    async fn outbox_hint(&self) -> Result<bool, StoreError> {
        self.watched_outbox_hint().await
    }

    async fn claim_due(&self, limit: i64) -> Result<Vec<OutboxEntry>, StoreError> {
        self.due_watched(limit).await
    }
}

/// What one pass decided, with the rows it claimed and the target they are
/// to be delivered to.
#[derive(Debug)]
pub struct DrainPass {
    pub outcome: TickOutcome,
    pub rows: Vec<OutboxEntry>,
    pub target: CuratorTarget,
}

/// Per-process drain state. Monotonic `Instant`s are passed in so the loop
/// can use Tokio's clock and the tests can step a virtual one.
#[derive(Debug)]
pub struct WatchedDrain {
    target: Option<(CuratorTarget, Instant)>,
    last_claim: Option<Instant>,
    /// Claim on the next pass whatever the hint says.
    force_next: bool,
    /// A local enqueue is waiting: the one reason, besides the hint, to
    /// claim while the Curator looks unconfigured.
    woken: bool,
    delay: Duration,
}

impl Default for WatchedDrain {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchedDrain {
    #[must_use]
    pub fn new() -> Self {
        Self {
            target: None,
            last_claim: None,
            // A fresh drain (a restart, a new lease owner) asks the authority
            // once before it trusts any hint.
            force_next: true,
            woken: false,
            delay: BASE_TICK,
        }
    }

    /// How long the loop should wait before the next pass.
    #[must_use]
    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// This node enqueued a row. The write returned from the authority but
    /// may not be applied to the local replica yet, so the next pass claims
    /// whatever the hint says, and runs at the base cadence.
    pub fn wake(&mut self) {
        self.force_next = true;
        self.woken = true;
        self.delay = BASE_TICK;
    }

    /// A settings write on this node touched the Curator URL or key: read
    /// the pair again on the next pass instead of waiting out
    /// [`SETTINGS_REFRESH`].
    pub fn settings_changed(&mut self) {
        self.target = None;
        self.force_next = true;
        self.delay = BASE_TICK;
    }

    /// Run one pass. Returns the claimed rows; delivering and settling them
    /// is the caller's job and is unchanged by this policy.
    pub async fn pass<S>(
        &mut self,
        store: &S,
        now: Instant,
        limit: i64,
    ) -> Result<DrainPass, StoreError>
    where
        S: WatchedDrainStore + ?Sized,
    {
        let (mut target, fresh) = self.target(store, now).await?;
        // A failed hint is treated as "something may be due": the hint only
        // ever saves work, it must never be the reason a row waits.
        let hint = store.outbox_hint().await.unwrap_or(true);
        if !target.configured() {
            if !hint && !self.woken {
                // Nothing can be delivered and nothing visibly waits: no
                // claim, and no forced claim either.
                self.back_off();
                return Ok(DrainPass {
                    outcome: TickOutcome::SkippedUnconfigured,
                    rows: Vec::new(),
                    target,
                });
            }
            // Rows may be waiting on a Curator this node believes is
            // unconfigured. Confirm that on the authority before acting on
            // it, then claim exactly as before: a still-unconfigured Curator
            // fails them permanently ("monarr is not configured"), a Curator
            // configured elsewhere since the cached read receives them.
            if !fresh {
                self.target = None;
                target = self.target(store, now).await?.0;
            }
        }
        let forced = self.force_next
            || self
                .last_claim
                .is_none_or(|at| now.saturating_duration_since(at) >= HINT_FORCE_INTERVAL);
        if !hint && !forced {
            self.back_off();
            self.keep_force_deadline(now);
            return Ok(DrainPass {
                outcome: TickOutcome::SkippedHint,
                rows: Vec::new(),
                target,
            });
        }
        let rows = store.claim_due(limit).await?;
        self.force_next = false;
        self.woken = false;
        self.last_claim = Some(now);
        let outcome = if rows.is_empty() {
            if hint {
                // The replica says something is due and the authority
                // disagrees: replication lag. Stay at the base cadence.
                self.delay = BASE_TICK;
            } else {
                self.back_off();
                self.keep_force_deadline(now);
            }
            TickOutcome::EmptyClaim
        } else {
            self.delay = BASE_TICK;
            TickOutcome::Claimed
        };
        Ok(DrainPass {
            outcome,
            rows,
            target,
        })
    }

    /// The cached target, re-read when older than [`SETTINGS_REFRESH`].
    /// The flag says whether this call read the authority.
    async fn target<S>(
        &mut self,
        store: &S,
        now: Instant,
    ) -> Result<(CuratorTarget, bool), StoreError>
    where
        S: WatchedDrainStore + ?Sized,
    {
        if let Some((target, read_at)) = &self.target {
            if now.saturating_duration_since(*read_at) < SETTINGS_REFRESH {
                return Ok((target.clone(), false));
            }
        }
        match store.curator_target().await {
            Ok(target) => {
                self.target = Some((target.clone(), now));
                Ok((target, true))
            }
            // Keep delivering to the last known endpoint through a transient
            // settings-read failure; the stale timestamp retries next pass.
            Err(error) => match &self.target {
                Some((target, _)) => Ok((target.clone(), false)),
                None => Err(error),
            },
        }
    }

    /// Never sleep past the forced claim: a row only the authority can see
    /// is claimed within [`HINT_FORCE_INTERVAL`], not that plus an idle tick.
    fn keep_force_deadline(&mut self, now: Instant) {
        if let Some(at) = self.last_claim {
            let remaining = (at + HINT_FORCE_INTERVAL).saturating_duration_since(now);
            if !remaining.is_zero() {
                self.delay = self.delay.min(remaining);
            }
        }
    }

    fn back_off(&mut self) {
        self.delay = self.delay.saturating_mul(2).min(IDLE_TICK_MAX);
    }
}
