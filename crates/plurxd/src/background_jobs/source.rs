//! One finite source-byte spend ledger shared by every helper of an owner.
//! Raft allocates epoch receipts; individual reads spend them locally. A
//! replayed receipt never resets its remaining allowance.
use plurx_core::store::{
    background_jobs_resources::{BackgroundSourceOwner, BackgroundSourceServiceGrant},
    Store,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// `source.rs` is itself a #[path] module, so children resolve beside it.
#[path = "source/completion.rs"]
mod completion;
pub(crate) use completion::{Holder as SourceHolder, Operation as SourceOperation, RawRetirement};

struct Spent {
    bytes: u64,
    retain_until_ms: i64,
}
static SPENT: LazyLock<Mutex<HashMap<String, Spent>>> = LazyLock::new(Default::default);
const MAX_SPEND_RECEIPTS: usize = 16_384;

fn reserve(
    grant: &BackgroundSourceServiceGrant,
    max: usize,
    now_ms: i64,
) -> Result<Option<usize>, String> {
    if now_ms >= grant.expires_at_ms {
        return Ok(None);
    }
    let mut receipts = SPENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    receipts.retain(|_, spent| spent.retain_until_ms > now_ms);
    // Permissions can expire and renew within one allocation epoch while the
    // SAME receipt is replayed. Keep spent credit through that entire epoch
    // (plus one epoch of clock margin), never only its temporary permission.
    let retain_until = grant
        .expires_at_ms
        .max(grant.epoch_id.saturating_add(2).saturating_mul(5000));
    if !receipts.contains_key(&grant.receipt_id) && receipts.len() >= MAX_SPEND_RECEIPTS {
        return Err("source byte receipt inventory exhausted".into());
    }
    let spent = receipts.entry(grant.receipt_id.clone()).or_insert(Spent {
        bytes: 0,
        retain_until_ms: retain_until,
    });
    let available = grant.byte_allowance.map_or(max as u64, |allowance| {
        allowance.saturating_sub(spent.bytes)
    });
    let count = (max as u64).min(available) as usize;
    if count == 0 {
        return Ok(None);
    }
    // Reserve before issuing the read. Short/cancelled reads receive no
    // refund, so lost acknowledgements cannot manufacture source credit.
    spent.bytes = spent.bytes.saturating_add(count as u64);
    spent.retain_until_ms = spent.retain_until_ms.max(retain_until);
    Ok(Some(count))
}

#[derive(Default)]
struct PacingState {
    waiters: usize,
    since: Option<tokio::time::Instant>,
    total: Duration,
}

/// One sequential original-media operation's proven finite-quota waits.
/// Physical read, mutex contention, unavailable authority and grant RPC time
/// are never entered in this clock and still consume active execution budget.
#[derive(Clone, Default)]
pub(crate) struct SourcePacingClock {
    state: Arc<Mutex<PacingState>>,
    changed: Arc<tokio::sync::Notify>,
}
impl SourcePacingClock {
    fn set_waiting(&self, entering: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entering {
            if state.waiters == 0 {
                state.since = Some(tokio::time::Instant::now());
            }
            state.waiters += 1;
        } else {
            state.waiters -= 1;
            if state.waiters == 0 {
                if let Some(since) = state.since.take() {
                    state.total = state.total.saturating_add(since.elapsed());
                }
            }
        }
        self.changed.notify_waiters();
    }
    fn elapsed(&self) -> (Duration, bool) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            state
                .total
                .saturating_add(state.since.map_or(Duration::ZERO, |since| since.elapsed())),
            state.waiters != 0,
        )
    }
    pub(crate) async fn scope<T>(&self, work: impl std::future::Future<Output = T>) -> T {
        let result = PACING_CLOCK.scope(self.clone(), work).await;
        let paced = self.elapsed().0;
        if !paced.is_zero() {
            tracing::debug!(
                source_paced_ms = paced.as_millis(),
                timing_class = "source_paced",
                "original-media hash timing includes deliberate finite service holds"
            );
        }
        result
    }
    /// Replaces the operation's existing active timeout. The finite wall-age
    /// bound remains active budget plus eighteen hours, including paced time.
    pub(crate) async fn deadline(&self, active_budget: Duration) {
        let started = tokio::time::Instant::now();
        let baseline = self.elapsed().0;
        let wall = started + active_budget.saturating_add(Duration::from_secs(18 * 60 * 60));
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (paced, waiting) = self.elapsed();
            let paced = paced.saturating_sub(baseline);
            let now = tokio::time::Instant::now();
            let deadline = (started + active_budget.saturating_add(paced)).min(wall);
            if now >= wall || now >= deadline {
                return;
            }
            tokio::select! {
                () = changed => {},
                () = tokio::time::sleep_until(if waiting {wall} else {deadline}) => {},
            }
        }
    }
}

