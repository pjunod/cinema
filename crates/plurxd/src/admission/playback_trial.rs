//! Protected playback admission for the node's shared finite trial lane.
use super::{Admissions, PermitOwner, PermitState, SwPermit};
use plurx_core::store::{DvrTrialExclusion, DvrTrialInvalidation};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::Instant,
};
use tokio::sync::Notify;

tokio::task_local! {
    // The origin considers a trial only in the actual ranked local opportunity.
    // The private finite permit and atomic affected-lane owner decide admission.
    pub(crate) static CONSIDER_LOCAL_PLAYBACK_TRIAL: Option<crate::live_tv::LocalDvrTrialAuthority>;
}

#[derive(Debug, Default)]
pub(super) struct PlaybackTrialSignal {
    cancelled: AtomicBool,
    cancel: Notify,
    released: AtomicBool,
    changed: Notify,
    store_invalidation: DvrTrialInvalidation,
}

impl PlaybackTrialSignal {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancel.notify_one();
    }
}

/// Single original driver waits on this signal. The shared allocation is
/// prepaid in the finite application's own metadata projection.
#[derive(Debug, Clone)]
pub(crate) struct PlaybackTrialStop(Arc<PlaybackTrialSignal>);

impl PlaybackTrialStop {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire) || self.0.store_invalidation.moved()
    }

    pub(crate) async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::select! {
                () = self.0.cancel.notified() => {},
                () = self.0.store_invalidation.changed() => return,
            }
        }
    }
}

/// The permit book keeps the exclusion with the exact owner, including an
/// original lane whose physical closure becomes unknown on Drop.
#[derive(Debug)]
pub(super) struct PlaybackTrialRegistration {
    signal: Arc<PlaybackTrialSignal>,
    exclusion: DvrTrialExclusion,
}

impl PermitState {
    fn advance_playback_epoch(&self) {
        let current = self.playback_epoch.load(Ordering::Relaxed);
        self.playback_epoch
            .store(current.saturating_add(1), Ordering::Release);
    }

    fn idle_with(&self, exact: &SwPermit) -> bool {
        exact.owner == PermitOwner::Live
            && exact.family_id().is_none()
            && self.protected_playback == 0
            && self.playback_trial.is_none()
            && self.live_waiting == 0
            && self.hardware_used() == 0
            && self.software_live_permits == 1
            && self.software_live_used == exact.weight
            && !self.background_active()
            && !self.has_reservation()
    }

    pub(super) fn blocks_playback_trial(&self, foreground: bool) -> bool {
        self.advance_playback_epoch();
        let Some(trial) = self.playback_trial.as_ref() else {
            return false;
        };
        if foreground {
            trial.signal.cancel();
        }
        true
    }
}

/// CPU/source/transport protection for an existing real owner. It does not
/// allocate hardware or software encoder slots. Shared transport keeps one
/// token for its original owner, irrespective of reader count.
#[derive(Debug)]
pub(crate) struct ProtectedPlaybackLane {
    permits: Arc<Mutex<PermitState>>,
    physical_possible: bool,
}

impl ProtectedPlaybackLane {
    pub(crate) fn arm_physical(&mut self) {
        self.physical_possible = true;
    }

    pub(crate) fn known_finished(mut self) {
        self.physical_possible = false;
    }
}

impl Drop for ProtectedPlaybackLane {
    fn drop(&mut self) {
        if self.physical_possible {
            return;
        }
        let mut permits = self.permits.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(permits.protected_playback > 0, "protected playback owner");
        permits.protected_playback -= 1;
    }
}

/// This local generation is captured before reading persisted capture
/// obligations and consumed under the same admission lock as trial entry.
pub(crate) struct CaptureTrialEpoch {
    permits: Arc<Mutex<PermitState>>,
    observed: u64,
}

pub(crate) struct CaptureTrialProof {
    epoch: CaptureTrialEpoch,
    exclusion: DvrTrialExclusion,
}

impl CaptureTrialEpoch {
    pub(crate) fn with_exclusion(self, exclusion: DvrTrialExclusion) -> CaptureTrialProof {
        CaptureTrialProof {
            epoch: self,
            exclusion,
        }
    }
}

impl Admissions {
    pub(crate) fn capture_trial_epoch(&self) -> Option<CaptureTrialEpoch> {
        let permits = self.permits.lock().ok()?;
        let observed = permits.playback_epoch.load(Ordering::Acquire);
        (observed != u64::MAX
            && permits.protected_playback == 0
            && permits.playback_trial.is_none())
        .then(|| CaptureTrialEpoch {
            permits: Arc::clone(&self.permits),
            observed,
        })
    }

