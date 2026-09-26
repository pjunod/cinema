use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PublishedFailureCleanupOutcome {
    Reaped,
    RetirementTookOwnership,
}

#[derive(Clone, Copy, Debug)]
enum PublishedAttemptCleanupCause {
    Failure {
        decision_sequence: u64,
        reason: crate::playback_control::ProducerDecisionReason,
    },
    Completion {
        disposition: crate::playback_control::RollingProducerCompletionDisposition,
    },
    ExecutorLost,
}

async fn settle_exact_published_child(
    session: &Session,
    producer_attempt: u64,
    require_successful_exit: bool,
) -> Result<(), String> {
    let mut slot = session.child.lock().await;
    let Some(child) = slot.as_mut() else {
        // Another cancellation-independent owner can win this exact cleanup
        // before a cancelled executor's monitor starts its fallback owner.
        // Both paths serialize on `child_transition`, so an empty slot here
        // is already-settled rather than an unowned live process.
        session.release_hardware_after_confirmed_reap();
        session.release_software_after_confirmed_reap();
        return Ok(());
    };
    if child.producer_attempt != producer_attempt {
        return Err(format!(
            "actor selected producer attempt {producer_attempt}, but child slot contains {}",
            child.producer_attempt
        ));
    }
    if !require_successful_exit {
        child
            .request_termination()
            .map_err(|error| format!("terminating producer attempt {producer_attempt}: {error}"))?;
    }
    tokio::time::timeout(
        PREPUBLICATION_REAP_ATTEMPT_TIMEOUT,
        child.wait_for_terminal(),
    )
    .await
    .map_err(|_| {
        format!(
            "producer attempt {producer_attempt} reap exceeded {:?}",
            PREPUBLICATION_REAP_ATTEMPT_TIMEOUT
        )
    })?
    .map_err(|error| format!("reaping producer attempt {producer_attempt}: {error}"))?;
    let status = child
        .try_wait_observed(&session.control)
        .map_err(|error| format!("confirming producer attempt {producer_attempt} reap: {error}"))?
        .ok_or_else(|| {
            format!("producer attempt {producer_attempt} wait returned before confirmed reap")
        })?;
    if require_successful_exit && !status.success() {
        return Err(format!(
            "producer attempt {producer_attempt} completion cleanup observed non-success status {status}"
        ));
    }
    *slot = None;
    session.release_hardware_after_confirmed_reap();
    session.release_software_after_confirmed_reap();
    Ok(())
}

/// Retain the published generation while converging physical cleanup for one
/// exact producer attempt.
///
/// This task is spawned before its caller waits, so cancellation of the
/// executor cannot drop kill/reap ownership. Unlike prepublication cleanup it
/// must not publish `Session::fail`, clear the compatibility catalog or scratch
/// directory, or send actor/session End: playlist/init/segment objects already
/// admitted from this generation remain legitimate. Admission capacity is
/// released only after this task confirms the exact supervised child reaped.
async fn own_published_attempt_cleanup(
    session: Arc<Session>,
    producer_attempt: u64,
    cause: PublishedAttemptCleanupCause,
    sid: String,
    settled: tokio::sync::oneshot::Sender<PublishedFailureCleanupOutcome>,
) {
    let (cause_kind, failure_reason, completion_disposition) = match cause {
        PublishedAttemptCleanupCause::Failure { reason, .. } => ("failure", Some(reason), None),
        PublishedAttemptCleanupCause::Completion { disposition } => {
            ("completion", None, Some(disposition))
        }
        PublishedAttemptCleanupCause::ExecutorLost => ("executor_lost", None, None),
    };
    loop {
        let outcome = {
            // Retirement still uses this gate, so cleanup stays ordered with
            // a simultaneous explicit End. The comment here used to promise
            // that the copy cut would remove the need for it; that cut landed
            // in #642 and the need remains, because the gate's remaining job
            // is retirement ordering rather than copy replacement. Actor-
            // managed transcodes never replace a published attempt.
            let _transition = session.child_transition.lock().await;
            if session.retirement_cleanup_finished.load(Acquire) {
                Ok(PublishedFailureCleanupOutcome::RetirementTookOwnership)
            } else {
                settle_exact_published_child(
                    &session,
                    producer_attempt,
                    matches!(cause, PublishedAttemptCleanupCause::Completion { .. }),
                )
                .await
                .map(|()| PublishedFailureCleanupOutcome::Reaped)
            }
        };
        match outcome {
            Ok(outcome) => {
                if outcome == PublishedFailureCleanupOutcome::Reaped {
                    if let PublishedAttemptCleanupCause::Failure {
                        decision_sequence, ..
                    } = cause
                    {
                        if let Err(error) = session
                            .control
                            .decision_applied(decision_sequence, None)
                            .await
                        {
                            tracing::warn!(
                                target: "plurxd::transcode",
                                session = %session_log_id(&sid),
                                producer_attempt,
                                decision_sequence,
                                cleanup_policy = "retain_published",
                                ?error,
                                "published producer reaped after its actor decision could no longer be acknowledged"
                            );
                        }
                    }
                }
                tracing::info!(
                    target: "plurxd::transcode",
                    session = %session_log_id(&sid),
                    producer_attempt,
                    cause = cause_kind,
                    failure_reason = ?failure_reason,
                    completion_disposition = ?completion_disposition,
                    cleanup_policy = "retain_published",
                    outcome = match outcome {
                        PublishedFailureCleanupOutcome::Reaped => "reaped",
                        PublishedFailureCleanupOutcome::RetirementTookOwnership => {
                            "retirement_took_ownership"
                        }
                    },
                    "published producer cleanup settled without discarding admitted media"
                );
                let _ = settled.send(outcome);
                return;
            }
            Err(error) => {
                tracing::error!(
                    target: "plurxd::transcode",
                    session = %session_log_id(&sid),
                    producer_attempt,
                    cause = cause_kind,
                    failure_reason = ?failure_reason,
                    completion_disposition = ?completion_disposition,
                    cleanup_policy = "retain_published",
                    retry_after_ms = PREPUBLICATION_REAP_RETRY.as_millis(),
                    %error,
                    "published producer reap was not confirmed; retaining admissions, bytes, and cleanup ownership"
                );
                tokio::time::sleep(PREPUBLICATION_REAP_RETRY).await;
            }
        }
    }
}

