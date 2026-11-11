//! Production lifecycle for common durable claims. A physical worker keeps
//! its admission guard until its child has joined; this module owns only the
//! durable token, monotonic deadline and cancellation notification.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use plurx_core::cluster::coordination::ClusterJobAuthority;
use plurx_core::error::StoreError;
use plurx_core::store::background_jobs::{
    BackgroundJob, ClaimJob, ClaimOutcome, ClaimResolution, JobPublishOutcome, JobSettlement,
    JobToken, PublishTranscodeJob, RenewJobs, RenewOutcome, ResolveClaim, SettleJob,
    TranscodeJobOutput, JOB_LEASE_MS, JOB_RENEW_INTERVAL_MS,
};
use plurx_core::store::Store;
use tokio::sync::{watch, Mutex};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
#[path = "../../plurx-core/tests/support/subtitle_jobs.rs"]
pub(crate) mod subtitle_fixture;

#[cfg(test)]
#[path = "background_job_faults.rs"]
pub(crate) mod faults;
#[path = "background_job_metrics.rs"]
mod metrics;
use metrics::Event;
pub(crate) use metrics::{accepted_claims, prometheus};

#[path = "background_jobs/analysis_reconcile.rs"]
pub(crate) mod analysis_reconcile;
#[path = "background_jobs/source.rs"]
mod source;
pub(crate) use source::{
    budgeted, source_budget, RawRetirement, SourceHolder, SourcePacingClock, SourceReadBudget,
};

const PUBLICATION_MARGIN: Duration = Duration::from_secs(3);

/// Why one claimed copy/encoded output preparation ended without publishing.
/// The class, not the message, decides the durable settlement: a preparation
/// that is refused for a reason that will hold on every rerun must consume
/// its attempt budget instead of re-running a full remux/encode forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparationError {
    /// Preemption by a viewer, a lost lease, the pass deadline or a busy
    /// owner. No attempt is consumed; the row is queued again.
    Yield(&'static str),
    /// A store, I/O or process failure. Consumes one attempt through the
    /// shared retry backoff, so the attempt limit bounds a persistent fault.
    Retry(String),
    /// The same payload against the same facts refuses identically on every
    /// run (unsupported payload, changed policy, identity or delivery).
    Fail(&'static str),
    /// The source the job names is gone or changed incarnation.
    Stop(&'static str),
}

impl std::fmt::Display for PreparationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Yield(code) => write!(f, "yield: {code}"),
            Self::Retry(detail) => write!(f, "retry: {detail}"),
            Self::Fail(code) => write!(f, "fail: {code}"),
            Self::Stop(code) => write!(f, "stop: {code}"),
        }
    }
}

/// Unclassified failures from store, filesystem and process layers are
/// transient by default; deterministic refusals are named explicitly.
impl From<String> for PreparationError {
    fn from(detail: String) -> Self {
        Self::Retry(detail)
    }
}

impl From<&str> for PreparationError {
    fn from(detail: &str) -> Self {
        Self::Retry(detail.to_owned())
    }
}

impl PreparationError {
    /// Durable settlement plus the bounded pass-telemetry reason. `preempted`
    /// is the caller's re-observation of lease loss, owner preemption or the
    /// pass deadline: a transient-looking error raised while a viewer was
    /// taking the owner is that preemption, not a consumed attempt.
    pub(crate) fn settlement(
        &self,
        job: &BackgroundJob,
        retry_code: &'static str,
        preempted: bool,
        now_ms: i64,
    ) -> (JobSettlement, &'static str) {
        match self {
            Self::Yield(code) => (
                JobSettlement::Yield {
                    error_code: Some((*code).to_owned()),
                    checkpoint: None,
                    not_before_ms: now_ms.saturating_add(5_000),
                },
                "yielded",
            ),
            Self::Retry(_) if preempted => (
                JobSettlement::Yield {
                    error_code: Some("preempted".to_owned()),
                    checkpoint: None,
                    not_before_ms: now_ms.saturating_add(5_000),
                },
                "yielded",
            ),
            Self::Retry(_) => (
                JobSettlement::Retry {
                    error_code: retry_code.to_owned(),
                    not_before_ms: now_ms
                        .saturating_add(retry_delay_ms(&job.id, job.failed_attempts)),
                },
                "retry",
            ),
            Self::Fail(code) => (
                JobSettlement::Fail {
                    error_code: (*code).to_owned(),
                },
                "failed",
            ),
            Self::Stop(code) => (
                JobSettlement::Stop {
                    error_code: (*code).to_owned(),
                },
                "stopped",
            ),
        }
    }
}

/// One finite Copy watchdog covers resolution through historical settlement.
/// Returning drops the owned operation before its caller settles or retires;
/// cancellation does not assert that an already-dispatched SQL write rolled back.
pub(crate) async fn watch_copy_preparation(
    fence: &JobFence,
    deadline: Instant,
    still_idle: impl Fn() -> bool,
    operation: impl std::future::Future<Output = Result<bool, PreparationError>>,
) -> Result<bool, PreparationError> {
    let lost = fence.loss_token();
    if lost.is_cancelled() && !fence.copy_output_completed() {
        return Err(PreparationError::Yield("lease_lost"));
    }
    if Instant::now() >= deadline {
        return Err(PreparationError::Yield("pass_deadline"));
    }
    if !still_idle() {
        return Err(PreparationError::Yield("preempted"));
    }
    tokio::pin!(operation);
    loop {
        tokio::select! {
            biased;
            result = &mut operation => return result,
            _ = tokio::time::sleep_until(deadline) =>
                return Err(PreparationError::Yield("pass_deadline")),
            _ = lost.cancelled(), if !fence.copy_output_completed() => {
                if !fence.copy_output_completed() {
                    // Settlement may be historical; the caller's settle is
                    // refused by the fence either way.
                    return Err(PreparationError::Yield("lease_lost"));
                }
            },
            _ = tokio::time::sleep(crate::transcode::PRODUCER_POLL) => {
                if !still_idle() {
                    return Err(PreparationError::Yield("preempted"));
                }
            },
        }
    }
}

/// Disposable per-loop pacing. Empty polls never need a durable timestamp.
pub(crate) struct IdlePoll {
    idle_rounds: u32,
    jitter: u64,
}
impl IdlePoll {
    pub(crate) fn new() -> Self {
        Self {
            idle_rounds: 0,
            jitter: uuid::Uuid::new_v4().as_u128() as u64,
        }
    }
    /// Only callers with an acknowledged per-pass publication may use the
    /// burst delay. Claims, retries and yields are not productive outcomes.
    pub(crate) fn after_completion(&mut self, published: bool) -> Duration {
        if !published {
            return self.delay(false);
        }
        self.idle_rounds = 0;
        self.jitter = self
            .jitter
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        Duration::from_millis(100 + self.jitter % 151)
    }

    pub(crate) fn delay(&mut self, progressed: bool) -> Duration {
        if progressed {
            self.idle_rounds = 0;
        }
        let base_ms = (5000_u64 << self.idle_rounds.min(3)).min(25_000);
        self.jitter = self
            .jitter
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        let delay = Duration::from_millis(base_ms + self.jitter % 5001);
        if !progressed {
            self.idle_rounds = self.idle_rounds.saturating_add(1);
        }
        delay
    }
}

pub(crate) fn retry_delay_ms(id: &str, failed_attempts: i64) -> i64 {
    let base = match failed_attempts {
        0 => 5_000,
        1 => 30_000,
        2 => 120_000,
        _ => 600_000,
    };
    let jitter = id.bytes().fold(failed_attempts as u64, |seed, byte| {
        seed.wrapping_mul(31).wrapping_add(u64::from(byte))
    }) % (base as u64 / 5);
    base + jitter as i64
}

fn unix_ms() -> Result<i64, StoreError> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| StoreError::Task(format!("background clock precedes epoch: {error}")))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| StoreError::Task("background clock overflow".into()))
}

fn settlement_event(settlement: &JobSettlement) -> Event {
    match settlement {
        JobSettlement::Yield { .. } => Event::Yield,
        JobSettlement::Retry { .. } | JobSettlement::Fail { .. } => Event::ChargedFailure,
        JobSettlement::Cancel | JobSettlement::Stop { .. } => Event::Cancel,
    }
}

struct ClaimState {
    token: Option<JobToken>,
    cancelling: bool,
}

struct Inner {
    kind: plurx_core::store::background_jobs::JobKind,
    started: Instant,
    store: Arc<dyn Store>,
    authority: Arc<dyn ClusterJobAuthority>,
    state: Mutex<ClaimState>,
    deadline: watch::Sender<Instant>,
    lost: CancellationToken,
    /// Historical acknowledged completion only; never artifact authority.
    copy_output_completed: AtomicBool,
    /// Retained independently of the renewable/retired job token. Only an
    /// explicit finish after physical join may settle this source custody.
    source_custody:
        Option<plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken>,
    source_budget: Arc<SourceReadBudget>,
    /// Opened only by `finish` after retirement; authorizes the live
    /// completion ledger. Drop never opens it.
    settle: CancellationToken,
}

#[derive(Clone)]
pub(crate) struct JobFence(Arc<Inner>);