    /// Saving a prospective rule does not await or promise a capture. Its
    /// original write still advances the same generation and blocks new
    /// speculation while actual materialization waits for protected entry.
    pub(crate) fn begin_capture_intent_update(&self) -> Option<ProtectedPlaybackLane> {
        let mut permits = self.permits.lock().ok()?;
        let _ = permits.blocks_playback_trial(true);
        self.register_protected_lane(&mut permits).ok()
    }

    pub(crate) fn try_acquire_protected_playback_lane(
        &self,
    ) -> Result<ProtectedPlaybackLane, &'static str> {
        let mut permits = self
            .permits
            .lock()
            .map_err(|_| "playback admission unavailable")?;
        if permits.blocks_playback_trial(true) {
            return Err("the original finite trial is yielding to protected playback");
        }
        self.register_protected_lane(&mut permits)
    }

    fn register_protected_lane(
        &self,
        permits: &mut PermitState,
    ) -> Result<ProtectedPlaybackLane, &'static str> {
        permits.protected_playback = permits
            .protected_playback
            .checked_add(1)
            .ok_or("protected playback owner count exhausted")?;
        permits.advance_playback_epoch();
        Ok(ProtectedPlaybackLane {
            permits: Arc::clone(&self.permits),
            physical_possible: false,
        })
    }

    pub(crate) async fn acquire_protected_playback_lane(
        &self,
        deadline: Instant,
    ) -> Result<ProtectedPlaybackLane, &'static str> {
        let (lane, trial) = {
            let mut permits = self
                .permits
                .lock()
                .map_err(|_| "playback admission unavailable")?;
            let trial = permits.playback_trial.as_ref().map(|trial| {
                trial.signal.cancel();
                Arc::clone(&trial.signal)
            });
            if Instant::now() >= deadline {
                return Err("protected playback admission deadline elapsed");
            }
            // Reserve the same protected lane before waiting. Settlement of
            // the original trial cannot admit another trial ahead of us.
            (self.register_protected_lane(&mut permits)?, trial)
        };
        if let Some(trial) = trial {
            let changed = trial.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !trial.released.load(Ordering::Acquire) {
                tokio::time::timeout_at(deadline.into(), changed)
                    .await
                    .map_err(|_| "original playback trial still owns cleanup at deadline")?;
            }
        }
        if Instant::now() >= deadline {
            return Err("protected playback admission deadline elapsed");
        }
        Ok(lane)
    }
}

/// Selection is atomic with all incumbent registrations and checks the
/// actual original finite worker's compatibility reservation. The shared
/// lane never authorizes a new native process by itself.
#[derive(Debug)]
pub(crate) struct PlaybackTrialLane {
    permits: Arc<Mutex<PermitState>>,
    stop: PlaybackTrialStop,
    native_possible: bool,
}

impl SwPermit {
    pub(crate) fn idle_qualification_window(
        &self,
    ) -> Option<plurx_core::adaptive_resources::runtime::finite_vod::FiniteQualificationWindow>
    {
        let permits = self.permits.lock().ok()?;
        permits.idle_with(self).then(|| {
            plurx_core::adaptive_resources::runtime::finite_vod::FiniteQualificationWindow::capture(
                Arc::clone(&permits.playback_epoch),
            )
        })
    }

    pub(crate) fn try_acquire_playback_trial_lane(
        &self,
        capture: CaptureTrialProof,
    ) -> Option<PlaybackTrialLane> {
        let CaptureTrialProof { epoch, exclusion } = capture;
        let mut permits = self.permits.lock().ok()?;
        if !Arc::ptr_eq(&epoch.permits, &self.permits)
            || epoch.observed != permits.playback_epoch.load(Ordering::Acquire)
            || !permits.idle_with(self)
            || !exclusion.is_current()
        {
            return None;
        }
        permits.advance_playback_epoch();
        let signal = Arc::new(PlaybackTrialSignal {
            store_invalidation: exclusion.invalidation(),
            ..PlaybackTrialSignal::default()
        });
        let stop = PlaybackTrialStop(Arc::clone(&signal));
        permits.playback_trial = Some(PlaybackTrialRegistration { signal, exclusion });
        Some(PlaybackTrialLane {
            permits: Arc::clone(&self.permits),
            stop,
            native_possible: false,
        })
    }
}

impl PlaybackTrialLane {
    pub(crate) fn stop_token(&self) -> PlaybackTrialStop {
        self.stop.clone()
    }