pub(super) fn spawn_published_failure_cleanup_owner(
    session: &Arc<Session>,
    producer_attempt: u64,
    decision_sequence: u64,
    reason: crate::playback_control::ProducerDecisionReason,
    sid: &str,
) -> tokio::sync::oneshot::Receiver<PublishedFailureCleanupOutcome> {
    let (settled, settlement) = tokio::sync::oneshot::channel();
    tokio::spawn(own_published_attempt_cleanup(
        Arc::clone(session),
        producer_attempt,
        PublishedAttemptCleanupCause::Failure {
            decision_sequence,
            reason,
        },
        sid.to_owned(),
        settled,
    ));
    settlement
}

pub(super) fn spawn_published_completion_cleanup_owner(
    session: &Arc<Session>,
    producer_attempt: u64,
    disposition: crate::playback_control::RollingProducerCompletionDisposition,
    sid: &str,
) -> tokio::sync::oneshot::Receiver<PublishedFailureCleanupOutcome> {
    let (settled, settlement) = tokio::sync::oneshot::channel();
    tokio::spawn(own_published_attempt_cleanup(
        Arc::clone(session),
        producer_attempt,
        PublishedAttemptCleanupCause::Completion { disposition },
        sid.to_owned(),
        settled,
    ));
    settlement
}

pub(super) fn spawn_published_executor_loss_cleanup_owner(
    session: &Arc<Session>,
    producer_attempt: u64,
    sid: &str,
) {
    let (settled, _settlement) = tokio::sync::oneshot::channel();
    tokio::spawn(own_published_attempt_cleanup(
        Arc::clone(session),
        producer_attempt,
        PublishedAttemptCleanupCause::ExecutorLost,
        sid.to_owned(),
        settled,
    ));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RollingRetirementParticipation {
    Won,
    Joined,
}

pub(super) struct RollingRetirementOutcome {
    pub(super) removed: bool,
    pub(super) participation: RollingRetirementParticipation,
    pub(super) cause: Arc<str>,
}

/// The future a lifecycle hook returns. Boxed so the hook set is one trait
/// object in every build; see [`RetirementSettlementHooks`].
pub(super) type HookFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

/// The points of the rolling retirement settlement below that a test can
/// pause at (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8).
///
/// The settlement holds one of these in every build, so its layout and the
/// `.await` points of its `wait` are the same in the test and release
/// binaries. Production installs
/// [`NoopRetirementSettlementHooks`]; the race tests install a pausing
/// implementation. A paused hook's timing is still a test artefact: what this
/// makes identical is the struct and the set of await points, not scheduling.
pub(super) trait RetirementSettlementHooks: Send + Sync {
    /// After the waiter has registered its `Notify` interest and re-checked
    /// the result, before it awaits the notification.
    fn before_await_settled(&self) -> HookFuture<'_>;
}

/// What production installs: every point is already ready.
pub(super) struct NoopRetirementSettlementHooks;

/// A zero-sized, already-ready future, so boxing it allocates nothing and
/// awaiting it costs one poll.
struct HookReady;

impl std::future::Future for HookReady {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        std::task::Poll::Ready(())
    }
}

impl RetirementSettlementHooks for NoopRetirementSettlementHooks {
    fn before_await_settled(&self) -> HookFuture<'_> {
        Box::pin(HookReady)
    }
}

pub(super) struct RollingRetirementSettlement {
    cause: std::sync::Mutex<Arc<str>>,
    result: std::sync::Mutex<Option<Result<bool, String>>>,
    settled: tokio::sync::Notify,
    hooks: Box<dyn RetirementSettlementHooks>,
}

impl RollingRetirementSettlement {
    pub(super) fn new(cause: &'static str) -> Self {
        Self::with_hooks(cause, Box::new(NoopRetirementSettlementHooks))
    }

    pub(super) fn with_hooks(
        cause: &'static str,
        hooks: Box<dyn RetirementSettlementHooks>,
    ) -> Self {
        Self {
            cause: std::sync::Mutex::new(Arc::from(cause)),
            result: std::sync::Mutex::new(None),
            settled: tokio::sync::Notify::new(),
            hooks,
        }
    }