tokio::task_local! { static PACING_CLOCK: SourcePacingClock; }

struct PacingWait<'a> {
    budget: &'a SourceReadBudget,
    clock: Option<SourcePacingClock>,
    quota: Option<plurx_core::process::bounded::SourceQuotaWait>,
}
impl Drop for PacingWait<'_> {
    fn drop(&mut self) {
        // Stop clock credit before publishing the absence of a waiter to the
        // owner; its next poll may begin a source-slot reacquisition RPC.
        drop(self.quota.take());
        let prior = self.budget.pacing_waiters.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prior > 0);
        if let Some(clock) = &self.clock {
            clock.set_waiting(false);
        }
        if self.budget.pacing_waiters() == 0 {
            self.budget.changed.notify_waiters();
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct SourcePauseBinding(Arc<SourceReadBudget>);
#[cfg(target_os = "linux")]
impl Drop for SourcePauseBinding {
    fn drop(&mut self) {
        self.0
            .pause_controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

pub(crate) struct SourceReadBudget {
    store: Arc<dyn Store>,
    owner: BackgroundSourceOwner,
    analysis_phase: Option<plurx_core::store::analysis_coordination::AnalysisPhysicalPhase>,
    grant: tokio::sync::Mutex<Option<BackgroundSourceServiceGrant>>,
    cancel: CancellationToken,
    runtime: tokio::runtime::Handle,
    invalidated: std::sync::atomic::AtomicBool,
    changed: tokio::sync::Notify,
    pacing_waiters: AtomicUsize,
    pause_controller: Mutex<Option<plurx_core::process::bounded::SourcePauseController>>,
    completion: Option<Arc<completion::Completion>>,
}
impl SourceReadBudget {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        owner: BackgroundSourceOwner,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        Self::new_scoped(store, owner, cancel, None)
    }
    /// The supervisor cancels this phase budget after its children join and
    /// before settling its exact receipt. The next physical phase gets a new
    /// budget, UUID and sequence; owner-only construction cannot obtain service.
    pub(crate) fn from_analysis_phase(
        store: Arc<dyn Store>,
        phase: plurx_core::store::analysis_coordination::AnalysisPhysicalPhase,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        Self::new_scoped(store, phase.custody.owner.clone(), cancel, Some(phase))
    }
    fn new_scoped(
        store: Arc<dyn Store>,
        owner: BackgroundSourceOwner,
        cancel: CancellationToken,
        analysis_phase: Option<plurx_core::store::analysis_coordination::AnalysisPhysicalPhase>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            owner,
            analysis_phase,
            grant: tokio::sync::Mutex::new(None),
            cancel,
            runtime: tokio::runtime::Handle::current(),
            invalidated: std::sync::atomic::AtomicBool::new(false),
            changed: tokio::sync::Notify::new(),
            pacing_waiters: AtomicUsize::new(0),
            pause_controller: Mutex::new(None),
            completion: None,
        })
    }
    pub(crate) fn with_completion(
        store: Arc<dyn Store>,
        custody: &plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken,
        cancel: CancellationToken,
        authorize: CancellationToken,
    ) -> Arc<Self> {
        let mut budget = Self::new(Arc::clone(&store), custody.owner.clone(), cancel.clone());
        // This bounded enrollment audits the Linux strict probe collector.
        // Other platform parser lifetimes retain their existing explicit join.
        if cfg!(target_os = "linux") {
            Arc::get_mut(&mut budget)
                .expect("new source budget")
                .completion = Some(completion::Completion::start(
                store,
                custody.clone(),
                cancel,
                authorize,
            ));
        }
        budget
    }
    pub(crate) fn enroll_preparation(
        &self,
        source: Arc<std::fs::File>,
    ) -> Result<Option<SourceOperation>, String> {
        self.completion
            .as_ref()
            .map(|owner| owner.enroll(source))
            .transpose()
    }
    pub(crate) fn register_source_file(&self, source: Arc<std::fs::File>) -> Result<(), String> {
        self.completion
            .as_ref()
            .map_or(Ok(()), |owner| owner.file(source))
    }
    pub(crate) fn source_holder(&self) -> Result<Option<SourceHolder>, String> {
        self.completion
            .as_ref()
            .map(|owner| owner.holder())
            .transpose()
            .map(Option::flatten)
    }
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn raw_retirement(&self) -> Result<Option<RawRetirement>, String> {
        self.completion
            .as_ref()
            .map(|owner| owner.raw())
            .transpose()
            .map(Option::flatten)
    }
    /// Call after the loss token fired. True when the live completion ledger
    /// (not the caller) settles this custody once every holder retires.
    pub(crate) fn completion_owns_settlement(&self) -> bool {
        self.completion
            .as_ref()
            .is_some_and(|owner| owner.owns_settlement())
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn completion_observation(&self) -> Option<(usize, usize, bool)> {
        self.completion.as_ref().map(|owner| owner.observation())
    }
    pub(crate) fn untracked_source_capability(&self) {
        if let Some(owner) = &self.completion {
            owner.veto();
        }
    }
    pub(super) fn job_token(&self) -> Option<&plurx_core::store::background_jobs::JobToken> {
        match &self.owner {
            BackgroundSourceOwner::Job { token } => Some(token),
            _ => None,
        }
    }

    /// The existing operation supervisor retains this exact receipt and must
    /// release it explicitly after every read/bridge joins. The budget owns no
    /// cleanup actor and never treats Drop or lease expiry as completion.
    pub(crate) fn from_existing_custody(
        store: Arc<dyn Store>,
        custody: &plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        Self::new(store, custody.owner.clone(), cancel)
    }
    #[cfg(target_os = "linux")]
    pub(crate) async fn source_may_continue(&self) -> Result<bool, String> {
        if let Some(phase) = &self.analysis_phase {
            return self
                .store
                .analysis_source_phase_current(phase, crate::state::clock_ms())
                .await
                .map_err(|error| format!("source phase observation: {error}"));
        }
        self.store
            .background_source_may_continue(self.owner.clone(), crate::state::clock_ms())
            .await
            .map_err(|error| format!("source authority observation: {error}"))
    }
    /// The caller already proved aggregate child/thread stop and daemon-read
    /// settlement. Only the exact Job fence has a replicated release/resume
    /// transition; analysis/offline/read owners keep their custody charged.
    #[cfg(target_os = "linux")]
    pub(crate) async fn pause_verified(&self) -> Result<bool, String> {
        match &self.owner {
            BackgroundSourceOwner::Job { token } => self
                .store
                .pause_background_source(token.clone(), crate::state::clock_ms())
                .await
                .map_err(|error| format!("source pause authority: {error}")),
            _ => Ok(true),
        }
    }
    #[cfg(target_os = "linux")]
    pub(crate) async fn resume_verified(&self) -> Result<bool, String> {
        match &self.owner {
            BackgroundSourceOwner::Job { token } => self
                .store
                .resume_background_source(token.clone(), crate::state::clock_ms())
                .await
                .map_err(|error| format!("source resume authority: {error}")),
            _ => self.source_may_continue().await,
        }
    }
    pub(crate) async fn wait_pressure(&self) {
        loop {
            if self.cancel.is_cancelled()
                || !self
                    .store
                    .background_source_may_continue(self.owner.clone(), crate::state::clock_ms())
                    .await
                    .unwrap_or(false)
            {
                return;
            }
            tokio::select! {
                ()=self.cancel.cancelled()=>return,
                ()=crate::source_custody::source_pressure_changed().notified()=>{},
                ()=tokio::time::sleep(Duration::from_millis(250))=>{},
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn invalidate_grant(&self) {
        self.invalidated
            .store(true, std::sync::atomic::Ordering::Release);
        self.changed.notify_waiters();
    }

    /// A finite receipt was successfully issued and exhausted. This is not
    /// an I/O stop acknowledgement: the owned controller must still verify
    /// every process thread and await actual daemon reads before releasing IO.
    pub(crate) fn pacing_waiters(&self) -> usize {
        self.pacing_waiters.load(Ordering::Acquire)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn bind_pause_controller(
        self: &Arc<Self>,
        controller: plurx_core::process::bounded::SourcePauseController,
    ) -> SourcePauseBinding {
        let mut bound = self
            .pause_controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            bound.is_none(),
            "one source pause driver owns a byte budget"
        );
        *bound = Some(controller);
        SourcePauseBinding(Arc::clone(self))
    }
    fn pacing_wait(&self) -> PacingWait<'_> {
        self.pacing_waiters.fetch_add(1, Ordering::AcqRel);
        let clock = PACING_CLOCK.try_with(Clone::clone).ok();
        let quota = self
            .pause_controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(plurx_core::process::bounded::SourcePauseController::finite_quota_wait);
        if let Some(clock) = &clock {
            clock.set_waiting(true);
        }
        self.changed.notify_waiters();
        PacingWait {
            budget: self,
            clock,
            quota,
        }
    }

    async fn claim_service(
        &self,
        now_ms: i64,
    ) -> Result<Option<BackgroundSourceServiceGrant>, plurx_core::error::StoreError> {
        match &self.analysis_phase {
            Some(phase) => {
                self.store
                    .claim_analysis_source_phase_service(phase, now_ms)
                    .await
            }
            None => {
                self.store
                    .claim_background_source_service(self.owner.clone(), now_ms)
                    .await
            }
        }
    }

    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }
    pub(crate) async fn allowance(&self, max: usize) -> Result<usize, String> {
        let operation =
            plurx_core::process::bounded::cancellation().unwrap_or_else(|| self.cancel.clone());
        self.allowance_cancellable(max, &operation).await
    }
    async fn allowance_cancellable(
        &self,
        max: usize,
        cancel: &CancellationToken,
    ) -> Result<usize, String> {
        if max == 0 {
            return Err("zero-length source service request".into());
        }
        let mut cached = tokio::select! {
            ()=cancel.cancelled()=>return Err("source read cancelled".into()),
            ()=self.cancel.cancelled()=>return Err("source owner cancelled".into()),
            cached=self.grant.lock()=>cached,
        };
        loop {
            if cancel.is_cancelled() || self.cancel.is_cancelled() {
                return Err("source read cancelled".into());
            }
            if matches!(&self.owner, BackgroundSourceOwner::Companion { .. }) {
                // Optional foreground custody is physical, but byte service
                // is revocable. Revalidate every bounded bridge range so a
                // selected reader's new Refill projection takes effect before
                // another original-media read is issued.
                *cached = None;
            }
            if self
                .invalidated
                .swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                *cached = None;
            }
            let now = crate::state::clock_ms();
            if let Some(grant) = cached.as_ref() {
                if let Some(count) = reserve(grant, max, now)? {
                    return Ok(count);
                }
                if now >= grant.expires_at_ms {
                    *cached = None;
                }
            }
            if cached.is_none() {
                *cached = tokio::select! {
                    ()=cancel.cancelled()=>return Err("source read cancelled".into()),
                    ()=self.cancel.cancelled()=>return Err("source owner cancelled".into()),
                    granted=self.claim_service(now)=>granted.map_err(|error|format!("source byte grant: {error}"))?,
                };
                if let Some(grant) = cached.as_ref() {
                    if let Some(count) = reserve(grant, max, crate::state::clock_ms())? {
                        return Ok(count);
                    }
                }
            }
            let wait = cached.as_ref().map_or(250, |grant| {
                grant
                    .expires_at_ms
                    .saturating_sub(crate::state::clock_ms())
                    .clamp(1, 250)
            });
            // Enter only after a successful finite receipt with no bytes
            // left. Drop the hold before any next Store RPC or physical read.
            // None/unlimited/error paths must consume ordinary active time.
            let _pacing = cached
                .as_ref()
                .filter(|grant| {
                    grant.byte_allowance.is_some() && crate::state::clock_ms() < grant.expires_at_ms
                })
                .map(|_| self.pacing_wait());
            tokio::select! {
                ()=cancel.cancelled()=>return Err("source read cancelled".into()),
                ()=self.cancel.cancelled()=>return Err("source owner cancelled".into()),
                ()=self.changed.notified()=>{},
                ()=crate::source_custody::source_pressure_changed().notified()=>{},
                ()=tokio::time::sleep(Duration::from_millis(wait as u64))=>{},
            }
        }
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) async fn install_test_service_grant(&self, grant: BackgroundSourceServiceGrant) {
        *self.grant.lock().await = Some(grant);
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) async fn current_reserved_test_bytes(&self) -> Option<u64> {
        let grant = self.grant.lock().await;
        let receipt = &grant.as_ref()?.receipt_id;
        SPENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(receipt)
            .map(|spent| spent.bytes)
    }

    /// Blocking sparse read workers retain their operation cancellation and
    /// use this same asynchronous allocator through the captured runtime.
    pub(crate) fn allowance_blocking(
        &self,
        max: usize,
        cancel: &CancellationToken,
    ) -> Result<usize, String> {
        self.runtime
            .block_on(self.allowance_cancellable(max, cancel))
    }
}

tokio::task_local! { static SOURCE_BUDGET:Arc<SourceReadBudget>; }
pub(crate) async fn budgeted<T>(
    budget: Arc<SourceReadBudget>,
    work: impl std::future::Future<Output = T>,
) -> T {
    SOURCE_BUDGET.scope(budget, work).await
}
pub(crate) fn source_budget() -> Option<Arc<SourceReadBudget>> {
    SOURCE_BUDGET.try_with(Arc::clone).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    async fn quota_held_scanner(retain_read_activity: bool) -> (Duration, Duration) {
        use plurx_core::{
            process::bounded,
            transcode::{EncoderCaps, Pipeline},
        };
        let (store, _job, active) = crate::background_jobs::tests::active().await;
        let fence = active.fence();
        let budget = fence.source_budget();
        let now = crate::state::clock_ms();
        // A real exhausted finite receipt: the allocator's existing timer
        // advances to a fresh Store grant while the process is verified held.
        *budget.grant.lock().await = Some(BackgroundSourceServiceGrant {
            receipt_id: uuid::Uuid::new_v4().to_string(),
            epoch_id: now / 5000,
            expires_at_ms: now + 3000,
            byte_allowance: Some(0),
            resource_keys: vec!["source_io".into()],
        });
        let directory = crate::test_tempdir().expect("create source test temp directory");
        let manager = crate::transcode::TranscodeManager::new(
            store,
            directory.path().into(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let admission = manager
            .admit_fragment()
            .await
            .expect("admit quota-pacing probe");
        let cancel = CancellationToken::new();
        let shutdown = CancellationToken::new();
        let started = tokio::time::Instant::now();
        let child_finished = Arc::new(Mutex::new(None));
        let finished = Arc::clone(&child_finished);
        let result = tokio::time::timeout(
            Duration::from_secs(12),
            crate::source_probe::run_source_admitted_probe(
                &manager,
                &admission,
                &cancel,
                &shutdown,
                None,
                &fence,
                || async { true },
                async {
                    let control =
                        bounded::source_pause_controller().expect("create source pause controller");
                    // This is the exact guard held by an outstanding daemon AVIO
                    // worker. Retaining it forbids aggregate stopped proof even
                    // when the child's own threads have accepted SIGSTOP.
                    let activity = if retain_read_activity {
                        Some(
                            control
                                .enlist(&cancel)
                                .await
                                .expect("register outstanding daemon source read"),
                        )
                    } else {
                        None
                    };
                    let child = async {
                        let result = bounded::output(
                            "/bin/sh",
                            &["-c", "exec sleep 60"],
                            Duration::from_millis(1200),
                            1024,
                            plurx_core::process::ChildWork::background(
                                "quota-held scanner regression",
                            ),
                        )
                        .await;
                        *finished.lock().expect("lock child completion timestamp") =
                            Some(started.elapsed());
                        result
                    };
                    let (child, allowance) = tokio::join!(child, budget.allowance(1));
                    drop(activity);
                    allowance.expect("source allowance resolves after the quota hold");
                    child
                },
            ),
        )
        .await
        .expect("bounded source probe finishes at its deadline");
        let elapsed = started.elapsed();
        assert_eq!(
            result
                .expect_err("probe must stop after its child deadline")
                .kind(),
            std::io::ErrorKind::TimedOut
        );
        let child_finished = child_finished
            .lock()
            .expect("lock child completion timestamp")
            .expect("child completion is recorded before probe returns");
        assert_eq!(budget.pacing_waiters(), 0);
        active.finish().await;
        (elapsed, child_finished)
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn finite_quota_held_scanner_preserves_active_budget_after_verified_stop() {
        let (elapsed, child_finished) = quota_held_scanner(false).await;
        assert!(
            elapsed >= Duration::from_millis(3500),
            "quota holds must not spend the scanner's 1200ms active budget"
        );
        assert!(child_finished >= Duration::from_millis(3500));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn outstanding_daemon_read_still_spends_active_budget_despite_quota_hold() {
        let (_elapsed, child_finished) = quota_held_scanner(true).await;
        assert!(
            child_finished < Duration::from_millis(2400),
            "a live daemon read is not aggregate source quiescence"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn finite_wait_drop_closes_confirmed_slot_credit_before_driver_poll() {
        use plurx_core::process::bounded::SourcePauseController;
        let (_store, _job, active) = crate::background_jobs::tests::active().await;
        let budget = active.fence().source_budget();
        let control = SourcePauseController::new_slot_gated(Duration::from_secs(1));
        let _binding = budget.bind_pause_controller(control.clone());
        let revision = control.request_pause();
        assert!(control.confirm_pause_slot(revision));
        let wait = budget.pacing_wait();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let credited = control.paused_duration();
        assert!(credited >= Duration::from_millis(30));
        drop(wait);
        assert_eq!(budget.pacing_waiters(), 0);
        assert_eq!(control.quota_waiters(), 0);
        let after_drop = control.paused_duration();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            control.paused_duration(),
            after_drop,
            "finite credit must stop at the waiter's synchronous drop"
        );
        assert!(control.resume(revision));
        drop(_binding);
        active.finish().await;
    }

    #[tokio::test]
    async fn direct_hash_clock_excludes_only_known_finite_quota_waits() {
        let (_store, _job, active) = crate::background_jobs::tests::active().await;
        let budget = active.fence().source_budget();
        let clock = SourcePacingClock::default();
        let deadline = clock.deadline(Duration::from_millis(100));
        tokio::pin!(deadline);
        let holding = clock.scope(async {
            let guard = budget.pacing_wait();
            tokio::time::sleep(Duration::from_millis(180)).await;
            drop(guard);
        });
        tokio::pin!(holding);
        tokio::select! {
            ()=&mut deadline=>panic!("finite byte service waiting consumed active hash budget"),
            ()=&mut holding=>{},
        }
        assert_eq!(budget.pacing_waiters(), 0);
        let active_started = tokio::time::Instant::now();
        // Neither an actual read nor a refused budget request enters the
        // quota-wait clock. They remain ordinary active execution time.
        assert!(budget.allowance(0).await.is_err());
        let directory = crate::test_tempdir().expect("create source test temp directory");
        let source = directory.path().join("active-read.bin");
        tokio::fs::write(&source, vec![1u8; 4096])
            .await
            .expect("write active-read fixture");
        assert_eq!(
            tokio::fs::read(source)
                .await
                .expect("read active-read fixture")
                .len(),
            4096
        );
        deadline.await;
        assert!(active_started.elapsed() >= Duration::from_millis(70));
        assert!(active_started.elapsed() < Duration::from_millis(250));
        active.finish().await;
    }

    #[tokio::test]
    async fn source_bridges_share_actual_byte_service_and_keep_cloned_descriptor_ranges_independent(
    ) {
        let (_store, _job, active) = crate::background_jobs::tests::active().await;
        let budget = active.fence().source_budget();
        let now = crate::state::clock_ms();
        // Inject only the allocator's issued receipt. Core Store tests cover
        // allocation/conjunction; this case exercises the real HTTP and file
        // hooks. A generous fixture expiry isolates byte limits from timing.
        *budget.grant.lock().await = Some(BackgroundSourceServiceGrant {
            receipt_id: uuid::Uuid::new_v4().to_string(),
            epoch_id: now / 5000,
            expires_at_ms: now + 60_000,
            byte_allowance: Some(4096),
            resource_keys: vec!["source_io".into()],
        });
        let directory = tempfile::tempdir().expect("source bridge fixture");
        let path = directory.path().join("positioned.bin");
        let bytes: Vec<u8> = (0..8192).map(|index| ((index / 2048) * 53) as u8).collect();
        tokio::fs::write(&path, &bytes)
            .await
            .expect("source fixture");
        let held = std::fs::File::open(&path).expect("held source");
        let a = crate::source_input::SourceInput::open_background_held(
            crate::fragment_index_cluster::SourceFence::clone_current_held(&held)
                .expect("first capability"),
            "application/octet-stream",
            Arc::clone(&budget),
        )
        .await
        .expect("first bridge");
        let b = crate::source_input::SourceInput::open_background_held(
            crate::fragment_index_cluster::SourceFence::clone_current_held(&held)
                .expect("same open description"),
            "application/octet-stream",
            Arc::clone(&budget),
        )
        .await
        .expect("second bridge");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("loopback client");
        async fn range(client: &reqwest::Client, url: &str, from: u64) -> Vec<u8> {
            client
                .get(url)
                .header(
                    reqwest::header::RANGE,
                    format!("bytes={from}-{}", from + 2047),
                )
                .send()
                .await
                .expect("range request")
                .error_for_status()
                .expect("range status")
                .bytes()
                .await
                .expect("range body")
                .to_vec()
        }
        let (first, second) =
            tokio::join!(range(&client, a.url(), 0), range(&client, b.url(), 4096));
        assert_eq!(first, &bytes[..2048]);
        assert_eq!(second, &bytes[4096..6144]);
        let response = client
            .get(a.url())
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .await
            .expect("exhausted request headers");
        {
            let body = response.bytes();
            tokio::pin!(body);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut body)
                    .await
                    .is_err(),
                "two helpers spent the one 4096-byte receipt; neither may start an extra source byte"
            );
        }
        a.close_and_wait().await.expect("first source cleanup");
        b.close_and_wait().await.expect("second source cleanup");
        active.finish().await;
    }

    #[test]
    fn renewing_permission_inside_the_same_epoch_never_forgets_spent_bytes() {
        let epoch = crate::state::clock_ms() / 5000;
        let now = epoch * 5000;
        let mut grant = BackgroundSourceServiceGrant {
            receipt_id: uuid::Uuid::new_v4().to_string(),
            epoch_id: epoch,
            expires_at_ms: now + 1000,
            byte_allowance: Some(1024),
            resource_keys: vec!["source_io".into()],
        };
        assert_eq!(
            reserve(&grant, 700, now + 1).expect("first permission"),
            Some(700)
        );
        grant.expires_at_ms = now + 4000;
        assert_eq!(
            reserve(&grant, 700, now + 1500).expect("renewed same receipt"),
            Some(324)
        );
        assert_eq!(
            reserve(&grant, 1, now + 1501).expect("same epoch exhausted"),
            None
        );
    }

    #[test]
    fn replayed_epoch_receipts_share_spend_and_never_reset_finite_allowance() {
        let now = crate::state::clock_ms();
        let grant = BackgroundSourceServiceGrant {
            receipt_id: uuid::Uuid::new_v4().to_string(),
            epoch_id: now / 5000,
            expires_at_ms: now + 5000,
            byte_allowance: Some(1024),
            resource_keys: vec!["source_io".into()],
        };
        assert_eq!(reserve(&grant, 700, now).expect("first helper"), Some(700));
        assert_eq!(
            reserve(&grant.clone(), 700, now).expect("replayed receipt in second helper"),
            Some(324)
        );
        assert_eq!(reserve(&grant, 1, now).expect("exhausted"), None);
        assert_eq!(
            reserve(&grant, 700, now + 5000).expect("expired grants cannot spend"),
            None
        );
    }
}