    pub(crate) fn requested_heap_bytes() -> Option<usize> {
        std::mem::size_of::<PlaybackTrialSignal>()
            .checked_add(2 * std::mem::size_of::<usize>())?
            .checked_add(DvrTrialInvalidation::requested_heap_bytes()?)
    }

    pub(crate) fn begin_adoption(&self, claim: impl FnOnce() -> bool) -> bool {
        let Ok(permits) = self.permits.lock() else {
            return false;
        };
        permits.playback_trial.as_ref().is_some_and(|trial| {
            Arc::ptr_eq(&trial.signal, &self.stop.0) && trial.exclusion.is_current()
        }) && !self.stop.is_cancelled()
            && claim()
    }

    pub(crate) fn arm_native(&mut self) {
        self.native_possible = true;
    }

    /// The same caller has either proved all original physical cleanup, or
    /// promoted the original surviving hard charge to an ordinary owner.
    pub(crate) fn known_finished(mut self) {
        self.native_possible = false;
    }

    fn release(&self) {
        let mut permits = self.permits.lock().unwrap_or_else(PoisonError::into_inner);
        if permits
            .playback_trial
            .as_ref()
            .is_some_and(|trial| Arc::ptr_eq(&trial.signal, &self.stop.0))
        {
            permits.playback_trial.take();
            self.stop.0.released.store(true, Ordering::Release);
            self.stop.0.changed.notify_waiters();
        }
    }
}

impl Drop for PlaybackTrialLane {
    fn drop(&mut self) {
        if !self.native_possible {
            self.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::Priority;
    use plurx_core::store::{DvrStore, SqliteStore};
    use std::time::Duration;

    async fn proven_capture_epoch(
        admissions: &Admissions,
        store: &SqliteStore,
    ) -> Option<CaptureTrialProof> {
        let epoch = admissions.capture_trial_epoch()?;
        let exclusion = store
            .local_dvr_trial_exclusion(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .expect("durable capture snapshot")?;
        Some(epoch.with_exclusion(exclusion))
    }

    #[tokio::test]
    async fn finished_capture_commit_cannot_disappear_from_an_earlier_trial_epoch() {
        let admissions = Admissions::new();
        let store = SqliteStore::open_in_memory().expect("local capture store");
        let hold = admissions
            .try_admit_software(2, 1, Priority::Live)
            .expect("original producer");
        let observed = proven_capture_epoch(&admissions, &store)
            .await
            .expect("original idle epoch");
        let intent = admissions
            .begin_capture_intent_update()
            .expect("new desired rule");
        assert!(admissions.capture_trial_epoch().is_none());
        intent.known_finished();
        assert!(hold.try_acquire_playback_trial_lane(observed).is_none());
        let snapshot = proven_capture_epoch(&admissions, &store)
            .await
            .expect("next exact local epoch");
        assert!(hold.try_acquire_playback_trial_lane(snapshot).is_some());
    }

    #[tokio::test]
    async fn protected_lane_preempts_and_waits_for_original_trial_release() {
        let admissions = Admissions::new();
        let store = SqliteStore::open_in_memory().expect("local capture store");
        let hold = admissions
            .try_admit_software(2, 1, Priority::Live)
            .expect("original producer");
        let acquire_trial = || async {
            let proof = proven_capture_epoch(&admissions, &store).await?;
            hold.try_acquire_playback_trial_lane(proof)
        };
        let mut trial = acquire_trial().await.expect("actual idle lane");
        trial.arm_native();
        let incoming =
            admissions.acquire_protected_playback_lane(Instant::now() + Duration::from_secs(1));
        tokio::pin!(incoming);
        assert!(futures_util::poll!(&mut incoming).is_pending());
        assert!(trial.stop_token().is_cancelled());
        assert!(acquire_trial().await.is_none());
        trial.known_finished();
        assert!(
            acquire_trial().await.is_none(),
            "new trials cannot overtake a registered protected waiter"
        );
        let mut owner = incoming
            .await
            .expect("original trial settlement released the lane");
        owner.arm_physical();
        assert!(acquire_trial().await.is_none());
        owner.known_finished();

        let mut uncertain = admissions
            .acquire_protected_playback_lane(Instant::now() + Duration::from_secs(1))
            .await
            .expect("another exact owner");
        uncertain.arm_physical();
        drop(uncertain);
        assert!(
            acquire_trial().await.is_none(),
            "lost physical custody remains held"
        );
    }
}