    pub(super) fn cause(&self) -> Arc<str> {
        Arc::clone(
            &self
                .cause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn set_cause(&self, cause: &'static str) {
        *self
            .cause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::from(cause);
    }

    pub(super) fn complete(&self, result: Result<bool, String>) {
        let mut stored = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if stored.is_none() {
            *stored = Some(result);
            drop(stored);
            self.settled.notify_waiters();
        }
    }

    pub(super) async fn wait(&self) -> Result<bool, String> {
        loop {
            if let Some(result) = self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return result;
            }
            let settled = self.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            if self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
            {
                continue;
            }
            self.hooks.before_await_settled().await;
            settled.await;
        }
    }
}

pub(super) struct RollingRetirementTicket {
    pub(super) participation: RollingRetirementParticipation,
    pub(super) settlement: Arc<RollingRetirementSettlement>,
}

#[derive(Clone)]
pub(super) struct RetiredPresentation {
    pub(super) session: Arc<Session>,
    pub(super) producer_attempt: u64,
    pub(super) serve_until: Instant,
}

pub(super) type RetiredPresentations = Arc<Mutex<HashMap<String, RetiredPresentation>>>;

#[derive(Clone)]
pub(super) struct RollingRetirementContext {
    pub(super) sessions: Weak<Mutex<HashMap<String, Arc<Session>>>>,
    pub(super) retired_presentations: Weak<Mutex<HashMap<String, RetiredPresentation>>>,
    pub(super) active_session_count: Arc<AtomicUsize>,
    pub(super) store: Arc<dyn Store>,
    pub(super) recent_marker_ambiguities: RecentMarkerAmbiguities,
}

impl RollingRetirementTicket {
    pub(super) async fn wait(&self) -> Result<bool, String> {
        self.settlement.wait().await
    }
}

/// Unique retired scratch is no longer on the serving or admission critical
/// path. One exact CAS-owned detached task makes a finite set of bounded
/// attempts and reports every failure. A persistent orphan is then left for
/// the existing startup/scheduled maintenance sweep; cached directories are
/// never submitted here.
pub(super) fn spawn_rolling_scratch_cleanup_owner(
    session_id: String,
    session: &Arc<Session>,
    #[cfg(test)] pause: Option<Arc<LifecycleTestPause>>,
) {
    if session.cached
        || session
            .scratch_cleanup_started
            .compare_exchange(false, true, AcqRel, Acquire)
            .is_err()
    {
        return;
    }
    let dir = session.dir.clone();
    begin_rolling_scratch_release(session);
    let owned = Arc::clone(session);
    tokio::spawn(async move {
        #[cfg(test)]
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
        for attempt in 1..=ROLLING_SCRATCH_CLEANUP_ATTEMPTS {
            let cleanup = tokio::time::timeout(ROLLING_SCRATCH_CLEANUP_ATTEMPT, async {
                clear_session_dir(&dir).await?;
                match tokio::fs::remove_dir_all(&dir).await {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                }
            })
            .await;
            match cleanup {
                Ok(Ok(())) => {
                    owned.live_bytes.store(0, Release);
                    owned.retention_garbage_bytes.store(0, Release);
                    settle_rolling_scratch_release(&owned);
                    tracing::debug!(
                        target: "plurxd::transcode",
                        session = %session_log_id(&session_id),
                        path = %dir.display(),
                        attempt,
                        "rolling retirement scratch cleanup settled"
                    );
                    return;
                }
                Ok(Err(error)) => tracing::warn!(
                    target: "plurxd::transcode",
                    session = %session_log_id(&session_id),
                    path = %dir.display(),
                    attempt,
                    %error,
                    "rolling retirement scratch cleanup attempt failed"
                ),
                Err(_) => tracing::warn!(
                    target: "plurxd::transcode",
                    session = %session_log_id(&session_id),
                    path = %dir.display(),
                    attempt,
                    timeout_ms = ROLLING_SCRATCH_CLEANUP_ATTEMPT.as_millis(),
                    "rolling retirement scratch cleanup attempt timed out"
                ),
            }
            if attempt < ROLLING_SCRATCH_CLEANUP_ATTEMPTS {
                tokio::time::sleep(ROLLING_SCRATCH_CLEANUP_RETRY).await;
            }
        }
        hold_rolling_scratch_charge(&owned, "cleanup_exhausted");
        tracing::error!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id),
            path = %dir.display(),
            attempts = ROLLING_SCRATCH_CLEANUP_ATTEMPTS,
            "rolling retirement scratch cleanup exhausted; orphan handed to startup/maintenance sweep"
        );
    });
}