pub(crate) struct ActiveBackgroundJob {
    fence: JobFence,
    stop: CancellationToken,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

/// A failed RPC is not a failed claim. Resolve the same identity while the
/// caller continues holding its local permit; do not try another candidate
/// merely because the first acknowledgement disappeared.
pub(crate) async fn claim_with_resolution(
    store: &dyn Store,
    candidate: &BackgroundJob,
    request: ClaimJob,
) -> Result<Option<(BackgroundJob, Instant)>, StoreError> {
    let kind = request.kind;
    let issued = Instant::now();
    metrics::event(kind, Event::ClaimWrite);
    let queue_age = request
        .now_ms
        .saturating_sub(candidate.created_at_ms)
        .max(0) as u64;
    let result = claim_with_resolution_inner(store, request).await;
    metrics::claim_latency(kind, issued.elapsed());
    match &result {
        Ok(Some(_)) => {
            metrics::event(kind, Event::ClaimAccepted);
            metrics::queue_wait(kind, Duration::from_millis(queue_age));
            if candidate.state == plurx_core::store::background_jobs::JobState::Running {
                metrics::event(kind, Event::Takeover);
            }
        }
        Ok(None) => metrics::event(kind, Event::ClaimRefused),
        Err(_) => metrics::event(kind, Event::ClaimError),
    }
    result
}

async fn claim_with_resolution_inner(
    store: &dyn Store,
    request: ClaimJob,
) -> Result<Option<(BackgroundJob, Instant)>, StoreError> {
    let issued = Instant::now();
    let deadline = issued + Duration::from_millis(JOB_LEASE_MS as u64);
    let claimed = if request.kind.permits_artifact_execution() {
        store.claim_artifact_job(request.clone()).await
    } else {
        store.claim_job(request.clone()).await
    };
    #[cfg(test)]
    let claimed = faults::after_commit(faults::CLAIM, claimed);
    match claimed {
        Ok(ClaimOutcome::Claimed { job }) if Instant::now() < deadline => {
            let remaining = job.token.as_ref().map_or(0, |token| {
                token
                    .lease_expires_ms
                    .saturating_sub(request.now_ms)
                    .clamp(0, JOB_LEASE_MS)
            });
            let confirmed_deadline = issued + Duration::from_millis(remaining as u64);
            return Ok((Instant::now() < confirmed_deadline).then_some((*job, confirmed_deadline)));
        }
        Ok(_) => return Ok(None),
        Err(error) => {
            tracing::warn!(job = request.job_id, %error, "resolving ambiguous background claim")
        }
    }
    while Instant::now() < deadline {
        let resolution = store
            .resolve_claim(ResolveClaim {
                job_id: request.job_id.clone(),
                node_id: request.node_id.clone(),
                boot_id: request.boot_id.clone(),
                claim_id: request.claim_id.clone(),
                fence: None,
                dispatched_at_ms: request.dispatched_at_ms,
                now_ms: unix_ms()?,
            })
            .await;
        match resolution {
            Ok(ClaimResolution::Running { token }) if Instant::now() < deadline => {
                // A takeover charges the abandoned attempt and can change
                // scheduling/checkpoint state. Recover the committed row,
                // never pair a new token with the pre-claim candidate image.
                if let Ok(Some(job)) = store.background_job(&request.job_id).await {
                    if job.token.as_ref() == Some(&token)
                        && job
                            .supported_payload()
                            .is_ok_and(|payload| payload.kind() == request.kind)
                    {
                        let remaining = token
                            .lease_expires_ms
                            .saturating_sub(request.now_ms)
                            .clamp(0, JOB_LEASE_MS);
                        let confirmed_deadline = issued + Duration::from_millis(remaining as u64);
                        return Ok((Instant::now() < confirmed_deadline)
                            .then_some((job, confirmed_deadline)));
                    }
                }
            }
            Ok(ClaimResolution::CancelRequested { cleanup_token }) => {
                store
                    .settle_job(SettleJob {
                        token: cleanup_token,
                        settlement: JobSettlement::Cancel,
                        now_ms: unix_ms()?,
                    })
                    .await?;
                return Ok(None);
            }
            Ok(ClaimResolution::LostOwnership | ClaimResolution::Settled { .. }) => {
                return Ok(None)
            }
            Ok(ClaimResolution::Running { .. } | ClaimResolution::ExpiredOrPruned) | Err(_) => {
                // Absence cannot prove that a timed-out write is not still in
                // flight. The original fixed lease deadline bounds this wait.
            }
        }
        tokio::time::sleep_until((Instant::now() + Duration::from_millis(250)).min(deadline)).await;
    }
    Ok(None)
}

/// Called by the operation owner only after its read/child join, or before
/// input was ever exposed. The proof write is durable before the exact row is
/// deleted, so restart replay can finish an unacknowledged DELETE.
pub(crate) async fn settle_joined_source_custody(
    store: &Arc<dyn Store>,
    receipt: plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken,
) {
    let _receipt_owner = crate::source_custody::lock_background_settlement(&receipt).await;
    loop {
        match crate::source_custody::persist_background_settlement(&receipt) {
            Ok(()) => break,
            Err(error) => tracing::warn!(custody_id=%receipt.custody_id,%error,
                "joined source journal remains pending"),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    loop {
        match store.mark_background_source_settled(receipt.clone()).await {
            Ok(false) => {
                if let Err(error) =
                    crate::source_custody::clear_background_settlement(store, &receipt)
                {
                    tracing::warn!(%error,"background source journal cleanup will replay");
                }
                return;
            }
            Ok(true) => break,
            Err(error) => tracing::warn!(custody_id=%receipt.custody_id,%error,
                "joined source proof remains pending"),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    loop {
        let deleted = store
            .release_background_source_custody(receipt.clone(), unix_ms().unwrap_or_default())
            .await;
        #[cfg(test)]
        let deleted = faults::after_commit(faults::SOURCE_RELEASE, deleted);
        match deleted {
            Ok(_) => {
                if let Err(error) =
                    crate::source_custody::clear_background_settlement(store, &receipt)
                {
                    tracing::warn!(%error,"background source journal cleanup will replay");
                }
                return;
            }
            Err(error) => tracing::warn!(custody_id=%receipt.custody_id,%error,
                "joined source deletion remains pending"),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Initial registration itself carries a durable no-reader proof. DELETE is
/// conditional on that proof, so a duplicate or ambiguously armed reader stays
/// charged instead of being mistaken for this unstarted operation.
pub(crate) async fn settle_no_reader_source_custody(
    store: &Arc<dyn Store>,
    receipt: plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken,
) {
    loop {
        match store
            .release_background_source_custody(receipt.clone(), unix_ms().unwrap_or_default())
            .await
        {
            Ok(_) => return,
            Err(error) => tracing::warn!(custody_id=%receipt.custody_id,%error,
                "no-reader source deletion remains pending"),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A registration error may be a committed insert with a lost acknowledgement.
/// Close the exact claim before resolving it, so no delayed write can create a
/// source reader after a no-reader receipt is released.
async fn settle_unstarted_job_source(
    store: &Arc<dyn Store>,
    owner: plurx_core::store::background_jobs_resources::BackgroundSourceOwner,
    mut receipt: Option<plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken>,
) {
    let plurx_core::store::background_jobs_resources::BackgroundSourceOwner::Job { token } = &owner
    else {
        return;
    };
    loop {
        let now_ms = unix_ms().unwrap_or_default();
        let resolution = store
            .resolve_claim(ResolveClaim {
                job_id: token.job_id.clone(),
                node_id: token.node_id.clone(),
                boot_id: token.boot_id.clone(),
                claim_id: token.claim_id.clone(),
                fence: Some(token.fence),
                dispatched_at_ms: now_ms,
                now_ms,
            })
            .await;
        let settlement = match resolution {
            Ok(ClaimResolution::Running { token }) => Some((
                token,
                JobSettlement::Yield {
                    error_code: Some("source_registration_unavailable".into()),
                    checkpoint: None,
                    not_before_ms: now_ms,
                },
            )),
            Ok(ClaimResolution::CancelRequested { cleanup_token }) => {
                Some((cleanup_token, JobSettlement::Cancel))
            }
            Ok(
                ClaimResolution::LostOwnership
                | ClaimResolution::Settled { .. }
                | ClaimResolution::ExpiredOrPruned,
            ) => break,
            Err(error) => {
                tracing::warn!(%error,job=%token.job_id,
                "unstarted source claim fencing will retry");
                None
            }
        };
        if let Some((token, settlement)) = settlement {
            if let Err(error) = store
                .settle_job(SettleJob {
                    token,
                    settlement,
                    now_ms,
                })
                .await
            {
                tracing::warn!(%error,"unstarted source claim settlement will retry");
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    loop {
        if let Some(held) = receipt.take() {
            settle_no_reader_source_custody(store, held).await;
            return;
        }
        match store.register_background_source_custody(owner.clone(), unix_ms().unwrap_or_default()).await {
            Ok(plurx_core::store::background_jobs_resources::BackgroundSourceRegistration::Registered(held)) => {
                receipt = Some(held);
            }
            Ok(plurx_core::store::background_jobs_resources::BackgroundSourceRegistration::NotRequired
                | plurx_core::store::background_jobs_resources::BackgroundSourceRegistration::LostOwnership) => return,
            Err(error) => tracing::warn!(%error,"unstarted source receipt resolution will retry"),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

impl ActiveBackgroundJob {
    pub(crate) async fn start(
        store: Arc<dyn Store>,
        authority: Arc<dyn ClusterJobAuthority>,
        token: JobToken,
        deadline: Instant,
        kind: plurx_core::store::background_jobs::JobKind,
    ) -> Result<Self, StoreError> {
        token.validate()?;
        if Instant::now() >= deadline {
            return Err(StoreError::Task(
                "background claim acknowledgement arrived after its deadline".to_owned(),
            ));
        }
        let owner = plurx_core::store::background_jobs_resources::BackgroundSourceOwner::Job {
            token: token.clone(),
        };
        let registration = store
            .register_background_source_custody(owner.clone(), unix_ms()?)
            .await;
        #[cfg(test)]
        let registration = faults::after_commit(faults::SOURCE_REGISTER, registration);
        let source_custody = match registration {
            Ok(plurx_core::store::background_jobs_resources::BackgroundSourceRegistration::Registered(receipt)) => Some(receipt),
            Ok(plurx_core::store::background_jobs_resources::BackgroundSourceRegistration::NotRequired) => None,
            Ok(plurx_core::store::background_jobs_resources::BackgroundSourceRegistration::LostOwnership) => {
                return Err(StoreError::Task("source custody claim unavailable before execution".into()));
            }
            Err(error) => {
                settle_unstarted_job_source(&store, owner, None).await;
                return Err(error);
            }
        };
        if Instant::now() >= deadline {
            settle_unstarted_job_source(&store, owner, source_custody).await;
            return Err(StoreError::Task(
                "source custody registration exceeded the claim deadline".into(),
            ));
        }
        if let Some(receipt) = &source_custody {
            if let Err(error) =
                crate::source_custody::activate_background_with_journal(&store, receipt.clone())
                    .await
            {
                settle_unstarted_job_source(&store, owner, source_custody).await;
                return Err(StoreError::Task(error));
            }
        }
        let lost = CancellationToken::new();
        let settle = CancellationToken::new();
        let source_budget = match &source_custody {
            Some(receipt) => SourceReadBudget::with_completion(
                Arc::clone(&store),
                receipt,
                lost.clone(),
                settle.clone(),
            ),
            None => SourceReadBudget::new(Arc::clone(&store), owner, lost.clone()),
        };
        let (deadline, _) = watch::channel(deadline);
        let fence = JobFence(Arc::new(Inner {
            kind,
            started: Instant::now(),
            store,
            authority,
            state: Mutex::new(ClaimState {
                token: Some(token),
                cancelling: false,
            }),
            deadline,
            lost,
            copy_output_completed: AtomicBool::new(false),
            source_custody,
            source_budget,
            settle,
        }));
        let stop = CancellationToken::new();
        let heartbeat_stop = stop.clone();
        let heartbeat_fence = fence.clone();
        let heartbeat = tokio::spawn(async move {
            let mut tick =
                tokio::time::interval(Duration::from_millis(JOB_RENEW_INTERVAL_MS as u64));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                let deadline = *heartbeat_fence.0.deadline.borrow();
                tokio::select! {
                    biased;
                    () = heartbeat_stop.cancelled() => break,
                    () = tokio::time::sleep_until(deadline) => {
                        heartbeat_fence.0.lost.cancel();
                        break;
                    }
                    _ = tick.tick() => {}
                }
                let renewal = heartbeat_fence.renew();
                tokio::pin!(renewal);
                let renewed = tokio::select! {
                    biased;
                    () = heartbeat_stop.cancelled() => {
                        // A dispatched write must finish before retirement
                        // samples its final token, even during shutdown.
                        let _ = renewal.await;
                        break;
                    }
                    () = tokio::time::sleep_until(deadline) => {
                        heartbeat_fence.0.lost.cancel();
                        let _ = renewal.await;
                        break;
                    }
                    result = &mut renewal => result,
                };
                if !matches!(renewed, Ok(true)) {
                    if let Err(error) = renewed {
                        tracing::warn!(%error, "background renewal lost authority");
                    }
                    heartbeat_fence.0.lost.cancel();
                    break;
                }
            }
        });
        Ok(Self {
            fence,
            stop,
            heartbeat: Some(heartbeat),
        })
    }

    pub(crate) fn fence(&self) -> JobFence {
        self.fence.clone()
    }

    /// Call only after the handler's child and output work have joined.
    pub(crate) async fn finish(mut self) {
        self.fence.0.lost.cancel();
        self.stop.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            if let Err(error) = heartbeat.await {
                tracing::warn!(%error, "joining background heartbeat");
            }
        }
        if let Err(error) = self.fence.retire().await {
            tracing::warn!(%error, "background retirement remains ambiguous");
        }
        if let Some(custody) = self.fence.0.source_custody.clone() {
            // `lost` is cancelled above, so enrollment is closed. An enrolled
            // producer's File clones, InputState holders and declared raw
            // children may outlive this call (a planning error can return while
            // a blocking read still runs); only the ledger may publish the
            // joined proof, after they physically retire.
            if self.fence.0.source_budget.completion_owns_settlement() {
                self.fence.0.settle.cancel();
            } else {
                settle_joined_source_custody(&self.fence.0.store, custody).await;
            }
        }
        metrics::execution(self.fence.0.kind, self.fence.0.started.elapsed());
    }
}

impl Drop for ActiveBackgroundJob {
    fn drop(&mut self) {
        self.fence.0.lost.cancel();
        self.stop.cancel();
        // Do not release a durable reservation here: a dropped caller is not
        // proof that its child exited. Physical custody survives expiry until
        // explicit join+finish. Dropping JoinHandle does not abort writes.
    }
}

impl JobFence {
    pub(crate) fn copy_output_completed(&self) -> bool {
        self.0.copy_output_completed.load(Ordering::Acquire)
    }
    pub(crate) fn source_budget(&self) -> Arc<SourceReadBudget> {
        Arc::clone(&self.0.source_budget)
    }

    pub(crate) fn loss_token(&self) -> CancellationToken {
        self.0.lost.clone()
    }

    pub(crate) async fn snapshot(&self) -> Option<JobToken> {
        if self.0.lost.is_cancelled() || Instant::now() >= *self.0.deadline.borrow() {
            return None;
        }
        let state = self.0.state.lock().await;
        if self.0.lost.is_cancelled() || Instant::now() >= *self.0.deadline.borrow() {
            return None;
        }
        state.token.clone()
    }

    /// The source controller can be driving the same future that holds
    /// `state` across a publication RPC. Waiting for that lock here would
    /// strand the publisher and its physical custody after the write commits.
    /// Source RPCs authenticate the original claim identity and recheck the
    /// current running state and expiry in the Store. Its renewable revision
    /// is required by publication, but is not part of physical source ownership.
    fn source_token(&self) -> Option<&JobToken> {
        if self.0.lost.is_cancelled() || Instant::now() >= *self.0.deadline.borrow() {
            return None;
        }
        self.0.source_budget.job_token()
    }

    pub(crate) async fn source_may_continue(&self) -> Result<bool, StoreError> {
        #[cfg(test)]
        faults::worker::at(faults::worker::Point::SourcePermissionCheck).await;
        let Some(token) = self.source_token() else {
            return Ok(false);
        };
        self.0
            .store
            .background_source_may_continue(
                plurx_core::store::background_jobs_resources::BackgroundSourceOwner::Job {
                    token: token.clone(),
                },
                unix_ms()?,
            )
            .await
    }

    /// Caller must hold a verified whole-group stop acknowledgement.
    #[cfg(target_os = "linux")]
    pub(crate) async fn pause_source(&self) -> Result<bool, StoreError> {
        let Some(token) = self.source_token().filter(|_| self.may_publish()) else {
            return Ok(false);
        };
        self.0
            .store
            .pause_background_source(token.clone(), unix_ms()?)
            .await
    }

    /// Source capacity is reacquired atomically before the caller sends CONT.
    #[cfg(target_os = "linux")]
    pub(crate) async fn resume_source(&self) -> Result<bool, StoreError> {
        if !self.0.authority.may_execute_job(self.0.kind).await {
            return Ok(false);
        }
        let Some(token) = self.source_token().filter(|_| self.may_publish()) else {
            return Ok(false);
        };
        self.0
            .store
            .resume_background_source(token.clone(), unix_ms()?)
            .await
    }

    pub(crate) async fn source_checkpoint_artifacts(
        &self,
        node_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        self.0.store.source_checkpoint_artifacts(node_id).await
    }

    /// The artifact must already be atomically durable. Serialize this update
    /// with renewal/settlement so its token cannot race our own heartbeat.
    pub(crate) async fn checkpoint(
        &self,
        checkpoint: serde_json::Value,
    ) -> Result<bool, StoreError> {
        let state = self.0.state.lock().await;
        if self.0.lost.is_cancelled()
            || state.cancelling
            || Instant::now() >= *self.0.deadline.borrow()
            || !self.0.authority.may_execute_job(self.0.kind).await
        {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        self.0
            .store
            .checkpoint_job(token, checkpoint, unix_ms()?)
            .await
    }

    async fn renew(&self) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if self.0.lost.is_cancelled() || !self.0.authority.may_execute_job(self.0.kind).await {
            return Ok(false);
        }
        let Some(current) = state.token.clone() else {
            return Ok(false);
        };
        let issued = Instant::now();
        let now_ms = unix_ms()?;
        metrics::event(self.0.kind, Event::RenewWrite);
        let renewed = self
            .0
            .store
            .renew_jobs(RenewJobs {
                tokens: vec![current.clone()],
                now_ms,
            })
            .await;
        #[cfg(test)]
        let renewed = faults::after_commit(faults::RENEW, renewed);
        let token = match renewed {
            Ok(outcomes) => match outcomes.into_iter().next() {
                Some(RenewOutcome::Renewed { token }) => Some(token),
                _ => None,
            },
            Err(_) => None,
        };
        let token = match token {
            Some(token) => token,
            None => match self
                .0
                .store
                .resolve_claim(ResolveClaim {
                    job_id: current.job_id,
                    node_id: current.node_id,
                    boot_id: current.boot_id,
                    claim_id: current.claim_id,
                    fence: Some(current.fence),
                    dispatched_at_ms: now_ms,
                    now_ms: unix_ms()?,
                })
                .await?
            {
                ClaimResolution::Running { token } => token,
                ClaimResolution::CancelRequested { cleanup_token } => {
                    state.token = Some(cleanup_token);
                    state.cancelling = true;
                    return Ok(false);
                }
                _ => {
                    state.token = None;
                    return Ok(false);
                }
            },
        };
        let remaining = token
            .lease_expires_ms
            .saturating_sub(now_ms)
            .clamp(0, JOB_LEASE_MS);
        state.token = Some(token);
        if self.0.lost.is_cancelled() || Instant::now() >= *self.0.deadline.borrow() {
            return Ok(false);
        }
        let deadline = issued + Duration::from_millis(remaining as u64);
        self.0.deadline.send_replace(deadline);
        Ok(Instant::now() < deadline)
    }

    pub(crate) async fn settle(&self, settlement: JobSettlement) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if self.0.lost.is_cancelled() || Instant::now() >= *self.0.deadline.borrow() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let event = settlement_event(&settlement);
        let result = self
            .0
            .store
            .settle_job(SettleJob {
                token,
                settlement,
                now_ms: unix_ms()?,
            })
            .await?;
        if result {
            metrics::event(self.0.kind, event);
            state.token = None;
        }
        Ok(result)
    }

    pub(crate) async fn write_subtitle(
        &self,
        request: &plurx_core::store::AnalysisRequest,
        output: plurx_core::store::background_jobs_subtitle::SubtitleJobWrite,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let complete = matches!(
            output,
            plurx_core::store::background_jobs_subtitle::SubtitleJobWrite::Complete { .. }
        );
        let mut write = plurx_core::store::background_jobs_subtitle::WriteSubtitleJob {
            token,
            request: request.clone(),
            output,
            now_ms: unix_ms()?,
        };
        let result = match self.0.store.write_subtitle_job(write.clone()).await {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                write.now_ms = unix_ms()?;
                self.0.store.write_subtitle_job(write).await?
            }
        };
        if result && complete {
            state.token = None;
        }
        Ok(result)
    }

    pub(crate) async fn bind_library(
        &self,
        publisher: &plurx_core::store::PublicationStore<'_>,
    ) -> Result<bool, StoreError> {
        let state = self.0.state.lock().await;
        if !self.0.authority.may_run_cluster_jobs().await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        publisher.bind_library_job(token, unix_ms()?).await
    }

    pub(crate) async fn complete_library(
        &self,
        publisher: &plurx_core::store::PublicationStore<'_>,
        request_id: String,
        result: plurx_core::store::background_jobs_library::LibraryWorkResult,
    ) -> Result<bool, StoreError> {
        let state = self.0.state.lock().await;
        if !self.0.authority.may_run_cluster_jobs().await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let completed = publisher
            .complete_library_work(token, request_id, result, unix_ms()?)
            .await;
        #[cfg(test)]
        let completed = {
            faults::worker::at(faults::worker::Point::CompletionCommitted).await;
            faults::after_commit(faults::COMPLETE, completed)
        };
        completed
    }

    pub(crate) async fn bind_transcode_recipe(
        &self,
        recipe_hash: &str,
    ) -> Result<bool, StoreError> {
        let state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        match self
            .0
            .store
            .bind_transcode_job_recipe(token.clone(), recipe_hash, unix_ms()?)
            .await
        {
            Ok(bound) => Ok(bound),
            Err(error) => {
                if !self.may_publish() {
                    return Err(error);
                }
                self.0
                    .store
                    .bind_transcode_job_recipe(token, recipe_hash, unix_ms()?)
                    .await
            }
        }
    }

    pub(crate) async fn publish_copy_output(
        &self,
        intent: plurx_core::store::background_jobs::CopyOutputIntent,
        output: plurx_core::store::background_jobs::CopyOutputJobOutput,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if self.0.kind != plurx_core::store::background_jobs::JobKind::CopyOutputPrepare
            || !self.0.authority.may_execute_job(self.0.kind).await
            || !self.may_publish()
        {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let mut request = plurx_core::store::background_jobs::PublishCopyOutputJob {
            token,
            intent,
            output,
            now_ms: unix_ms()?,
        };
        let result = match self.0.store.publish_copy_output_job(request.clone()).await {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_copy_output_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        if published {
            self.0.copy_output_completed.store(true, Ordering::Release);
        }
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn publish_encoded_output(
        &self,
        intent: plurx_core::store::background_jobs::EncodedOutputIntent,
        output: plurx_core::store::background_jobs::CopyOutputJobOutput,
    ) -> Result<bool, StoreError> {
        use plurx_core::store::background_jobs::{JobKind, PublishEncodedOutputJob};
        let mut state = self.0.state.lock().await;
        if self.0.kind != JobKind::EncodedOutputPrepare
            || !self.0.authority.may_execute_job(self.0.kind).await
            || !self.may_publish()
        {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let mut request = PublishEncodedOutputJob {
            token,
            intent,
            output,
            now_ms: unix_ms()?,
        };
        let result = match self
            .0
            .store
            .publish_encoded_output_job(request.clone())
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_encoded_output_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        if published {
            self.0.copy_output_completed.store(true, Ordering::Release);
            state.token = None;
        }
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        Ok(published)
    }

    pub(crate) async fn publish_transcode(
        &self,
        output: TranscodeJobOutput,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        };
        let mut request = PublishTranscodeJob {
            token,
            output,
            now_ms: unix_ms()?,
        };
        let reply = self.0.store.publish_transcode_job(request.clone()).await;
        #[cfg(test)]
        let reply = faults::after_commit(faults::PUBLISH, reply);
        let result = match reply {
            Ok(result) => result,
            // Retry the exact publication once; the Store recognizes its
            // committed result even when the original token is now terminal.
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_transcode_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn publish_fragment(
        &self,
        artifact: plurx_core::store::ClusterFragmentIndexArtifact,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        };
        let mut request = plurx_core::store::background_jobs::PublishFragmentJob {
            token,
            artifact,
            now_ms: unix_ms()?,
        };
        let reply = self.0.store.publish_fragment_job(request.clone()).await;
        #[cfg(test)]
        let reply = faults::after_commit(faults::PUBLISH, reply);
        let result = match reply {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_fragment_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn publish_artwork(
        &self,
        mut location: plurx_core::store::background_jobs_artwork::ArtworkLocation,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        };
        let now_ms = unix_ms()?;
        location.verified_at_ms = now_ms;
        let mut request = plurx_core::store::background_jobs_artwork::PublishArtworkJob {
            token,
            location,
            now_ms,
        };
        let reply = self.0.store.publish_artwork_job(request.clone()).await;
        #[cfg(test)]
        let reply = faults::after_commit(faults::PUBLISH, reply);
        let result = match reply {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                request.location.verified_at_ms = request.now_ms;
                self.0.store.publish_artwork_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn observe_subtitle(
        &self,
        phase: plurx_core::store::background_jobs_transcription::SubtitlePhase,
    ) -> Result<bool, StoreError> {
        let state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        self.0
            .store
            .observe_subtitle_job(
                plurx_core::store::background_jobs_transcription::ObserveSubtitleJob {
                    token,
                    phase,
                    completed_cues: None,
                    total_cues: None,
                    now_ms: unix_ms()?,
                },
            )
            .await
    }

    pub(crate) async fn publish_text_slice(
        &self,
        source: plurx_core::store::background_jobs::JobPayload,
        cues: Vec<plurx_core::store::subtitle_text_translation::TextTranslationCue>,
    ) -> Result<bool, StoreError> {
        let state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        self.0
            .store
            .publish_text_translation_slice(
                plurx_core::store::subtitle_text_translation::PublishTextSlice {
                    token,
                    source,
                    cues,
                    now_ms: unix_ms()?,
                },
            )
            .await
    }

    pub(crate) async fn observe_text_progress(
        &self,
        phase: plurx_core::store::background_jobs_transcription::SubtitlePhase,
        completed: u32,
        total: u32,
    ) -> Result<bool, StoreError> {
        let state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        self.0
            .store
            .observe_subtitle_job(
                plurx_core::store::background_jobs_transcription::ObserveSubtitleJob {
                    token,
                    phase,
                    completed_cues: Some(completed),
                    total_cues: Some(total),
                    now_ms: unix_ms()?,
                },
            )
            .await
    }

    pub(crate) async fn bind_translation_source(
        &self,
        source: plurx_core::store::background_jobs::JobPayload,
        source_sha256: String,
        local_identity: String,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let mut request =
            plurx_core::store::subtitle_translation_publication::BindTranslationSource {
                token,
                source,
                source_sha256,
                local_identity,
                now_ms: unix_ms()?,
            };
        let result = match self.0.store.bind_translation_source(request.clone()).await {
            Ok(result) => result,
            Err(error) => {
                if !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.bind_translation_source(request).await?
            }
        };
        if result {
            state.token = None;
        }
        Ok(result)
    }
    pub(crate) async fn publish_translation_segment(
        &self,
        source: plurx_core::store::background_jobs::JobPayload,
        segment: plurx_core::store::subtitle_translation::SegmentDraft,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let mut request =
            plurx_core::store::subtitle_translation_publication::PublishTranslationSegment {
                token,
                source,
                segment,
                now_ms: unix_ms()?,
            };
        let result = match self
            .0
            .store
            .publish_translation_segment(request.clone())
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_translation_segment(request).await?
            }
        };
        if result {
            state.token = None;
        }
        Ok(result)
    }

    pub(crate) async fn publish_subtitle_model(
        &self,
        source: plurx_core::store::background_jobs::JobPayload,
        receipt: plurx_core::store::background_jobs_transcription::SubtitleModelReceipt,
    ) -> Result<bool, StoreError> {
        self.publish_qualified_subtitle_model(source, receipt, None)
            .await
    }

    pub(crate) async fn publish_translation_model(
        &self,
        source: plurx_core::store::background_jobs::JobPayload,
        receipt: plurx_core::store::background_jobs_transcription::SubtitleModelReceipt,
        translation: plurx_core::store::background_jobs_transcription::TranslationQualification,
    ) -> Result<bool, StoreError> {
        self.publish_qualified_subtitle_model(source, receipt, Some(translation))
            .await
    }
    async fn publish_qualified_subtitle_model(
        &self,
        source: plurx_core::store::background_jobs::JobPayload,
        mut receipt: plurx_core::store::background_jobs_transcription::SubtitleModelReceipt,
        translation: Option<
            plurx_core::store::background_jobs_transcription::TranslationQualification,
        >,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let now_ms = unix_ms()?;
        receipt.node_id = token.node_id.clone();
        receipt.boot_id = token.boot_id.clone();
        receipt.job_id = token.job_id.clone();
        receipt.fence = token.fence;
        receipt.verified_at_ms = now_ms;
        let mut request = plurx_core::store::background_jobs_transcription::PublishSubtitleModel {
            translation,
            token,
            source,
            receipt,
            now_ms,
        };
        let result = match self.0.store.publish_subtitle_model(request.clone()).await {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_subtitle_model(request).await?
            }
        };
        let published = result;
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn publish_transcription(
        &self,
        source: plurx_core::store::background_jobs::JobPayload,
        caption: plurx_core::domain::DownloadedSubtitle,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        };
        let now_ms = unix_ms()?;
        let mut request = plurx_core::store::background_jobs_transcription::PublishTranscription {
            token,
            source,
            caption,
            now_ms,
        };
        let reply = self
            .0
            .store
            .publish_transcription_job(request.clone())
            .await;
        #[cfg(test)]
        let reply = faults::after_commit(faults::PUBLISH, reply);
        let result = match reply {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_transcription_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn publish_embedding(
        &self,
        artifact: plurx_core::store::background_jobs_embeddings::SharedEmbedding,
        source_json: String,
        classification_revision: i64,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        };
        let now_ms = unix_ms()?;
        let mut request = plurx_core::store::background_jobs_embeddings::PublishEmbeddingJob {
            token,
            artifact,
            source_json,
            classification_revision,
            now_ms,
        };
        let reply = self.0.store.publish_embedding_job(request.clone()).await;
        #[cfg(test)]
        let reply = faults::after_commit(faults::PUBLISH, reply);
        let result = match reply {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_embedding_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    #[cfg(target_os = "linux")]
    pub(crate) async fn publish_dv_title(
        &self,
        output: plurx_core::store::dv_title_jobs::TitleOutput,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let mut request = plurx_core::store::dv_title_jobs::PublishTitleJob {
            token,
            output,
            now_ms: unix_ms()?,
        };
        let result = match self.0.store.publish_dv_title_job(request.clone()).await {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_dv_title_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn publish_probe(
        &self,
        output: plurx_core::store::background_jobs_probe::ProbeOutput,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        };
        let now_ms = unix_ms()?;
        let mut request = plurx_core::store::background_jobs_probe::PublishProbeJob {
            token,
            output,
            now_ms,
        };
        let reply = self.0.store.publish_probe_job(request.clone()).await;
        #[cfg(test)]
        let reply = faults::after_commit(faults::PUBLISH, reply);
        let result = match reply {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.publish_probe_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn verify_transcode(
        &self,
        candidate: plurx_core::store::background_jobs_integrity::TranscodeVerificationCandidate,
        valid: bool,
        next_object_index: i64,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let mut request = plurx_core::store::background_jobs_integrity::VerifyTranscode {
            token,
            recipe_hash: candidate.recipe_hash,
            manifest_digest: candidate.manifest_digest,
            relative_dir: candidate.relative_dir,
            publication_generation: candidate.publication_generation,
            valid,
            next_object_index,
            now_ms: unix_ms()?,
        };
        let reply = self.0.store.verify_transcode_job(request.clone()).await;
        #[cfg(test)]
        let reply = faults::after_commit(faults::PUBLISH, reply);
        let result = match reply {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.verify_transcode_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn verify_artwork(
        &self,
        location: plurx_core::store::background_jobs_artwork::ArtworkLocation,
        valid: bool,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
            metrics::event(self.0.kind, Event::FencedPublication);
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let mut request = plurx_core::store::background_jobs_integrity::VerifyArtwork {
            token,
            location,
            valid,
            now_ms: unix_ms()?,
        };
        let reply = self.0.store.verify_artwork_job(request.clone()).await;
        #[cfg(test)]
        let reply = faults::after_commit(faults::PUBLISH, reply);
        let result = match reply {
            Ok(result) => result,
            Err(error) => {
                if !self.0.authority.may_execute_job(self.0.kind).await || !self.may_publish() {
                    return Err(error);
                }
                request.now_ms = unix_ms()?;
                self.0.store.verify_artwork_job(request).await?
            }
        };
        let published = matches!(
            result,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        );
        metrics::event(
            self.0.kind,
            if published {
                Event::Published
            } else {
                Event::FencedPublication
            },
        );
        if published {
            state.token = None;
        }
        Ok(published)
    }

    pub(crate) async fn fail_fragment(
        &self,
        code: plurx_core::content_analysis::IndexFailureCode,
        transient_allowlisted: bool,
        diagnostic: plurx_core::content_analysis::IndexDiagnostic,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if self.0.lost.is_cancelled() || Instant::now() >= *self.0.deadline.borrow() {
            return Ok(false);
        }
        let Some(token) = state.token.clone() else {
            return Ok(false);
        };
        let result = self
            .0
            .store
            .fail_fragment_job(plurx_core::store::background_jobs::FragmentJobFailure {
                token,
                code,
                transient_allowlisted,
                diagnostic,
                now_ms: unix_ms()?,
            })
            .await?;
        if result {
            metrics::event(self.0.kind, Event::ChargedFailure);
            state.token = None;
        }
        Ok(result)
    }

    fn may_publish(&self) -> bool {
        !self.0.lost.is_cancelled()
            && self
                .0
                .deadline
                .borrow()
                .saturating_duration_since(Instant::now())
                > PUBLICATION_MARGIN
    }

    async fn retire(&self) -> Result<(), StoreError> {
        let mut state = self.0.state.lock().await;
        let Some(token) = state.token.take() else {
            return Ok(());
        };
        let now_ms = unix_ms()?;
        let settlement = if state.cancelling {
            JobSettlement::Cancel
        } else {
            JobSettlement::Yield {
                error_code: Some("worker_retired".into()),
                checkpoint: None,
                not_before_ms: now_ms,
            }
        };
        let event = settlement_event(&settlement);
        if matches!(
            self.0
                .store
                .settle_job(SettleJob {
                    token: token.clone(),
                    settlement,
                    now_ms
                })
                .await,
            Ok(true)
        ) {
            metrics::event(self.0.kind, event);
            return Ok(());
        }
        // Cancellation or an unacknowledged renewal may have advanced only
        // this attempt's revision. Resolve that identity, never a successor.
        let resolution = self
            .0
            .store
            .resolve_claim(ResolveClaim {
                job_id: token.job_id,
                node_id: token.node_id,
                boot_id: token.boot_id,
                claim_id: token.claim_id,
                fence: Some(token.fence),
                dispatched_at_ms: now_ms,
                now_ms: unix_ms()?,
            })
            .await?;
        let now_ms = unix_ms()?;
        let request = match resolution {
            ClaimResolution::Running { token } => Some(SettleJob {
                token,
                settlement: JobSettlement::Yield {
                    error_code: Some("worker_retired".into()),
                    checkpoint: None,
                    not_before_ms: now_ms,
                },
                now_ms,
            }),
            ClaimResolution::CancelRequested { cleanup_token } => Some(SettleJob {
                token: cleanup_token,
                settlement: JobSettlement::Cancel,
                now_ms,
            }),
            _ => None,
        };
        if let Some(request) = request {
            let event = settlement_event(&request.settlement);
            if self.0.store.settle_job(request).await? {
                metrics::event(self.0.kind, event);
            }
        }
        Ok(())
    }
}

/// Find a compatible candidate without letting an unreadable high-priority
/// item hide every lower item. Keyset pages bound each read; the active-row
/// cap bounds a complete pass. Admission is held through ambiguous claims.
pub(crate) enum PreparationClaim {
    Encoded(
        plurx_core::store::background_jobs::BackgroundJob,
        ActiveBackgroundJob,
        tokio::sync::OwnedSemaphorePermit,
    ),
    Transcode(
        plurx_core::domain::PretranscodeJob,
        ActiveBackgroundJob,
        crate::transcode::PretranscodeFence,
    ),
    Copy(
        plurx_core::store::background_jobs::BackgroundJob,
        ActiveBackgroundJob,
        crate::transcode::FragmentAdmission,
    ),
}

/// Which preparation kinds one pass may claim, and what each may occupy.
///
/// The speculative lane is the discovery schedule's (`jobs.cache_produce_mins`)
/// and occupies the pre-transcode cache; the output lanes are the
/// `vod.output_preparation` switch's and occupy the retained registry. One
/// loop runs both, but neither may open the other: until 2026-10-04 the loop
/// ran only when the schedule was on, so viewer-demand rows had no executor
/// on any node that left discovery at "never".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PreparationLanes {
    pub(crate) speculative: bool,
    pub(crate) output: crate::vodserve::OutputPreparation,
}

impl PreparationLanes {
    #[cfg(test)]
    pub(crate) const ALL: Self = Self {
        speculative: true,
        output: crate::vodserve::OutputPreparation::CopyAndEncoded,
    };

    pub(crate) fn kinds(self) -> Vec<plurx_core::store::background_jobs::JobKind> {
        let mut kinds = Vec::with_capacity(3);
        if self.speculative {
            kinds.push(plurx_core::store::background_jobs::JobKind::TranscodePrepare);
        }
        kinds.extend_from_slice(self.output.job_kinds());
        kinds
    }

    pub(crate) fn is_empty(self) -> bool {
        self.kinds().is_empty()
    }
}

#[allow(clippy::too_many_arguments)] // one claim's complete admission inputs
pub(crate) async fn claim_pretranscode(
    store: Arc<dyn Store>,
    authority: Arc<dyn ClusterJobAuthority>,
    transcode: &crate::transcode::TranscodeManager,
    node: &str,
    allowed: &[plurx_core::store::background_jobs::JobKind],
    capabilities: &plurx_core::domain::PretranscodeWorkerCapabilities,
    speculative_capacity: i64,
    output_capacity: i64,
    excluded: &[String],
) -> Result<Option<PreparationClaim>, StoreError> {
    // Each lane is bounded by the store it publishes into: speculative
    // generations by the pre-transcode cache, output preparations by the
    // retained registry. Physical scratch bounds both.
    let mut speculative_capabilities = capabilities.clone();
    speculative_capabilities.scratch_bytes = capabilities.scratch_bytes.min(speculative_capacity);
    use plurx_core::store::background_jobs::{
        CandidateQuery, JobKind, JobPayload, MAX_ACTIVE_JOBS, MAX_PAGE_SIZE,
    };
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let boot = BOOT.get_or_init(|| uuid::Uuid::new_v4().to_string());
    let mut kinds = Vec::new();
    for kind in [
        JobKind::TranscodePrepare,
        JobKind::CopyOutputPrepare,
        JobKind::EncodedOutputPrepare,
    ] {
        if allowed.contains(&kind) && authority.may_execute_job(kind).await {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        return Ok(None);
    }
    let mut cursor = None;
    for _ in 0..MAX_ACTIVE_JOBS.div_ceil(MAX_PAGE_SIZE) {
        let page = store
            .job_candidates(CandidateQuery {
                node_id: node.into(),
                kinds: kinds.clone(),
                after: cursor,
                now_ms: unix_ms()?,
                limit: MAX_PAGE_SIZE,
            })
            .await?;
        for candidate in page.jobs {
            if candidate.payload_version != 1 || excluded.contains(&candidate.id) {
                continue;
            }
            if let Some(checkpoint) = &candidate.checkpoint {
                if checkpoint["staging_node_id"]
                    .as_str()
                    .is_some_and(|preferred| preferred != node)
                    && checkpoint["prefer_until_ms"]
                        .as_i64()
                        .is_some_and(|until| until > unix_ms().unwrap_or(i64::MAX))
                {
                    continue;
                }
            }
            let Ok(payload) = candidate.supported_payload() else {
                continue;
            };
            if let JobPayload::EncodedOutputPrepare {
                intent,
                scratch_bytes,
                ..
            } = &payload
            {
                // An output row is bounded by the retained registry it
                // publishes into, not by the pre-transcode cache budget.
                if !kinds.contains(&JobKind::EncodedOutputPrepare)
                    || intent.target_node_id != node
                    || *scratch_bytes > capabilities.scratch_bytes.min(output_capacity)
                {
                    continue;
                }
                let Some(admission) = transcode.admit_encoded_preparation() else {
                    return Ok(None);
                };
                if !authority
                    .may_execute_job(JobKind::EncodedOutputPrepare)
                    .await
                {
                    continue;
                }
                let now_ms = unix_ms()?;
                let request = ClaimJob {
                    job_id: candidate.id.clone(),
                    expected_revision: candidate.revision,
                    node_id: node.into(),
                    boot_id: boot.clone(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::EncodedOutputPrepare,
                    payload_version: 1,
                    now_ms,
                    dispatched_at_ms: now_ms,
                };
                let Some((job, deadline)) =
                    claim_with_resolution(store.as_ref(), &candidate, request).await?
                else {
                    continue;
                };
                let active = ActiveBackgroundJob::start(
                    Arc::clone(&store),
                    Arc::clone(&authority),
                    job.token.clone().ok_or_else(|| {
                        StoreError::Task("claimed encoded job has no ownership token".into())
                    })?,
                    deadline,
                    JobKind::EncodedOutputPrepare,
                )
                .await?;
                return Ok(Some(PreparationClaim::Encoded(job, active, admission)));
            }
            if let JobPayload::CopyOutputPrepare {
                intent,
                scratch_bytes,
                ..
            } = &payload
            {
                if !kinds.contains(&JobKind::CopyOutputPrepare)
                    || intent.target_node_id != node
                    || *scratch_bytes > capabilities.scratch_bytes.min(output_capacity)
                {
                    continue;
                }
                let Some(admission) = transcode.admit_fragment().await else {
                    return Ok(None);
                };
                if !authority.may_execute_job(JobKind::CopyOutputPrepare).await {
                    continue;
                }
                let now_ms = unix_ms()?;
                let request = ClaimJob {
                    job_id: candidate.id.clone(),
                    expected_revision: candidate.revision,
                    node_id: node.into(),
                    boot_id: boot.clone(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::CopyOutputPrepare,
                    payload_version: 1,
                    now_ms,
                    dispatched_at_ms: now_ms,
                };
                let Some((job, deadline)) =
                    claim_with_resolution(store.as_ref(), &candidate, request).await?
                else {
                    continue;
                };
                let active = ActiveBackgroundJob::start(
                    Arc::clone(&store),
                    Arc::clone(&authority),
                    job.token.clone().ok_or_else(|| {
                        StoreError::Task("claimed copy job has no ownership token".into())
                    })?,
                    deadline,
                    JobKind::CopyOutputPrepare,
                )
                .await?;
                return Ok(Some(PreparationClaim::Copy(job, active, admission)));
            }
            let JobPayload::TranscodePrepare {
                requirements,
                target_height,
                file_id,
                ..
            } = &payload
            else {
                continue;
            };
            if !kinds.contains(&JobKind::TranscodePrepare) {
                continue;
            }
            if speculative_capabilities.scratch_bytes <= 0
                || !speculative_capabilities.validate()
                || !requirements.compatible_with(&speculative_capabilities)
                || i64::from(*target_height) > speculative_capabilities.max_target_height
            {
                continue;
            }
            let Some(file) = store.get_file(*file_id).await? else {
                continue;
            };
            let admission = match transcode
                .admit_pretranscode(&file, i64::from(*target_height))
                .await
            {
                Ok(Some(admission)) => admission,
                Ok(None) => return Ok(None),
                Err(error) => {
                    tracing::debug!(job = candidate.id, %error, "candidate cannot use this encoder");
                    continue;
                }
            };
            if !authority.may_execute_job(JobKind::TranscodePrepare).await {
                return Ok(None);
            }
            let now_ms = unix_ms()?;
            let request = ClaimJob {
                job_id: candidate.id.clone(),
                expected_revision: candidate.revision,
                node_id: node.into(),
                boot_id: boot.clone(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::TranscodePrepare,
                payload_version: 1,
                now_ms,
                dispatched_at_ms: now_ms,
            };
            let Some((job, deadline)) =
                claim_with_resolution(store.as_ref(), &candidate, request).await?
            else {
                continue;
            };
            let projection = plurx_core::store::background_jobs_pretranscode::projection(&job)?;
            let active = ActiveBackgroundJob::start(
                Arc::clone(&store),
                Arc::clone(&authority),
                job.token
                    .ok_or_else(|| StoreError::Task("claimed job has no ownership token".into()))?,
                deadline,
                JobKind::TranscodePrepare,
            )
            .await?;
            let fence = crate::transcode::PretranscodeFence::new(
                projection.clone(),
                active.fence(),
                admission,
            );
            return Ok(Some(PreparationClaim::Transcode(projection, active, fence)));
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(None)
}

/// A pipeline is a worker capability, not an authority-wide version verdict.
pub(crate) struct FragmentPipeline<'a> {
    pub(crate) engine_sha256: &'a str,
    pub(crate) have_dovi: bool,
    pub(crate) convert_dolby_vision: bool,
}

async fn fragment_candidate_compatible(
    store: &dyn Store,
    payload: &plurx_core::store::background_jobs::JobPayload,
    worker: &FragmentPipeline<'_>,
) -> Result<bool, StoreError> {
    use plurx_core::store::background_jobs::JobPayload;
    let JobPayload::FragmentIndexBuild {
        file_id,
        source_size,
        source_mtime,
        pipeline_digest,
        ..
    } = payload
    else {
        return Ok(true); // Hydration consumes verified bytes, not this encoder.
    };
    let Some(file) = store.get_file(*file_id).await? else {
        return Ok(true);
    };
    if file.size != *source_size || file.mtime != *source_mtime {
        return Ok(true); // Let the claimed handler settle globally stale input.
    }
    let identities = crate::state::fragment_index_video_identity_options(
        store,
        &file,
        worker.have_dovi,
        worker.convert_dolby_vision,
    )
    .await?;
    Ok(identities.into_iter().any(|(video, _)| {
        crate::fragment_index_cluster::pipeline_digest(&file, worker.engine_sha256, video)
            == *pipeline_digest
    }))
}

/// Reserve this node's actual indexing capacity before acquiring a durable lease.
pub(crate) async fn claim_fragment(
    store: Arc<dyn Store>,
    authority: Arc<dyn ClusterJobAuthority>,
    transcode: &crate::transcode::TranscodeManager,
    node: &str,
    excluded: &[String],
    worker: &FragmentPipeline<'_>,
) -> Result<
    Option<(
        plurx_core::store::ClusterFragmentIndexJob,
        ActiveBackgroundJob,
        crate::transcode::FragmentAdmission,
    )>,
    StoreError,
> {
    use plurx_core::store::background_jobs::{
        CandidateQuery, JobKind, JobPayload, MAX_ACTIVE_JOBS, MAX_PAGE_SIZE,
    };
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let boot = BOOT.get_or_init(|| uuid::Uuid::new_v4().to_string());
    let mut eligible_kinds = Vec::new();
    for kind in [JobKind::FragmentIndexBuild, JobKind::ArtifactHydrate] {
        if authority.may_execute_job(kind).await {
            eligible_kinds.push(kind);
        }
    }
    if eligible_kinds.is_empty() {
        return Ok(None);
    }
    let mut cursor = None;
    for _ in 0..MAX_ACTIVE_JOBS.div_ceil(MAX_PAGE_SIZE) {
        let page = store
            .job_candidates(CandidateQuery {
                node_id: node.into(),
                kinds: eligible_kinds.clone(),
                after: cursor,
                now_ms: unix_ms()?,
                limit: MAX_PAGE_SIZE,
            })
            .await?;
        for candidate in page.jobs {
            let Ok(payload) = candidate.supported_payload() else {
                continue;
            };
            if !fragment_candidate_compatible(store.as_ref(), &payload, worker).await? {
                continue;
            }
            let (key, kind, artifact) = match &payload {
                JobPayload::FragmentIndexBuild { cache_key, .. } => {
                    (cache_key.clone(), JobKind::FragmentIndexBuild, None)
                }
                JobPayload::ArtifactHydrate {
                    artifact_key,
                    target_node_id,
                } if target_node_id == node => {
                    let Some(key) = artifact_key.strip_prefix("fragment:") else {
                        continue;
                    };
                    (
                        key.into(),
                        JobKind::ArtifactHydrate,
                        store.cluster_fragment_index_artifact(key).await?,
                    )
                }
                _ => continue,
            };
            if excluded.contains(&key) {
                continue;
            }
            let Some(admission) = transcode.admit_fragment().await else {
                return Ok(None);
            };
            if !authority.may_execute_job(kind).await {
                continue;
            }
            let now_ms = unix_ms()?;
            let request = ClaimJob {
                job_id: candidate.id.clone(),
                expected_revision: candidate.revision,
                node_id: node.into(),
                boot_id: boot.clone(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind,
                payload_version: 1,
                now_ms,
                dispatched_at_ms: now_ms,
            };
            let Some((job, deadline)) =
                claim_with_resolution(store.as_ref(), &candidate, request).await?
            else {
                continue;
            };
            let projection = match plurx_core::store::background_jobs_fragment_admission::projection(
                &job,
                artifact.as_ref(),
            ) {
                Ok(projection) => projection,
                Err(error) => {
                    if let Some(token) = job.token {
                        store
                            .settle_job(SettleJob {
                                token,
                                settlement: JobSettlement::Fail {
                                    error_code: "artifact_metadata_missing".into(),
                                },
                                now_ms: unix_ms()?,
                            })
                            .await?;
                    }
                    tracing::warn!(%error, "durable delivery metadata disappeared before execution");
                    continue;
                }
            };
            let active = ActiveBackgroundJob::start(
                Arc::clone(&store),
                Arc::clone(&authority),
                job.token
                    .ok_or_else(|| StoreError::Task("claimed fragment has no token".into()))?,
                deadline,
                kind,
            )
            .await?;
            return Ok(Some((projection, active, admission)));
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(None)
}

/// The queued builds of a fragment's other copy identities: same file, same
/// attested source, a pipeline in `pipelines` (shared-read §5.5).
///
/// Walks the same candidate pages [`claim_fragment`] reads — no new store
/// query. With `claim` false it only reports which are queued, so the lead's
/// pass can build them from its one read. With `claim` true, which the worker
/// does once the lead has settled and released its source reservation, each
/// is CAS-claimed under a fence and heartbeat of its own, exactly as a claim
/// of its own would be, so its prebuilt index is published under that fence.
/// A candidate another worker took first, or one in `excluded`, is left where
/// it is; it is still built the old way.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fragment_siblings(
    store: Arc<dyn Store>,
    authority: Arc<dyn ClusterJobAuthority>,
    node: &str,
    lead: &plurx_core::store::ClusterFragmentIndexJob,
    pipelines: &[String],
    excluded: &[String],
    worker: &FragmentPipeline<'_>,
    claim: bool,
) -> Result<
    Vec<(
        plurx_core::store::ClusterFragmentIndexJob,
        Option<ActiveBackgroundJob>,
    )>,
    StoreError,
> {
    use plurx_core::store::background_jobs::{
        CandidateQuery, JobKind, JobPayload, MAX_ACTIVE_JOBS, MAX_PAGE_SIZE,
    };
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let boot = BOOT.get_or_init(|| uuid::Uuid::new_v4().to_string());
    let mut found: Vec<(
        plurx_core::store::ClusterFragmentIndexJob,
        Option<ActiveBackgroundJob>,
    )> = Vec::new();
    if pipelines.is_empty() || !authority.may_execute_job(JobKind::FragmentIndexBuild).await {
        return Ok(found);
    }
    let mut cursor = None;
    for _ in 0..MAX_ACTIVE_JOBS.div_ceil(MAX_PAGE_SIZE) {
        let page = store
            .job_candidates(CandidateQuery {
                node_id: node.into(),
                kinds: vec![JobKind::FragmentIndexBuild],
                after: cursor,
                now_ms: unix_ms()?,
                limit: MAX_PAGE_SIZE,
            })
            .await?;
        for candidate in page.jobs {
            if found.len() >= pipelines.len() {
                return Ok(found);
            }
            let Ok(payload) = candidate.supported_payload() else {
                continue;
            };
            let JobPayload::FragmentIndexBuild {
                file_id,
                source_sha256,
                cache_key,
                pipeline_digest,
                ..
            } = &payload
            else {
                continue;
            };
            if *file_id != lead.file_id
                || *source_sha256 != lead.source_sha256
                || *cache_key == lead.cache_key
                || !pipelines.contains(pipeline_digest)
                || found
                    .iter()
                    .any(|(job, _)| job.pipeline_sha256 == *pipeline_digest)
                || excluded.contains(cache_key)
                || !fragment_candidate_compatible(store.as_ref(), &payload, worker).await?
            {
                continue;
            }
            if !claim {
                found.push((
                    plurx_core::store::background_jobs_fragment_admission::projection(
                        &candidate, None,
                    )?,
                    None,
                ));
                continue;
            }
            let now_ms = unix_ms()?;
            let request = ClaimJob {
                job_id: candidate.id.clone(),
                expected_revision: candidate.revision,
                node_id: node.into(),
                boot_id: boot.clone(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::FragmentIndexBuild,
                payload_version: 1,
                now_ms,
                dispatched_at_ms: now_ms,
            };
            let Some((job, deadline)) =
                claim_with_resolution(store.as_ref(), &candidate, request).await?
            else {
                continue;
            };
            let Some(token) = job.token.clone() else {
                continue;
            };
            let projection = match plurx_core::store::background_jobs_fragment_admission::projection(
                &job, None,
            ) {
                Ok(projection) => projection,
                Err(error) => {
                    store
                        .settle_job(SettleJob {
                            token,
                            settlement: JobSettlement::Fail {
                                error_code: "artifact_metadata_missing".into(),
                            },
                            now_ms: unix_ms()?,
                        })
                        .await?;
                    tracing::warn!(%error, "sibling fragment metadata disappeared before publication");
                    continue;
                }
            };
            let active = ActiveBackgroundJob::start(
                Arc::clone(&store),
                Arc::clone(&authority),
                token,
                deadline,
                JobKind::FragmentIndexBuild,
            )
            .await?;
            found.push((projection, Some(active)));
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(found)
}

/// Bounded, node-local eligibility observations. They never settle durable work:
/// another member may have a readable mount while this one does not.
#[derive(Default)]
pub(crate) struct LibraryReadiness {
    problems: std::sync::Mutex<std::collections::BTreeMap<i64, LibraryReadinessProblem>>,
    advertised: std::sync::Mutex<std::collections::BTreeMap<String, LibraryTurnObservation>>,
    interactive_waiting: AtomicBool,
}

/// How long a root-check failure stays a statement about this node if its
/// library worker loses authority or stops checking that library.
const LIBRARY_READINESS_TTL: Duration = Duration::from_secs(120);
const LIBRARY_TURN_REFRESH: Duration = Duration::from_secs(3);

struct LibraryTurnObservation {
    revision: i64,
    capacity: plurx_core::store::background_jobs::LibraryTurnCapacity,
    recorded: Instant,
    interactive: bool,
}

#[derive(Clone)]
struct LibraryReadinessProblem {
    roots: Vec<std::path::PathBuf>,
    message: String,
    observed: std::time::Instant,
}
impl LibraryReadiness {
    pub(crate) fn poll_interval(&self) -> Duration {
        if self.interactive_waiting.load(Ordering::Relaxed) {
            Duration::from_millis(750)
        } else {
            Duration::from_secs(5)
        }
    }

    /// Cache only the permission to request a turn. The durable claim always
    /// rechecks interest, eligibility and the complete set of shared resources.
    async fn advertise(
        &self,
        store: &dyn Store,
        candidate: &BackgroundJob,
        node: &str,
        roots: &[std::path::PathBuf],
        capacity: plurx_core::store::background_jobs::LibraryTurnCapacity,
    ) -> Result<bool, StoreError> {
        let now = Instant::now();
        {
            let mut recent = self
                .advertised
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            recent.retain(|_, row| {
                now.saturating_duration_since(row.recorded) < LIBRARY_TURN_REFRESH
            });
            if let Some(row) = recent
                .get(&candidate.id)
                .filter(|row| row.revision == candidate.revision && row.capacity == capacity)
            {
                return Ok(row.interactive);
            }
        }
        let interactive = store
            .advertise_library_turn(
                &candidate.id,
                candidate.revision,
                node,
                roots,
                capacity,
                unix_ms()?,
            )
            .await?;
        self.advertised
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                candidate.id.clone(),
                LibraryTurnObservation {
                    revision: candidate.revision,
                    capacity,
                    recorded: now,
                    interactive,
                },
            );
        Ok(interactive)
    }

    fn record(&self, library_id: i64, roots: &[std::path::PathBuf], problem: Option<String>) {
        let mut rows = self
            .problems
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(problem) = problem {
            if rows.len() >= 128 && !rows.contains_key(&library_id) {
                rows.pop_first();
            }
            rows.insert(
                library_id,
                LibraryReadinessProblem {
                    roots: roots.to_vec(),
                    message: problem.chars().take(512).collect(),
                    observed: std::time::Instant::now(),
                },
            );
        } else {
            rows.remove(&library_id);
        }
    }

    /// Compare against the current catalogue, including edits made on another
    /// node. A late check of old roots must not revive a superseded diagnosis.
    /// One batched read only when there are cached failures; no filesystem I/O
    /// or per-library store reads are added to the Activity request path.
    pub(crate) async fn current_problems(
        &self,
        store: &dyn Store,
    ) -> std::collections::HashMap<i64, String> {
        self.current_problems_at(store, std::time::Instant::now())
            .await
    }

    async fn current_problems_at(
        &self,
        store: &dyn Store,
        now: std::time::Instant,
    ) -> std::collections::HashMap<i64, String> {
        let observations: std::collections::BTreeMap<i64, LibraryReadinessProblem> = self
            .problems
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, problem)| {
                now.saturating_duration_since(problem.observed) < LIBRARY_READINESS_TTL
            })
            .map(|(id, problem)| (*id, problem.clone()))
            .collect();
        if observations.is_empty() {
            return std::collections::HashMap::new();
        }
        let Ok(libraries) = store.list_libraries().await else {
            // Without current configuration, the old diagnosis is unverified.
            return std::collections::HashMap::new();
        };
        libraries
            .into_iter()
            .filter_map(|library| {
                let observed = observations.get(&library.id)?;
                (observed.roots == library.paths).then(|| (library.id, observed.message.clone()))
            })
            .collect()
    }
}

async fn library_candidate_readiness(
    store: &dyn Store,
    authority: &dyn ClusterJobAuthority,
    transcode: &crate::transcode::TranscodeManager,
    node: &str,
    library_filter: Option<i64>,
    readiness: &LibraryReadiness,
    candidate: &BackgroundJob,
) -> Result<Option<LibraryCandidateReadiness>, StoreError> {
    use plurx_core::store::background_jobs::{JobPayload, LibraryTurnCapacity};
    use plurx_core::store::background_jobs_library::{
        LIBRARY_TARGETED_GENERATION, LIBRARY_WORK_GENERATION,
    };
    let Ok(payload) = candidate.supported_payload() else {
        return Ok(None);
    };
    let kind = payload.kind();
    let (library_id, targeted) = match payload {
        JobPayload::LibraryScan {
            library_id,
            generation,
        }
        | JobPayload::MetadataRefresh {
            library_id,
            generation,
        } if generation == LIBRARY_WORK_GENERATION => (library_id, false),
        JobPayload::LibraryScan {
            library_id,
            generation,
        } if generation == LIBRARY_TARGETED_GENERATION => (library_id, true),
        _ => return Ok(None),
    };
    if library_filter.is_some_and(|wanted| wanted != library_id) {
        return Ok(None);
    }
    let library = store.get_library(library_id).await?;
    // A deleted library still needs its accepted requests settled.
    let mut problem = library
        .as_ref()
        .filter(|library| library.paths.is_empty())
        .map(|_| {
            "This node observed no library roots; waiting for a compatible worker.".to_owned()
        });
    let mut unreadable = None;
    for path in library.iter().flat_map(|library| &library.paths) {
        if let Err(error) = tokio::fs::read_dir(path).await {
            problem = Some(format!("This node could not read library root {}: {error}. Waiting for a compatible worker.", path.display()));
            unreadable = Some(path.clone());
            break;
        }
    }
    let readable = problem.is_none();
    readiness.record(
        library_id,
        library
            .as_ref()
            .map_or(&[], |library| library.paths.as_slice()),
        problem,
    );
    if !readable {
        if let Some(root) = unreadable.filter(|_| targeted) {
            note_targeted_wait(
                store,
                &candidate.id,
                &format!("root_unreadable:{}", root.display()),
            )
            .await;
        }
        return Ok(None);
    }
    if !authority.may_run_cluster_jobs().await {
        return Ok(None);
    }
    let protected = transcode.background_yield_reason().is_some();
    let capacity = if protected {
        LibraryTurnCapacity::Unavailable
    } else if transcode.fragment_worker_may_start() {
        LibraryTurnCapacity::Available
    } else if transcode.background_worker_in_use() {
        LibraryTurnCapacity::HeavyOccupied
    } else {
        LibraryTurnCapacity::Unavailable
    };
    let interactive = match library.as_ref() {
        Some(library) if candidate.priority >= 2 => {
            readiness
                .advertise(store, candidate, node, &library.paths, capacity)
                .await?
        }
        _ => false,
    };
    if protected && targeted {
        // Playback protection holds it, not the fence: drop a reason an
        // earlier pass wrote that may no longer be true.
        refresh_targeted_wait(store, &candidate.id, library_id, library.as_ref()).await;
    }
    Ok((!protected).then_some(LibraryCandidateReadiness {
        kind,
        interactive,
        targeted,
        library_id,
    }))
}

/// What one readable library candidate needs from the claim: its kind,
/// whether it holds an interactive dispatch turn, and whether it is a
/// contract v2 targeted request.
struct LibraryCandidateReadiness {
    kind: plurx_core::store::background_jobs::JobKind,
    interactive: bool,
    targeted: bool,
    library_id: i64,
}

async fn library_candidates(
    store: &dyn Store,
    node: &str,
    after: Option<plurx_core::store::background_jobs::CandidateCursor>,
) -> Result<plurx_core::store::background_jobs::CandidatePage, StoreError> {
    use plurx_core::store::background_jobs::{CandidateQuery, JobKind, MAX_PAGE_SIZE};
    store
        .job_candidates(CandidateQuery {
            node_id: node.into(),
            kinds: vec![JobKind::LibraryScan, JobKind::MetadataRefresh],
            after,
            now_ms: unix_ms()?,
            limit: MAX_PAGE_SIZE,
        })
        .await
}

/// Keep other readable libraries in line for the next source turn while the
/// only compatible worker is still executing its current library scan.
pub(crate) async fn advertise_library_successors(
    store: &dyn Store,
    authority: &dyn ClusterJobAuthority,
    transcode: &crate::transcode::TranscodeManager,
    node: &str,
    readiness: &LibraryReadiness,
) -> Result<(), StoreError> {
    use plurx_core::store::background_jobs::{MAX_ACTIVE_JOBS, MAX_PAGE_SIZE};
    if !authority.may_run_cluster_jobs().await {
        return Ok(());
    }
    let mut cursor = None;
    for _ in 0..MAX_ACTIVE_JOBS.div_ceil(MAX_PAGE_SIZE) {
        let page = library_candidates(store, node, cursor).await?;
        for candidate in page.jobs.iter().filter(|candidate| candidate.priority >= 2) {
            library_candidate_readiness(
                store, authority, transcode, node, None, readiness, candidate,
            )
            .await?;
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(())
}

/// Library coordinators share the same physical heavy-work admission as
/// preparation/indexing. Source checks happen before claiming; domain-lease
/// contention is resolved by the caller through an uncharged yield.
pub(crate) async fn claim_library(
    store: Arc<dyn Store>,
    authority: Arc<dyn ClusterJobAuthority>,
    transcode: &crate::transcode::TranscodeManager,
    node: &str,
    library_filter: Option<i64>,
    readiness: &LibraryReadiness,
) -> Result<
    Option<(
        BackgroundJob,
        ActiveBackgroundJob,
        crate::transcode::FragmentAdmission,
    )>,
    StoreError,
> {
    use plurx_core::store::background_jobs::{MAX_ACTIVE_JOBS, MAX_PAGE_SIZE};
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let boot = BOOT.get_or_init(|| uuid::Uuid::new_v4().to_string());
    readiness
        .interactive_waiting
        .store(false, Ordering::Relaxed);
    if !authority.may_run_cluster_jobs().await {
        return Ok(None);
    }
    let mut cursor = None;
    for _ in 0..MAX_ACTIVE_JOBS.div_ceil(MAX_PAGE_SIZE) {
        let page = library_candidates(store.as_ref(), node, cursor).await?;
        for candidate in page.jobs {
            let Some(LibraryCandidateReadiness {
                kind,
                interactive,
                targeted,
                library_id,
            }) = library_candidate_readiness(
                store.as_ref(),
                authority.as_ref(),
                transcode,
                node,
                library_filter,
                readiness,
                &candidate,
            )
            .await?
            else {
                continue;
            };
            // A targeted request runs under a probe admission: it is never
            // refused because the heavy lane is busy (contract v2 §5.1).
            let admission = if targeted {
                Some(crate::transcode::FragmentAdmission::probe_only())
            } else if interactive {
                readiness.interactive_waiting.store(true, Ordering::Relaxed);
                tokio::time::timeout(
                    Duration::from_secs(1),
                    transcode.admit_interactive_library(),
                )
                .await
                .ok()
                .flatten()
            } else {
                transcode.admit_fragment().await
            };
            let Some(admission) = admission else {
                return Ok(None);
            };
            if !authority.may_run_cluster_jobs().await {
                return Ok(None);
            }
            let now_ms = unix_ms()?;
            let Some((job, deadline)) = claim_with_resolution(
                store.as_ref(),
                &candidate,
                ClaimJob {
                    job_id: candidate.id.clone(),
                    expected_revision: candidate.revision,
                    node_id: node.into(),
                    boot_id: boot.clone(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind,
                    payload_version: 1,
                    now_ms,
                    dispatched_at_ms: now_ms,
                },
            )
            .await?
            else {
                if targeted {
                    // Refused while still queued — the provider cap, or a
                    // claim this pass raced — rather than lost to another
                    // worker: say what it waits on now, not what an earlier
                    // pass saw.
                    let queued = store
                        .background_job(&candidate.id)
                        .await?
                        .is_some_and(|job| {
                            job.state == plurx_core::store::background_jobs::JobState::Queued
                        });
                    if queued {
                        let library = store.get_library(library_id).await?;
                        refresh_targeted_wait(
                            store.as_ref(),
                            &candidate.id,
                            library_id,
                            library.as_ref(),
                        )
                        .await;
                    }
                }
                continue;
            };
            let active = ActiveBackgroundJob::start(
                Arc::clone(&store),
                Arc::clone(&authority),
                job.token
                    .clone()
                    .ok_or_else(|| StoreError::Task("library claim has no owner".into()))?,
                deadline,
                kind,
            )
            .await?;
            return Ok(Some((job, active, admission)));
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(None)
}

/// A targeted candidate this pass could not claim for a reason other than an
/// unreadable root (playback protection, the provider cap, a raced claim).
/// Its one named wait is its own library's fence: `full_scan:<library>`
/// while that lease is visibly live, otherwise nothing, so a reason written
/// by an earlier fence refusal or root check does not outlive it. The lease
/// read is this node's local hint and the write is advisory and idempotent.
async fn refresh_targeted_wait(
    store: &dyn Store,
    job_id: &str,
    library_id: i64,
    library: Option<&plurx_core::domain::Library>,
) {
    let Ok(now_ms) = unix_ms() else {
        return;
    };
    let fenced = match store
        .lease_expiry_hint(&format!("scan:library:{library_id}"))
        .await
    {
        Ok(expires) => expires.is_some_and(|expires| expires > now_ms),
        Err(error) => {
            tracing::warn!(%error, job_id, "reading the library fence for waiting_on");
            return;
        }
    };
    let waiting_on = if fenced {
        format!(
            "full_scan:{}",
            library.map_or_else(|| library_id.to_string(), |library| library.name.clone())
        )
    } else {
        String::new()
    };
    note_targeted_wait(store, job_id, &waiting_on).await;
}

/// Record, on every pending request of a targeted library job, what it is
/// waiting on. Advisory: the request record is still the authority, so a
/// failed write is logged and the claim decision stands.
pub(crate) async fn note_targeted_wait(store: &dyn Store, job_id: &str, waiting_on: &str) {
    let now_ms = match unix_ms() {
        Ok(now_ms) => now_ms,
        Err(error) => {
            tracing::warn!(%error, job_id, "no clock for targeted waiting_on");
            return;
        }
    };
    if let Err(error) = store
        .set_library_job_waiting_on(job_id, waiting_on, now_ms)
        .await
    {
        tracing::warn!(%error, job_id, waiting_on, "recording targeted waiting_on");
    }
}

/// The caller holds physical admission through execution and cleanup.
pub(crate) async fn claim_subtitle(
    store: Arc<dyn Store>,
    authority: Arc<dyn ClusterJobAuthority>,
    node: &str,
    request_filter: Option<&str>,
    pipeline_digest: &str,
) -> Result<Option<(BackgroundJob, ActiveBackgroundJob)>, StoreError> {
    use plurx_core::store::background_jobs::{
        CandidateQuery, JobKind, JobPayload, MAX_ACTIVE_JOBS, MAX_PAGE_SIZE,
    };
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let boot = BOOT.get_or_init(|| uuid::Uuid::new_v4().to_string());
    if !authority.may_execute_job(JobKind::SubtitleExtract).await {
        return Ok(None);
    }
    let mut cursor = None;
    for _ in 0..MAX_ACTIVE_JOBS.div_ceil(MAX_PAGE_SIZE) {
        let page = store
            .job_candidates(CandidateQuery {
                node_id: node.into(),
                kinds: vec![JobKind::SubtitleExtract],
                after: cursor,
                now_ms: unix_ms()?,
                limit: MAX_PAGE_SIZE,
            })
            .await?;
        for candidate in page.jobs {
            let Ok(JobPayload::SubtitleExtract {
                file_id,
                source_generation,
                track: None,
                pipeline_digest: expected,
            }) = candidate.supported_payload()
            else {
                continue;
            };
            if expected != pipeline_digest
                || request_filter.is_some_and(|wanted| wanted != source_generation)
            {
                continue;
            }
            let Some(file) = store.get_file(file_id).await? else {
                continue;
            };
            if tokio::fs::File::open(&file.path).await.is_err() {
                continue;
            }
            if !authority.may_execute_job(JobKind::SubtitleExtract).await {
                return Ok(None);
            }
            let now_ms = unix_ms()?;
            let Some((job, deadline)) = claim_with_resolution(
                store.as_ref(),
                &candidate,
                ClaimJob {
                    job_id: candidate.id.clone(),
                    expected_revision: candidate.revision,
                    node_id: node.into(),
                    boot_id: boot.clone(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::SubtitleExtract,
                    payload_version: 1,
                    now_ms,
                    dispatched_at_ms: now_ms,
                },
            )
            .await?
            else {
                continue;
            };
            let active = ActiveBackgroundJob::start(
                Arc::clone(&store),
                Arc::clone(&authority),
                job.token
                    .clone()
                    .ok_or_else(|| StoreError::Task("subtitle claim has no owner".into()))?,
                deadline,
                JobKind::SubtitleExtract,
            )
            .await?;
            return Ok(Some((job, active)));
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use plurx_core::cluster::coordination::UnclusteredJobAuthority;
    use plurx_core::store::background_jobs::{
        CancelJob, EnqueueJob, JobKind, JobPayload, JobRequest, JobState,
    };
    use plurx_core::store::SqliteStore;

    /// Co-claim (shared-read §5.5): the lead's queued siblings — same file,
    /// same attested source, a pipeline of the file — are claimed under fences
    /// of their own; another file's build, a pipeline the caller did not ask
    /// for, and a sibling already claimed are left alone.
    #[tokio::test]
    async fn sibling_builds_of_the_same_source_are_found_and_claimed_by_their_own_fences() {
        use plurx_core::domain::{
            DolbyVisionFacts, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult,
        };
        use plurx_core::store::NewClusterFragmentIndexJob;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "co-claim".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/media".into()],
                anime: false,
            })
            .await
            .expect("library");
        let mut files = Vec::new();
        for title in ["lead", "other"] {
            let item = store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: title.into(),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("item");
            let id = store
                .upsert_file(
                    item,
                    &format!("/media/{title}.mkv"),
                    4_096,
                    100,
                    &ProbeResult {
                        video_codec: Some("hevc".into()),
                        duration_ms: Some(1_000),
                        hdr: Some("dolby_vision".into()),
                        dolby_vision: DolbyVisionFacts {
                            profile: Some(7),
                            level: Some(6),
                            bl_compat_id: Some(6),
                            el_present: Some(true),
                            rpu_present: Some(true),
                        },
                        ..Default::default()
                    },
                )
                .await
                .expect("file");
            files.push(store.get_file(id).await.expect("read").expect("file"));
        }
        let engine = "cd".repeat(32);
        let source = "ab".repeat(32);
        let now = unix_ms().expect("clock");
        let worker = FragmentPipeline {
            engine_sha256: &engine,
            have_dovi: true,
            convert_dolby_vision: true,
        };
        let mut keys = Vec::new();
        for file in &files {
            let identities = crate::state::fragment_index_video_identity_options(
                store.as_ref(),
                file,
                true,
                true,
            )
            .await
            .expect("identities");
            assert_eq!(identities.len(), 3);
            let mut file_keys = Vec::new();
            for (video, _) in identities {
                let pipeline = crate::fragment_index_cluster::pipeline_digest(file, &engine, video);
                let cache_key = plurx_core::store::cluster_fragment_index_key(
                    file.id, file.size, file.mtime, &source, &pipeline,
                )
                .expect("key");
                assert!(store
                    .enqueue_cluster_fragment_index(&NewClusterFragmentIndexJob {
                        cache_key: cache_key.clone(),
                        file_id: file.id,
                        source_size: file.size,
                        source_mtime: file.mtime,
                        source_sha256: source.clone(),
                        pipeline_sha256: pipeline.clone(),
                        priority: "normal".into(),
                        trigger: "background".into(),
                        target_node_id: "node-a".into(),
                        not_before_ms: now,
                        created_at_ms: now,
                    })
                    .await
                    .expect("enqueue"));
                file_keys.push((cache_key, pipeline));
            }
            keys.push(file_keys);
        }
        let lead = store
            .cluster_fragment_index_job(&keys[0][0].0, "node-a")
            .await
            .expect("read")
            .expect("lead job");
        let authority: Arc<dyn ClusterJobAuthority> = Arc::new(UnclusteredJobAuthority);
        // Every enqueued build is a candidate this worker can execute.
        let page = store
            .job_candidates(plurx_core::store::background_jobs::CandidateQuery {
                node_id: "node-a".into(),
                kinds: vec![JobKind::FragmentIndexBuild],
                after: None,
                now_ms: unix_ms().expect("clock"),
                limit: plurx_core::store::background_jobs::MAX_PAGE_SIZE,
            })
            .await
            .expect("candidates");
        assert_eq!(page.jobs.len(), 6, "every enqueued build is a candidate");
        for candidate in &page.jobs {
            let payload = candidate.supported_payload().expect("payload");
            assert!(
                fragment_candidate_compatible(store.as_ref(), &payload, &worker)
                    .await
                    .expect("compatible"),
                "{payload:?}"
            );
        }
        assert!(authority.may_execute_job(JobKind::FragmentIndexBuild).await);
        // Only the converted identity is asked for: the preserved one stays.
        // Pipelines name the recipe, not the file, so the other file's
        // converted build has the same pipeline and must still be left alone.
        assert_eq!(keys[0][2].1, keys[1][2].1);
        let asked = vec![keys[0][2].1.clone()];
        // A look claims nothing.
        let queued = fragment_siblings(
            Arc::clone(&store),
            Arc::clone(&authority),
            "node-a",
            &lead,
            &asked,
            &[],
            &worker,
            false,
        )
        .await
        .expect("look");
        assert_eq!(
            queued
                .iter()
                .map(|(job, active)| (job.cache_key.clone(), active.is_some()))
                .collect::<Vec<_>>(),
            [(keys[0][2].0.clone(), false)],
            "only the lead file's asked-for sibling, unclaimed"
        );
        let claimed = fragment_siblings(
            Arc::clone(&store),
            Arc::clone(&authority),
            "node-a",
            &lead,
            &asked,
            &[],
            &worker,
            true,
        )
        .await
        .expect("co-claim");
        assert_eq!(
            claimed
                .iter()
                .map(|(job, _)| job.cache_key.clone())
                .collect::<Vec<_>>(),
            [keys[0][2].0.clone()],
            "only the lead file's asked-for sibling"
        );
        // While that claim holds the storage domain's background source
        // slot, the preserved sibling cannot be claimed beside it — the
        // second slot is the store's for live demand. That is why the worker
        // claims and settles siblings one at a time.
        let beside = fragment_siblings(
            Arc::clone(&store),
            Arc::clone(&authority),
            "node-a",
            &lead,
            &[keys[0][1].1.clone()],
            &[],
            &worker,
            true,
        )
        .await
        .expect("claim beside");
        assert!(beside.is_empty(), "a second background reader was admitted");
        // Claimed once: a second walk finds nothing left to take.
        let again = fragment_siblings(
            Arc::clone(&store),
            Arc::clone(&authority),
            "node-a",
            &lead,
            &asked,
            &[],
            &worker,
            true,
        )
        .await
        .expect("co-claim again");
        assert!(again.is_empty());
        for (_, active) in claimed {
            active.expect("claimed").finish().await;
        }
    }

    #[tokio::test]
    async fn library_readiness_rejects_observations_of_superseded_roots() {
        use plurx_core::domain::{LibraryKind, NewLibrary};
        use plurx_core::store::LibraryStore;
        let store = SqliteStore::open_in_memory().expect("store");
        let old = NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/old/movies".into()],
            anime: false,
        };
        let library = store.create_library(&old).await.expect("library");
        let readiness = LibraryReadiness::default();
        readiness.record(library.id, &old.paths, Some("old root unavailable".into()));
        assert_eq!(
            readiness.current_problems(&store).await.get(&library.id),
            Some(&"old root unavailable".to_owned())
        );

        // A direct Store edit also models configuration written through another
        // cluster member: no local HTTP invalidation callback is involved.
        let new = NewLibrary {
            paths: vec!["/new/movies".into()],
            ..old.clone()
        };
        store
            .update_library(library.id, &new)
            .await
            .expect("update");
        assert!(readiness.current_problems(&store).await.is_empty());
        // A filesystem check issued before the edit may finish after it.
        readiness.record(library.id, &old.paths, Some("late old failure".into()));
        assert!(readiness.current_problems(&store).await.is_empty());
        readiness.record(library.id, &new.paths, Some("new root unavailable".into()));
        assert_eq!(
            readiness.current_problems(&store).await.get(&library.id),
            Some(&"new root unavailable".to_owned())
        );
        readiness.record(library.id, &new.paths, None);
        assert!(readiness.current_problems(&store).await.is_empty());
    }

    /// A node that stops running the library loop stops re-checking. Its last
    /// failure must age out rather than tell Activity forever that a scan
    /// cannot start while other members run it.
    #[tokio::test]
    async fn library_readiness_forgets_an_observation_nobody_renewed() {
        use plurx_core::domain::{LibraryKind, NewLibrary};
        use plurx_core::store::LibraryStore;
        let store = SqliteStore::open_in_memory().expect("store");
        let new = NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/missing/movies".into()],
            anime: false,
        };
        let library = store.create_library(&new).await.expect("library");
        let readiness = LibraryReadiness::default();
        readiness.record(library.id, &new.paths, Some("root unavailable".into()));
        let now = std::time::Instant::now();
        assert_eq!(
            readiness
                .current_problems_at(&store, now + LIBRARY_READINESS_TTL - Duration::from_secs(1))
                .await
                .get(&library.id),
            Some(&"root unavailable".to_owned()),
            "a failure renewed within the TTL is still current"
        );
        assert!(
            readiness
                .current_problems_at(&store, now + LIBRARY_READINESS_TTL)
                .await
                .is_empty(),
            "an unrenewed failure ages out"
        );
        readiness.record(
            library.id,
            &new.paths,
            Some("root still unavailable".into()),
        );
        assert_eq!(
            readiness.current_problems(&store).await.get(&library.id),
            Some(&"root still unavailable".to_owned()),
            "a fresh check renews it"
        );
    }

    #[tokio::test]
    async fn preparation_refusals_settle_by_class_not_as_unbounded_yields() {
        let (store, id, active) = active().await;
        let job = store.background_job(&id).await.expect("job").expect("row");
        let now = 1_000_000_i64;
        let (settlement, reason) = PreparationError::Fail("encoded_policy_changed").settlement(
            &job,
            "copy_output_failed",
            false,
            now,
        );
        assert!(matches!(settlement, JobSettlement::Fail { ref error_code }
            if error_code == "encoded_policy_changed"));
        assert_eq!(reason, "failed");
        let (settlement, reason) = PreparationError::from("store unavailable").settlement(
            &job,
            "copy_output_failed",
            false,
            now,
        );
        let expected = now + retry_delay_ms(&job.id, job.failed_attempts);
        assert!(
            matches!(settlement, JobSettlement::Retry { ref error_code, not_before_ms }
            if error_code == "copy_output_failed" && not_before_ms == expected),
            "a transient fault consumes an attempt through the shared backoff"
        );
        assert_eq!(reason, "retry");
        let (settlement, _) = PreparationError::from("io while a viewer attached").settlement(
            &job,
            "copy_output_failed",
            true,
            now,
        );
        assert!(
            matches!(settlement, JobSettlement::Yield { .. }),
            "observed preemption never charges an attempt"
        );
        let (settlement, _) = PreparationError::Stop("source_changed").settlement(
            &job,
            "copy_output_failed",
            true,
            now,
        );
        assert!(
            matches!(settlement, JobSettlement::Stop { .. }),
            "a deterministic refusal is not softened by concurrent preemption"
        );
        active.finish().await;
    }

    #[tokio::test]
    async fn copy_watchdog_bounds_pending_operation_and_distinguishes_historical_completion() {
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let (_store, _id, active) = active().await;
        let fence = active.fence();
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(Arc::clone(&dropped));
        let pending = async move {
            let _guard = guard;
            std::future::pending::<()>().await;
            Ok(true)
        };
        assert!(watch_copy_preparation(
            &fence,
            Instant::now() + Duration::from_millis(20),
            || true,
            pending
        )
        .await
        .is_err());
        assert!(
            dropped.load(Ordering::Acquire),
            "deadline drops private operation before caller settlement"
        );

        let checks = std::sync::atomic::AtomicUsize::new(0);
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(Arc::clone(&dropped));
        assert!(watch_copy_preparation(
            &fence,
            Instant::now() + Duration::from_secs(1),
            || checks.fetch_add(1, Ordering::Relaxed) == 0,
            async move {
                let _guard = guard;
                std::future::pending::<()>().await;
                Ok(true)
            }
        )
        .await
        .is_err());
        assert!(
            dropped.load(Ordering::Acquire),
            "changed observational predicate drops unfinished body"
        );

        fence.0.lost.cancel();
        assert!(
            watch_copy_preparation(
                &fence,
                Instant::now() + Duration::from_secs(1),
                || true,
                async { Ok(true) }
            )
            .await
            .is_err(),
            "unacknowledged loss must refuse"
        );
        // Model only the watchdog's phase signal, not SQL success or artifact
        // authority. The real publication consumer independently covers those.
        fence.0.copy_output_completed.store(true, Ordering::Release);
        assert!(watch_copy_preparation(
            &fence,
            Instant::now() + Duration::from_secs(1),
            || true,
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(true)
            }
        )
        .await
        .expect("historical completion is not own-lease loss"));
        active.finish().await;
    }

    #[tokio::test]
    async fn source_checkpoint_keeps_ownership_revision_and_survives_yield_without_accepting_stale_writers(
    ) {
        let (store, id, active) = active().await;
        let fence = active.fence();
        let token = fence.snapshot().await.expect("current ownership");
        let reference = serde_json::json!({"version":1,"node_id":"node-a","artifact":format!("{}.json", "a".repeat(64)),"offset":65});
        assert!(fence
            .checkpoint(serde_json::json!({"source_attestation":reference}))
            .await
            .expect("checkpoint write"));
        assert_eq!(fence.snapshot().await.expect("still owned"), token);
        let mut stale = token.clone();
        stale.fence += 1;
        assert!(!store
            .checkpoint_job(
                stale,
                serde_json::json!({"source_attestation":null}),
                unix_ms().expect("clock")
            )
            .await
            .expect("stale fence"));
        assert!(fence
            .settle(JobSettlement::Yield {
                error_code: Some("foreground_preempted".into()),
                checkpoint: Some(serde_json::json!({"staging_node_id":"node-a"})),
                not_before_ms: unix_ms().expect("clock")
            })
            .await
            .expect("yield"));
        let job = store
            .background_job(&id)
            .await
            .expect("job read")
            .expect("job");
        assert_eq!(
            job.checkpoint.as_ref().expect("queued checkpoint")["source_attestation"],
            reference
        );
        assert_eq!(
            job.checkpoint.as_ref().expect("queued checkpoint")["staging_node_id"],
            "node-a"
        );
        assert!(!store
            .checkpoint_job(
                token,
                serde_json::json!({"source_attestation":null}),
                unix_ms().expect("clock")
            )
            .await
            .expect("settled writer"));
        assert_eq!(
            store
                .source_checkpoint_artifacts("node-a")
                .await
                .expect("owned artifacts"),
            vec![format!("{}.json", "a".repeat(64))]
        );
        active.finish().await;
    }

    pub(crate) async fn active() -> (Arc<dyn Store>, String, ActiveBackgroundJob) {
        active_with_deadline(None).await
    }

    async fn active_with_deadline(
        local_deadline: Option<Duration>,
    ) -> (Arc<dyn Store>, String, ActiveBackgroundJob) {
        active_with_authority(local_deadline, Arc::new(UnclusteredJobAuthority)).await
    }

    async fn active_with_authority(
        local_deadline: Option<Duration>,
        authority: Arc<dyn ClusterJobAuthority>,
    ) -> (Arc<dyn Store>, String, ActiveBackgroundJob) {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let (id, active) = claimed_on(Arc::clone(&store), local_deadline, authority).await;
        (store, id, active)
    }

    /// The exact source custody a fixture job registered.
    #[cfg(target_os = "linux")]
    pub(crate) fn source_custody(
        active: &ActiveBackgroundJob,
    ) -> Option<plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken> {
        active.fence.0.source_custody.clone()
    }

    /// A real claimed job on a caller-configured Store (e.g. one whose exact
    /// source-custody backend is enabled).
    #[cfg(target_os = "linux")]
    pub(crate) async fn active_on(store: Arc<dyn Store>) -> (String, ActiveBackgroundJob) {
        claimed_on(store, None, Arc::new(UnclusteredJobAuthority)).await
    }

    async fn claimed_on(
        store: Arc<dyn Store>,
        local_deadline: Option<Duration>,
        authority: Arc<dyn ClusterJobAuthority>,
    ) -> (String, ActiveBackgroundJob) {
        let library = store
            .create_library(&plurx_core::domain::NewLibrary {
                name: "worker fault fixture".into(),
                kind: plurx_core::domain::LibraryKind::Movies,
                paths: vec!["/worker-fault".into()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&plurx_core::domain::NewItem {
                library_id: library.id,
                kind: plurx_core::domain::ItemKind::Movie,
                parent_id: None,
                title: "worker fault fixture".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file_id = store
            .upsert_file(item, "/worker-fault/movie.mkv", 100, 1, &Default::default())
            .await
            .expect("file");
        let cache_key = plurx_core::store::cluster_fragment_index_key(
            file_id,
            100,
            1,
            &"d".repeat(64),
            &"a".repeat(64),
        )
        .expect("key");
        let id = uuid::Uuid::new_v4().to_string();
        let now_ms = unix_ms().expect("clock");
        store
            .enqueue_job(EnqueueJob {
                id: id.clone(),
                payload: JobPayload::FragmentIndexBuild {
                    file_id,
                    source_generation: "source:1".into(),
                    source_size: 100,
                    source_mtime: 1,
                    source_sha256: "d".repeat(64),
                    cache_key,
                    pipeline_digest: "a".repeat(64),
                },
                dedupe_key: "fragment:1".into(),
                priority: 1,
                not_before_ms: now_ms,
                now_ms,
                request: JobRequest {
                    scope: "test:worker".into(),
                    request_id: id.clone(),
                    request_digest: "b".repeat(64),
                    consumer_kind: "analysis".into(),
                    consumer_ref: "1".into(),
                    target_node_id: None,
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .expect("enqueue");
        let candidate = store.background_job(&id).await.expect("read").expect("job");
        let (job, deadline) = claim_with_resolution(
            store.as_ref(),
            &candidate,
            ClaimJob {
                job_id: id.clone(),
                expected_revision: 0,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::FragmentIndexBuild,
                payload_version: 1,
                now_ms,
                dispatched_at_ms: now_ms,
            },
        )
        .await
        .expect("claim")
        .expect("owned");
        let active = ActiveBackgroundJob::start(
            Arc::clone(&store),
            authority,
            job.token.expect("token"),
            local_deadline.map_or(deadline, |duration| Instant::now() + duration),
            JobKind::FragmentIndexBuild,
        )
        .await
        .expect("start");
        (id, active)
    }

    struct ArtifactAuthority(std::sync::atomic::AtomicBool);
    #[plurx_core::cluster::coordination::cluster_job_async_trait]
    impl ClusterJobAuthority for ArtifactAuthority {
        async fn may_run_cluster_jobs(&self) -> bool {
            false
        }
        async fn may_execute_job(&self, kind: JobKind) -> bool {
            self.0.load(std::sync::atomic::Ordering::Acquire) && kind.permits_artifact_execution()
        }
    }

    #[tokio::test]
    async fn artifact_owner_renews_without_catalogue_authority_and_stops_on_role_loss() {
        let authority = Arc::new(ArtifactAuthority(std::sync::atomic::AtomicBool::new(true)));
        let (_store, _id, active) = active_with_authority(None, authority.clone()).await;
        assert!(!authority.may_run_cluster_jobs().await);
        assert!(!authority.may_execute_job(JobKind::LibraryScan).await);
        assert!(active
            .fence
            .renew()
            .await
            .expect("artifact ownership renews"));
        authority
            .0
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(!active
            .fence
            .renew()
            .await
            .expect("role loss refuses renewal"));
        // Finish still retires this exact attempt after the physical worker is
        // gone; losing dispatch authority never forbids ownership cleanup.
        active.finish().await;
    }

    #[test]
    fn publication_pacing_keeps_refusal_retry_and_yield_on_idle_backoff() {
        let mut pacing = IdlePoll {
            idle_rounds: 0,
            jitter: 7,
        };
        // A claimed job that yields, retries, or loses publication is still
        // unproductive. None may reset the accumulated idle backoff.
        for _outcome in ["claim_refused", "yield", "retry", "publication_fenced"] {
            assert!(pacing.after_completion(false) >= Duration::from_secs(5));
        }
        assert!(pacing.after_completion(false) >= Duration::from_secs(25));
        for _ in 0..100 {
            let delay = pacing.after_completion(true);
            assert!((Duration::from_millis(100)..=Duration::from_millis(250)).contains(&delay));
        }
        assert!(pacing.after_completion(false) <= Duration::from_secs(10));
        // Claim-based legacy callers do not acquire the fast publication path.
        assert!(pacing.delay(true) >= Duration::from_secs(5));
    }

    #[test]
    fn idle_poll_is_bounded_and_progress_resets_backoff() {
        let mut pacing = IdlePoll {
            idle_rounds: 0,
            jitter: 17,
        };
        for round in 0..16 {
            let delay = pacing.delay(false);
            assert!(delay >= Duration::from_secs(5));
            assert!(delay <= Duration::from_secs(30));
            if round >= 3 {
                assert!(delay >= Duration::from_secs(25));
            }
        }
        assert!(pacing.delay(true) <= Duration::from_secs(10));
        assert!(pacing.delay(false) <= Duration::from_secs(10));
    }

    /// A conservative confirmed deadline is valid even while the durable row
    /// remains live. Exercise the production heartbeat and child collection,
    /// with no manual cancellation and no test-only heartbeat implementation.
    #[cfg(unix)]
    #[tokio::test]
    async fn background_jobs_lease_expiry_reaps_child_before_releasing_capacity() {
        let (store, id, active) = active_with_deadline(Some(Duration::from_secs(2))).await;
        let fence = active.fence();
        let directory = tempfile::tempdir().expect("directory");
        let marker = directory.path().join("child.pid");
        let capacity = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&capacity).acquire_owned().await.expect("permit");
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", "echo $$ > \"$1\"; exec sleep 60", "lease-expiry-test"])
            .arg(&marker);
        let cancellation = fence.loss_token();
        let child = tokio::spawn(async move {
            let result = crate::ffmpeg::bounded_command_output_cancellable(
                command,
                Duration::from_secs(60),
                1024,
                "lease expiry regression",
                Some(&cancellation),
                crate::process_control::ChildWork::background("lease expiry regression"),
            )
            .await;
            // The child collector above has killed and awaited the process.
            active.finish().await;
            drop(permit);
            result
        });
        let pid = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(&marker).await {
                    if let Ok(pid) = text.trim().parse::<i32>() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child starts before the confirmed lease expires");
        assert_eq!(capacity.available_permits(), 0);
        assert!(!fence.loss_token().is_cancelled());
        let result = tokio::time::timeout(Duration::from_secs(5), child)
            .await
            .expect("lease loss returns capacity within playback budget")
            .expect("worker");
        assert!(result.is_err());
        assert!(fence.loss_token().is_cancelled());
        assert!(fence.snapshot().await.is_none());
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "child must be reaped before capacity is released"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert_eq!(capacity.available_permits(), 1);
        let job = store.background_job(&id).await.expect("read").expect("job");
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(
            job.failed_attempts, 0,
            "confirmed local expiry is an uncharged yield after join"
        );
        assert!(job.token.is_none());
    }

    #[tokio::test]
    async fn background_jobs_incompatible_pipeline_leaves_work_for_compatible_worker() {
        let (store, _, active) = active().await;
        let token = active.fence().snapshot().await.expect("current token");
        active.finish().await;
        let original = store
            .background_job(&token.job_id)
            .await
            .expect("read")
            .expect("original");
        let mut request = original.supported_payload().expect("payload");
        let JobPayload::FragmentIndexBuild {
            file_id,
            ref mut pipeline_digest,
            ref mut cache_key,
            ..
        } = request
        else {
            panic!("fragment");
        };
        let file = store
            .get_file(file_id)
            .await
            .expect("file read")
            .expect("file");
        let video = crate::fragindex::video_identities(&file, None, false, false)[0];
        *pipeline_digest =
            crate::fragment_index_cluster::pipeline_digest(&file, &"a".repeat(64), video);
        *cache_key = plurx_core::store::cluster_fragment_index_key(
            file_id,
            file.size,
            file.mtime,
            &"d".repeat(64),
            pipeline_digest,
        )
        .expect("key");
        let id = uuid::Uuid::new_v4().to_string();
        let now_ms = unix_ms().expect("clock");
        store
            .enqueue_job(EnqueueJob {
                id: id.clone(),
                payload: request.clone(),
                dedupe_key: format!("fragment:{id}"),
                priority: 1,
                not_before_ms: now_ms,
                now_ms,
                request: JobRequest {
                    scope: "test:heterogeneous".into(),
                    request_id: id.clone(),
                    request_digest: "b".repeat(64),
                    consumer_kind: "analysis".into(),
                    consumer_ref: id.clone(),
                    target_node_id: None,
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .expect("enqueue shared work");
        let incompatible = FragmentPipeline {
            engine_sha256: &"b".repeat(64),
            have_dovi: false,
            convert_dolby_vision: false,
        };
        assert!(
            !fragment_candidate_compatible(store.as_ref(), &request, &incompatible)
                .await
                .expect("preflight")
        );
        let unclaimed = store
            .background_job(&id)
            .await
            .expect("read")
            .expect("shared job");
        assert_eq!(unclaimed.state, JobState::Queued);
        assert_eq!(unclaimed.fence, 0);
        assert_eq!(unclaimed.failed_attempts, 0);
        let compatible = FragmentPipeline {
            engine_sha256: &"a".repeat(64),
            have_dovi: false,
            convert_dolby_vision: false,
        };
        assert!(
            fragment_candidate_compatible(store.as_ref(), &request, &compatible)
                .await
                .expect("compatible peer")
        );
        let now_ms = unix_ms().expect("clock");
        let result = claim_with_resolution(
            store.as_ref(),
            &unclaimed,
            ClaimJob {
                job_id: id,
                expected_revision: 0,
                node_id: "compatible-peer".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::FragmentIndexBuild,
                payload_version: 1,
                now_ms,
                dispatched_at_ms: now_ms,
            },
        )
        .await
        .expect("claim");
        assert!(
            result.is_some(),
            "the compatible peer can still execute the original request"
        );
    }

    #[tokio::test]
    async fn retirement_resolves_cancellation_revision_after_child_join() {
        let (store, id, active) = active().await;
        let fence = active.fence();
        store
            .cancel_job(CancelJob {
                job_id: id.clone(),
                now_ms: unix_ms().expect("clock"),
            })
            .await
            .expect("cancel");
        active.finish().await;
        let job = store.background_job(&id).await.expect("read").expect("job");
        assert_eq!(job.state, JobState::Cancelled);
        assert!(job.token.is_none());
        assert!(fence.snapshot().await.is_none());
        assert!(fence.loss_token().is_cancelled());
    }

    #[tokio::test]
    async fn blocked_snapshot_cannot_return_a_token_after_local_revocation() {
        let (_, _, active) = active().await;
        let fence = active.fence();
        let locked = fence.0.state.lock().await;
        let waiter = {
            let fence = fence.clone();
            tokio::spawn(async move { fence.snapshot().await })
        };
        tokio::task::yield_now().await;
        fence.0.lost.cancel();
        drop(locked);
        assert!(waiter.await.expect("snapshot task").is_none());
        active.finish().await;
    }
    #[tokio::test]
    async fn lost_claim_acknowledgement_resolves_the_original_attempt() {
        let (store, id, mut active) = active().await;
        // Model an abandoned claim after its physical producer has joined.
        // Lease expiry alone cannot reclaim the source from a live owner.
        active.stop.cancel();
        if let Some(heartbeat) = active.heartbeat.take() {
            heartbeat.await.expect("join fixture heartbeat");
        }
        let custody = active
            .fence
            .0
            .source_custody
            .clone()
            .expect("source custody");
        settle_joined_source_custody(&store, custody).await;
        let candidate = store.background_job(&id).await.expect("read").expect("job");
        let now_ms = candidate
            .token
            .as_ref()
            .expect("predecessor")
            .lease_expires_ms
            + 1;
        let request = ClaimJob {
            job_id: id.clone(),
            expected_revision: candidate.revision,
            node_id: "recovery-node".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::FragmentIndexBuild,
            payload_version: 1,
            now_ms,
            // The queue clock jumps past the predecessor lease; resolution
            // still runs on the real clock, so dispatch must be real too.
            dispatched_at_ms: unix_ms().expect("dispatch clock"),
        };
        let (result, dropped) = faults::dropping(
            faults::CLAIM,
            claim_with_resolution(store.as_ref(), &candidate, request.clone()),
        )
        .await;
        assert_eq!(dropped, faults::CLAIM);
        let (recovered, _) = result.expect("resolve").expect("owned");
        assert_eq!(
            recovered.failed_attempts,
            candidate.failed_attempts + 1,
            "takeover accounting must come from the committed row"
        );
        let token = recovered.token.expect("token");
        assert_eq!(token.claim_id, request.claim_id);
        assert_eq!(token.boot_id, request.boot_id);
        assert_eq!(token.fence, candidate.fence + 1);
        assert_eq!(
            store
                .background_job(&id)
                .await
                .expect("read")
                .expect("job")
                .token,
            Some(token)
        );
        assert_eq!(store.job_attempts(&id).await.expect("attempts").len(), 2);
        active.finish().await;
    }

    #[tokio::test]
    async fn lost_renewal_acknowledgement_recovers_the_advanced_revision() {
        let (store, id, active) = active().await;
        let fence = active.fence();
        let before = fence.snapshot().await.expect("token");
        let (result, dropped) = faults::dropping(faults::RENEW, fence.renew()).await;
        assert_eq!(dropped, faults::RENEW);
        assert!(result.expect("renewal resolution"));
        let after = fence.snapshot().await.expect("recovered token");
        assert_eq!(after.claim_id, before.claim_id);
        assert_eq!(after.fence, before.fence);
        assert_eq!(after.revision, before.revision + 1);
        assert_eq!(
            store
                .background_job(&id)
                .await
                .expect("read")
                .expect("job")
                .token,
            Some(after)
        );
        active.finish().await;
    }

    #[tokio::test]
    async fn source_pressure_can_progress_while_the_publisher_holds_the_job_fence() {
        let (store, id, active) = active().await;
        let fence = active.fence();
        // This is the exact guard held across complete_library_work. The
        // source supervisor cannot poll that publisher while awaiting a check.
        let publishing = fence.0.state.lock().await;
        tokio::time::timeout(Duration::from_secs(2), async {
            assert!(fence.source_may_continue().await.expect("live reader"));
            #[cfg(target_os = "linux")]
            {
                assert!(fence.pause_source().await.expect("verified pause"));
                assert!(fence.resume_source().await.expect("reacquire slot"));
            }
            store
                .cancel_job(plurx_core::store::background_jobs::CancelJob {
                    job_id: id,
                    now_ms: unix_ms().expect("clock"),
                })
                .await
                .expect("cancel reader")
                .expect("owned job");
            assert!(!fence.source_may_continue().await.expect("revoked reader"));
            #[cfg(target_os = "linux")]
            {
                assert!(!fence.pause_source().await.expect("revoked pause"));
                assert!(!fence.resume_source().await.expect("revoked resume"));
            }
        })
        .await
        .expect("source checks must finish before polling the publisher again");
        drop(publishing);
        active.finish().await;
    }

    #[tokio::test]
    async fn joined_source_settlement_retries_without_losing_exact_receipt() {
        let (store, _id, active) = active().await;
        let receipt = active
            .fence
            .0
            .source_custody
            .clone()
            .expect("source custody");
        let ((), dropped) = faults::dropping(faults::SOURCE_RELEASE, active.finish()).await;
        assert_eq!(dropped, faults::SOURCE_RELEASE);
        assert!(store
            .settled_background_source_custody()
            .await
            .expect("proof rows")
            .is_empty());
        assert!(!store
            .release_background_source_custody(receipt, unix_ms().expect("clock"))
            .await
            .expect("exact row already retired"));
    }

    #[tokio::test]
    async fn lost_publication_acknowledgement_reconciles_one_artifact() {
        let (store, id, active) = active().await;
        let job = store.background_job(&id).await.expect("read").expect("job");
        let token = job.token.clone().expect("token");
        let JobPayload::FragmentIndexBuild {
            file_id,
            source_size,
            source_mtime,
            source_sha256,
            cache_key,
            pipeline_digest,
            ..
        } = job.supported_payload().expect("payload")
        else {
            panic!("fragment");
        };
        let artifact = plurx_core::store::ClusterFragmentIndexArtifact {
            cache_key: cache_key.clone(),
            file_id,
            source_size,
            source_mtime,
            source_sha256,
            pipeline_sha256: pipeline_digest,
            blob_sha256: "e".repeat(64),
            bytes: 128,
            built_by_node_id: token.node_id,
            built_at_ms: unix_ms().expect("clock"),
        };
        let fence = active.fence();
        let (result, dropped) =
            faults::dropping(faults::PUBLISH, fence.publish_fragment(artifact.clone())).await;
        assert_eq!(dropped, faults::PUBLISH);
        assert!(result.expect("publication resolution"));
        assert!(fence.snapshot().await.is_none());
        assert_eq!(
            store
                .background_job(&id)
                .await
                .expect("read")
                .expect("job")
                .state,
            JobState::Succeeded
        );
        let saved = store
            .cluster_fragment_index_artifact(&cache_key)
            .await
            .expect("artifact")
            .expect("artifact");
        assert_eq!(saved.blob_sha256, artifact.blob_sha256);
        assert_eq!(
            store
                .cluster_fragment_index_locations(&cache_key)
                .await
                .expect("locations")
                .len(),
            1
        );
        assert_eq!(store.job_attempts(&id).await.expect("attempts").len(), 1);
        active.finish().await;
    }
}