fn spawn_retired_presentation_cleanup_owner(
    retired_presentations: RetiredPresentations,
    session_id: String,
    retired: RetiredPresentation,
) {
    tokio::spawn(async move {
        // Sleep on the shared promise, not on a value captured at spawn: an
        // exact same-viewer release can pull it in while this task waits, and
        // a deadline nobody can wake is a deadline that never moves. The
        // promise can only shorten, so re-reading it is monotone and the loop
        // terminates.
        loop {
            let until = retired
                .session
                .retired_release
                .deadline()
                .unwrap_or(retired.serve_until);
            // Register before the re-read, or a shorten between them is a
            // lost wakeup.
            let woken = retired.session.retired_release.wake.notified();
            if Instant::now() >= until {
                break;
            }
            tokio::select! {
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(until)) => {}
                () = woken => {}
            }
        }
        if retired
            .session
            .scratch_cleanup_started
            .compare_exchange(false, true, AcqRel, Acquire)
            .is_err()
        {
            return;
        }
        begin_rolling_scratch_release(&retired.session);
        #[cfg(test)]
        let cleanup_pause = retired
            .session
            .scratch_cleanup_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        #[cfg(test)]
        if let Some(pause) = cleanup_pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
        let mut attempt = 0_u64;
        loop {
            attempt = attempt.saturating_add(1);
            // Exactness governs the *map entry*, not the directory: `dir` is
            // this incarnation's own `w-{uuid}` and is shared with nothing.
            // A session id can be reused, so the map row can be overwritten
            // by a successor while this owner is still retrying a failing
            // unlink -- and returning here would abandon a charged ledger
            // entry whose last `Arc<Session>` is about to drop, leaving bytes
            // charged for the life of the process with nobody able to
            // release them. Clean our own directory either way.
            let exact = retired_presentations
                .lock()
                .await
                .get(&session_id)
                .is_some_and(|current| {
                    current.producer_attempt == retired.producer_attempt
                        && Arc::ptr_eq(&current.session, &retired.session)
                });
            let cleanup = tokio::time::timeout(ROLLING_SCRATCH_CLEANUP_ATTEMPT, async {
                clear_session_dir(&retired.session.dir).await?;
                match tokio::fs::remove_dir_all(&retired.session.dir).await {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                }
            })
            .await;
            if matches!(cleanup, Ok(Ok(()))) {
                let mut presentations = retired_presentations.lock().await;
                if presentations.get(&session_id).is_some_and(|current| {
                    current.producer_attempt == retired.producer_attempt
                        && Arc::ptr_eq(&current.session, &retired.session)
                }) {
                    presentations.remove(&session_id);
                }
                retired.session.live_bytes.store(0, Release);
                retired.session.retention_garbage_bytes.store(0, Release);
                drop(presentations);
                // The names are gone. Anything an accepted read still holds
                // open keeps its own charge; the rest is released once, and
                // a duplicate completion finds nothing left to subtract.
                settle_rolling_scratch_release(&retired.session);
                return;
            }
            hold_rolling_scratch_charge(&retired.session, "unlink_failed");
            tracing::warn!(
                target: "plurxd::transcode",
                session = %session_log_id(&session_id),
                path = %retired.session.dir.display(),
                attempt,
                superseded = !exact,
                "retired rolling object cleanup failed; promises remain charged for retry"
            );
            tokio::time::sleep(ROLLING_SCRATCH_CLEANUP_RETRY).await;
        }
    });
}

/// Convert this incarnation's producer reservation into its measured retained
/// inventory, once nothing can write into the directory any more.
///
/// Returns false when the conversion must be retried: a writer is still
/// registered, or the directory walk did not complete. Both keep the
/// conservative producer charge — a timeout is not evidence that capacity
/// became free, and a partial scan is not an inventory.
async fn convert_rolling_scratch_charge(session: &Session) -> bool {
    let Some(permit) = session.scratch.as_ref() else {
        return true;
    };
    let ledger = permit.ledger();
    let key = permit.key();
    if !ledger.writers_settled(key) {
        ledger.note_conservative(key, "writer_outstanding");
        return false;
    }
    // Taken before the walk and rechecked at commit: a writer that registers
    // and finishes while the directory is being enumerated moves this, and
    // the measurement it raced is refused rather than believed.
    let Some(generation) = ledger.inventory_generation(key) else {
        return true;
    };
    match session.measure_scratch_bytes().await {
        ScratchMeasurement::Complete(bytes) => {
            session.live_bytes.store(bytes, Release);
            ledger.commit_quiescent_measurement(key, generation, bytes)
        }
        ScratchMeasurement::Absent => {
            session.live_bytes.store(0, Release);
            ledger.commit_quiescent_measurement(key, generation, 0)
        }
        ScratchMeasurement::NotScratch => true,
        ScratchMeasurement::Incomplete => {
            ledger.note_conservative(key, "measurement_incomplete");
            false
        }
    }
}

/// Retry the conversion on a detached owner. Bounded: cleanup releases the
/// entry outright when it proves the names are gone, so this exists only to
/// shed unused future capacity sooner than that.
fn spawn_rolling_scratch_conversion_owner(session_id: String, session: Arc<Session>) {
    tokio::spawn(async move {
        for attempt in 1..=ROLLING_SCRATCH_CONVERSION_ATTEMPTS {
            tokio::time::sleep(ROLLING_SCRATCH_CONVERSION_RETRY).await;
            let Some(key) = session.scratch_key() else {
                return;
            };
            let Some(permit) = session.scratch.as_ref() else {
                return;
            };
            if permit.ledger().lifecycle_of(key).is_none() {
                return;
            }
            if convert_rolling_scratch_charge(&session).await {
                tracing::debug!(
                    target: "plurxd::transcode",
                    session = %session_log_id(&session_id),
                    attempt,
                    "retired rolling scratch reservation converted to measured inventory"
                );
                return;
            }
        }
        tracing::warn!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id),
            attempts = ROLLING_SCRATCH_CONVERSION_ATTEMPTS,
            reason = ?session
                .scratch
                .as_ref()
                .and_then(|permit| permit.ledger().conservative_reason(permit.key())),
            "retired rolling scratch kept its conservative charge; cleanup still owns it"
        );
    });
}

/// Fence new writers, wait for the registered ones, then convert.
///
/// Called with no registry or lifecycle lock held, and never on the admission
/// path: the wait is for a process's worker, not for a budget.
async fn settle_rolling_scratch_charge(session_id: &str, session: &Arc<Session>) {
    let Some(key) = session.scratch_key() else {
        return;
    };
    let Some(permit) = session.scratch.as_ref() else {
        return;
    };
    let ledger = Arc::clone(permit.ledger());
    let Some(barrier) = ledger.begin_retirement(key) else {
        return;
    };
    let deadline = tokio::time::Instant::now() + ROLLING_SCRATCH_WRITER_SETTLE;
    if !barrier.settle(&ledger, key, deadline).await {
        ledger.note_conservative(key, "writer_stalled");
        tracing::warn!(
            target: "plurxd::transcode",
            session = %session_log_id(session_id),
            budget_ms = ROLLING_SCRATCH_WRITER_SETTLE.as_millis(),
            "a scratch writer outlived retirement settlement; the producer charge stands"
        );
        spawn_rolling_scratch_conversion_owner(session_id.to_owned(), Arc::clone(session));
        return;
    }
    if !convert_rolling_scratch_charge(session).await {
        spawn_rolling_scratch_conversion_owner(session_id.to_owned(), Arc::clone(session));
    }
}

/// Cleanup has taken the directory. Bytes stay charged until deletion is
/// proven; only the category changes.
fn begin_rolling_scratch_release(session: &Session) {
    if let Some(permit) = session.scratch.as_ref() {
        permit.ledger().begin_release(permit.key());
    }
}

/// Every name is gone. Objects an accepted read still holds open move to
/// reader-pin ownership; everything else stops being charged, exactly once,
/// whether or not an unrelated status reader still holds this session.
fn settle_rolling_scratch_release(session: &Session) {
    if let Some(permit) = session.scratch.as_ref() {
        permit.ledger().account_unlink_all(permit.key());
    }
}

fn hold_rolling_scratch_charge(session: &Session, reason: &'static str) {
    if let Some(permit) = session.scratch.as_ref() {
        permit.ledger().note_conservative(permit.key(), reason);
    }
}

/// Finish one rolling Session only after its exact supervised child is known
/// reaped. Admission resources deliberately remain installed until this
/// point: releasing them after SIGKILL request but before `wait` would let a
/// replacement overcommit the same hardware or software capacity while the
/// predecessor still exists in the kernel.
async fn finish_rolling_retirement_after_reap(session: &Session) {
    session.release_hardware_after_confirmed_reap();
    session.release_software_after_confirmed_reap();
    session.prepublication_cleanup_active.store(false, Release);
}

async fn finish_prepublication_cleanup_after_reap(session: &Arc<Session>) {
    finish_rolling_retirement_after_reap(session).await;
    if !session.cached {
        spawn_rolling_scratch_cleanup_owner(
            "unregistered-prepublication".to_owned(),
            session,
            #[cfg(test)]
            session
                .scratch_cleanup_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
    }
    if session.control.end().await.is_err() {
        session.control.fence_unavailable();
    }
}

/// One detached cleanup owner survives cancellation of whichever request,
/// executor, or routine retirement transferred prepublication ownership. It
/// never returns an admission resource until the exact supervised child has
/// confirmed reap; unique scratch is then transferred to its own retrying
/// owner. A failed first reap terminalizes
/// serving but retains the Session, child handle, and permits while retrying
/// physical convergence; callers decide separately whether to record a typed
/// producer failure.
async fn own_prepublication_cleanup(
    session: Arc<Session>,
    first_attempt_settled: tokio::sync::oneshot::Sender<()>,
) {
    let mut first_attempt_settled = Some(first_attempt_settled);
    loop {
        // Hold lifecycle serialization only for one bounded reap attempt and,
        // on success, its final admission settlement and scratch handoff. Sleeping with
        // this lock would block the global reaper and every explicit stop.
        let cleanup = {
            let _transition = session.child_transition.lock().await;
            let cleanup = terminate_current_prepublication_child(&session).await;
            if cleanup.is_ok() {
                finish_prepublication_cleanup_after_reap(&session).await;
                session.prepublication_cleanup_active.store(false, Release);
            }
            cleanup
        };
        match cleanup {
            Ok(()) => {
                if let Some(settled) = first_attempt_settled.take() {
                    let _ = settled.send(());
                }
                return;
            }
            Err(error) => {
                tracing::error!(
                    target: "plurxd::transcode",
                    path = %session.dir.display(),
                    %error,
                    "prepublication producer reap was not confirmed; retaining admission and retrying cleanup"
                );
                if let Some(settled) = first_attempt_settled.take() {
                    if session.control.end().await.is_err() {
                        session.control.fence_unavailable();
                    }
                    let _ = settled.send(());
                }
                tokio::time::sleep(PREPUBLICATION_REAP_RETRY).await;
            }
        }
    }
}

pub(super) fn spawn_prepublication_cleanup_owner(
    session: &Arc<Session>,
) -> Option<tokio::sync::oneshot::Receiver<()>> {
    if session.retirement_cleanup_started.load(Acquire) {
        return None;
    }
    if session
        .prepublication_cleanup_active
        .compare_exchange(false, true, AcqRel, Acquire)
        .is_err()
    {
        return None;
    }
    let (first_attempt_settled, settled) = tokio::sync::oneshot::channel();
    let cleanup_session = Arc::clone(session);
    tokio::spawn(async move {
        // The cleanup owner is detached before the caller awaits it. Dropping
        // the HTTP/start/executor future therefore cannot drop the child or
        // release its admission half-way through a reap.
        own_prepublication_cleanup(cleanup_session, first_attempt_settled).await;
    });
    Some(settled)
}

/// Cancellation-independent convergence for one exact rolling Session.
///
/// Before actor Terminal admission, `deadline` is a genuine no-mutation
/// boundary. After Terminal applies, this task is the sole owner of registry
/// removal, confirmed child reap, and admission release;
/// caller cancellation and later deadlines cannot revoke that ownership.
#[allow(clippy::too_many_arguments)]
async fn own_rolling_retirement(
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    retired_presentations: RetiredPresentations,
    recent_marker_ambiguities: RecentMarkerAmbiguities,
    active_session_count: Arc<AtomicUsize>,
    store: Arc<dyn Store>,
    session_id: String,
    session: Arc<Session>,
    deadline: Option<tokio::time::Instant>,
    settlement: Arc<RollingRetirementSettlement>,
    committed: tokio::sync::oneshot::Sender<()>,
) -> Result<bool, String> {
    #[cfg(test)]
    session.retirement_started.store(true, Release);

    let transition = match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, session.child_transition.lock())
            .await
            .map_err(|_| replacement_deadline_error())?,
        None => session.child_transition.lock().await,
    };

    let registered_key = {
        let registry = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, sessions.lock())
                .await
                .map_err(|_| replacement_deadline_error())?,
            None => sessions.lock().await,
        };
        registry
            .iter()
            .find_map(|(id, registered)| Arc::ptr_eq(registered, &session).then_some(id.clone()))
    };
    if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
        return Err(replacement_deadline_error());
    }

    // Prepublication failure cleanup and routine retirement share this one
    // process-resource claim. The detached retirement owner takes it before
    // Terminal admission, but releases it again if the precommit deadline
    // wins; after Terminal, the universal retirement claim is monotonic.
    let prepublication_cleanup_claimed = session.prepublication_process_cleanup_required();
    if prepublication_cleanup_claimed {
        session.prepublication_cleanup_active.store(true, Release);
    }

    let terminal_cause = match session.end_activity_until(deadline).await {
        Ok(cause) => cause,
        Err(error) => {
            if !session.control.is_retired() {
                if prepublication_cleanup_claimed {
                    session.prepublication_cleanup_active.store(false, Release);
                }
                return Err(error);
            }
            session
                .control
                .snapshot()
                .await
                .and_then(|snapshot| snapshot.terminal)
                .unwrap_or(crate::playback_control::RollingTerminalCause::AuthorityFence)
        }
    };
    match terminal_cause {
        crate::playback_control::RollingTerminalCause::End => {}
        crate::playback_control::RollingTerminalCause::AuthorityFence => {
            settlement.set_cause("authority_fence");
        }
        crate::playback_control::RollingTerminalCause::LeaseExpired => {
            settlement.set_cause(if session.failed.load(Acquire) {
                "failed"
            } else {
                "idle"
            });
        }
        crate::playback_control::RollingTerminalCause::StartupExpired => {
            settlement.set_cause("startup_expired");
        }
        crate::playback_control::RollingTerminalCause::PauseExpired => {
            settlement.set_cause("pause_expired");
        }
    }
    session.retirement_cleanup_started.store(true, Release);
    let _ = committed.send(());

    #[cfg(test)]
    let pause = session
        .retirement_cleanup_handoff_pause
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    #[cfg(test)]
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }

    drop(transition);

    // Terminal admission is a fact and no lifecycle lock is held, so this is
    // where a subtitle window this playback still owns stops being anybody's
    // work. Releasing it here rather than after the reap matters for the same
    // reason the reap itself does: a slot that outlived its session would let
    // the next playback to use this id inherit an obsolete ordering fact and
    // refuse its own first window. Adoption may rename a session during actor
    // settlement, so both names it can be known by are released.
    crate::subtitles::release_session_window(&session_id).await;
    if let Some(registered) = registered_key.as_deref() {
        if registered != session_id {
            crate::subtitles::release_session_window(registered).await;
        }
    }

    loop {
        let cleanup = {
            let _transition = session.child_transition.lock().await;
            let cleanup = terminate_current_prepublication_child(&session).await;
            if cleanup.is_ok() {
                finish_rolling_retirement_after_reap(&session).await;
            }
            cleanup
        };
        match cleanup {
            Ok(()) => {
                // The child is reaped, but a copy reader may still be
                // draining its pipe. Fence new writers, wait for the ones
                // already registered, then measure once and convert the
                // producer reservation into the measured retained inventory
                // — all of it outside every registry and lifecycle lock, and
                // all of it before this session enters the retired registry
                // carrying a charge it no longer needs.
                settle_rolling_scratch_charge(&session_id, &session).await;
                // Keep the exact retired Arc discoverable until process death
                // and admission release are both facts. Adoption may rename it
                // during actor settlement, so remove by pointer at the end.
                let retired_promise = if session.cached {
                    None
                } else {
                    Some(RetiredPresentation {
                        session: Arc::clone(&session),
                        producer_attempt: session.control.current_producer_attempt(),
                        serve_until: session.prepare_retired_object_promise().await,
                    })
                };
                let (removed, removed_key, installed_retired) = {
                    let mut registry = sessions.lock().await;
                    let mut retired_registry = retired_presentations.lock().await;
                    let exact_key = registry.iter().find_map(|(id, registered)| {
                        Arc::ptr_eq(registered, &session).then_some(id.clone())
                    });
                    if let Some(exact_key) = exact_key.as_deref() {
                        // Refresh while the live registry lock is still held.
                        // Consumers check live first and recent second, so
                        // there is no instant where both forms are absent.
                        remember_rolling_marker_ambiguity(&recent_marker_ambiguities, &session);
                        if let Some(retired) = retired_promise.as_ref() {
                            retired_registry.insert(exact_key.to_owned(), retired.clone());
                        }
                        registry.remove(exact_key);
                        active_session_count.store(registry.len(), Relaxed);
                    }
                    let removed = exact_key.is_some();
                    (removed, exact_key, retired_promise.filter(|_| removed))
                };
                session.retirement_cleanup_finished.store(true, Release);
                settlement.complete(Ok(removed));
                let winning_cause = settlement.cause();
                let settled_session_id = removed_key
                    .as_deref()
                    .or(registered_key.as_deref())
                    .unwrap_or(&session_id);
                if let Some(retired) = installed_retired {
                    spawn_retired_presentation_cleanup_owner(
                        Arc::clone(&retired_presentations),
                        settled_session_id.to_owned(),
                        retired,
                    );
                } else if !session.cached {
                    spawn_rolling_scratch_cleanup_owner(
                        settled_session_id.to_owned(),
                        &session,
                        #[cfg(test)]
                        session
                            .scratch_cleanup_pause
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take(),
                    );
                }
                if registered_key.is_some() || removed {
                    emit_session_event_to_store(
                        Arc::clone(&store),
                        settled_session_id,
                        &session,
                        "session_end",
                        SessionEventFields {
                            reason: Some(winning_cause.as_ref()),
                            ..SessionEventFields::default()
                        },
                    )
                    .await;
                }
                return Ok(removed);
            }
            Err(error) => {
                tracing::error!(
                    target: "plurxd::transcode",
                    session = %session_log_id(&session_id),
                    path = %session.dir.display(),
                    %error,
                    "rolling retirement could not confirm producer reap; retaining admissions and retrying"
                );
                tokio::time::sleep(PREPUBLICATION_REAP_RETRY).await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_rolling_retirement_owner(
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    retired_presentations: RetiredPresentations,
    active_session_count: Arc<AtomicUsize>,
    store: Arc<dyn Store>,
    recent_marker_ambiguities: RecentMarkerAmbiguities,
    session_id: String,
    session: Arc<Session>,
    deadline: Option<tokio::time::Instant>,
    cause: &'static str,
) -> (tokio::sync::oneshot::Receiver<()>, RollingRetirementTicket) {
    let (settlement, participation) = {
        let mut shared = session
            .retirement_settlement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match shared.as_ref() {
            Some(settlement) => (
                Arc::clone(settlement),
                RollingRetirementParticipation::Joined,
            ),
            None => {
                let settlement = Arc::new(RollingRetirementSettlement::new(cause));
                *shared = Some(Arc::clone(&settlement));
                (settlement, RollingRetirementParticipation::Won)
            }
        }
    };
    let (committed, commit) = tokio::sync::oneshot::channel();
    if participation == RollingRetirementParticipation::Won {
        remember_rolling_marker_ambiguity(&recent_marker_ambiguities, &session);
        let owner_settlement = Arc::clone(&settlement);
        tokio::spawn(async move {
            let outcome = own_rolling_retirement(
                sessions,
                retired_presentations,
                recent_marker_ambiguities,
                active_session_count,
                store,
                session_id,
                Arc::clone(&session),
                deadline,
                Arc::clone(&owner_settlement),
                committed,
            )
            .await;
            if outcome.is_err() && !session.control.is_retired() {
                let mut shared = session
                    .retirement_settlement
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if shared
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &owner_settlement))
                {
                    *shared = None;
                }
                session.prepublication_cleanup_active.store(false, Release);
            }
            owner_settlement.complete(outcome);
        });
    } else {
        drop(committed);
    }
    (
        commit,
        RollingRetirementTicket {
            participation,
            settlement,
        },
    )
}

fn remember_rolling_marker_ambiguity(
    registry: &std::sync::Mutex<RecentMarkerAmbiguityLedger>,
    session: &Session,
) {
    let method = match session.kind {
        SessionKind::Copy { .. } => "remux",
        SessionKind::Transcode { .. } => "transcode",
    };
    remember_rolling_marker_ambiguity_key(
        registry,
        &session.supersession_user,
        session.file_id,
        method,
    );
}

pub(super) fn remember_rolling_marker_ambiguity_key(
    registry: &std::sync::Mutex<RecentMarkerAmbiguityLedger>,
    user_scope: &str,
    file_id: i64,
    method: &'static str,
) {
    let Ok(mut recent) = registry.lock() else {
        return;
    };
    let now = Instant::now();
    let deadline = now + MARKER_AMBIGUITY_TTL;
    recent.entries.retain(|_, deadline| *deadline > now);
    if recent
        .overflow_ambiguous_until
        .is_some_and(|deadline| deadline <= now)
    {
        recent.overflow_ambiguous_until = None;
    }
    let key = (user_scope.to_owned(), file_id, method);
    if let Some(existing) = recent.entries.get_mut(&key) {
        *existing = deadline;
    } else if recent.entries.len() < MAX_MARKER_AMBIGUITIES {
        recent.entries.insert(key, deadline);
    } else {
        recent.overflow_ambiguous_until = Some(
            recent
                .overflow_ambiguous_until
                .map_or(deadline, |existing| existing.max(deadline)),
        );
    }
}

pub(super) fn recent_rolling_marker_ambiguity(
    registry: &std::sync::Mutex<RecentMarkerAmbiguityLedger>,
    user_scope: &str,
    file_id: i64,
) -> bool {
    registry.lock().map_or(true, |mut recent| {
        let now = Instant::now();
        recent.entries.retain(|_, deadline| *deadline > now);
        if recent
            .overflow_ambiguous_until
            .is_some_and(|deadline| deadline <= now)
        {
            recent.overflow_ambiguous_until = None;
        }
        // The shipped method is plan-time and a rolling session may be
        // promoted later (for example, subtitle burn). Any recently retired
        // same-user/file rolling presentation is therefore ambiguous.
        recent.overflow_ambiguous_until.is_some()
            || recent
                .entries
                .keys()
                .any(|(scope, id, _)| scope == user_scope && *id == file_id)
    })
}

pub(super) fn spawn_context_retirement_owner(
    session: &Arc<Session>,
    deadline: Option<tokio::time::Instant>,
    cause: &'static str,
) -> Option<RollingRetirementTicket> {
    let context = session.retirement_context.as_ref()?;
    let sessions = context.sessions.upgrade()?;
    let retired_presentations = context.retired_presentations.upgrade()?;
    let (_commit, ticket) = spawn_rolling_retirement_owner(
        sessions,
        retired_presentations,
        Arc::clone(&context.active_session_count),
        Arc::clone(&context.store),
        Arc::clone(&context.recent_marker_ambiguities),
        "prepublication".to_owned(),
        Arc::clone(session),
        deadline,
        cause,
    );
    Some(ticket)
}

/// One detached supersession transaction owns the VOD sweep and the complete
/// rolling victim snapshot. The supplied deadline is honored only until the
/// first VOD tombstone or rolling actor Terminal admission. Once either
/// mutation wins, every snapshotted rolling victim is started without a
/// caller deadline and all settlement futures remain owned here.
#[allow(clippy::too_many_arguments)]
pub(super) async fn own_supersession_convergence(
    vod: Arc<crate::vodserve::VodServe>,
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    retired_presentations: RetiredPresentations,
    active_session_count: Arc<AtomicUsize>,
    store: Arc<dyn Store>,
    recent_marker_ambiguities: RecentMarkerAmbiguities,
    doomed: Vec<(String, Arc<Session>)>,
    deadline: Option<tokio::time::Instant>,
    supersession_user: String,
    playback_id: String,
) -> Result<Vec<(String, Arc<Session>)>, String> {
    let vod_ended = vod
        .supersede_before(&supersession_user, &playback_id, "", deadline)
        .await
        .map_err(|_| replacement_deadline_error())?;
    let mut committed = vod_ended > 0;
    let mut pending = Vec::new();
    let mut removed = Vec::new();
    for (session_id, session) in doomed {
        let retirement_deadline = if committed { None } else { deadline };
        let (mut commit, ticket) = spawn_rolling_retirement_owner(
            Arc::clone(&sessions),
            Arc::clone(&retired_presentations),
            Arc::clone(&active_session_count),
            Arc::clone(&store),
            Arc::clone(&recent_marker_ambiguities),
            session_id.clone(),
            Arc::clone(&session),
            retirement_deadline,
            "superseded",
        );
        if committed {
            pending.push((session_id, session, ticket));
            continue;
        }
        let outcome = {
            let result = ticket.wait();
            tokio::pin!(result);
            tokio::select! {
                biased;
                admission = &mut commit => {
                    match admission {
                        Ok(()) => None,
                        Err(_) => Some(result.await),
                    }
                }
                settlement = &mut result => {
                    Some(settlement)
                }
            }
        };
        match outcome {
            None => {
                committed = true;
                pending.push((session_id, session, ticket));
                continue;
            }
            Some(Ok(true)) => {
                committed = true;
                removed.push((session_id, session));
                continue;
            }
            Some(Ok(false)) => {}
            Some(Err(error)) => return Err(error),
        }
    }

    // Every victim after the first irreversible mutation is already running
    // under detached ownership. Awaiting here is only for the initiating
    // caller's success/event projection; dropping this future cannot cancel
    // any individual settlement task.
    for (session_id, session, ticket) in pending {
        match ticket.wait().await {
            Ok(true) => removed.push((session_id, session)),
            Ok(false) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(removed)
}
