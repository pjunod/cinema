//! Production lifecycle for common durable claims. A physical worker keeps
//! its admission guard until its child has joined; this module owns only the
//! durable token, monotonic deadline and cancellation notification.

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
#[path = "background_job_faults.rs"]
mod faults;
#[path = "background_job_metrics.rs"]
mod metrics;
use metrics::Event;
pub(crate) use metrics::{accepted_claims, prometheus};

const PUBLICATION_MARGIN: Duration = Duration::from_secs(3);

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
    let claimed = store.claim_job(request.clone()).await;
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
                    if job.token.as_ref() == Some(&token) {
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

impl ActiveBackgroundJob {
    pub(crate) fn start(
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
            lost: CancellationToken::new(),
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
        metrics::execution(self.fence.0.kind, self.fence.0.started.elapsed());
    }
}

impl Drop for ActiveBackgroundJob {
    fn drop(&mut self) {
        self.fence.0.lost.cancel();
        self.stop.cancel();
        // Do not release a durable reservation here: a dropped caller is not
        // proof that its child exited. The lease remains until explicit join
        // and finish, or expiry. Dropping JoinHandle does not abort its write.
    }
}

impl JobFence {
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

    async fn renew(&self) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if self.0.lost.is_cancelled() || !self.0.authority.may_run_cluster_jobs().await {
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

    pub(crate) async fn bind_transcode_recipe(
        &self,
        recipe_hash: &str,
    ) -> Result<bool, StoreError> {
        let state = self.0.state.lock().await;
        if !self.0.authority.may_run_cluster_jobs().await || !self.may_publish() {
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

    pub(crate) async fn publish_transcode(
        &self,
        output: TranscodeJobOutput,
    ) -> Result<bool, StoreError> {
        let mut state = self.0.state.lock().await;
        if !self.0.authority.may_run_cluster_jobs().await || !self.may_publish() {
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
                if !self.0.authority.may_run_cluster_jobs().await || !self.may_publish() {
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
        if !self.0.authority.may_run_cluster_jobs().await || !self.may_publish() {
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
                if !self.0.authority.may_run_cluster_jobs().await || !self.may_publish() {
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
pub(crate) async fn claim_pretranscode(
    store: Arc<dyn Store>,
    authority: Arc<dyn ClusterJobAuthority>,
    transcode: &crate::transcode::TranscodeManager,
    node: &str,
    capabilities: &plurx_core::domain::PretranscodeWorkerCapabilities,
    excluded: &[String],
) -> Result<
    Option<(
        plurx_core::domain::PretranscodeJob,
        ActiveBackgroundJob,
        crate::transcode::PretranscodeFence,
    )>,
    StoreError,
> {
    use plurx_core::store::background_jobs::{
        CandidateQuery, JobKind, JobPayload, MAX_ACTIVE_JOBS, MAX_PAGE_SIZE,
    };
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let boot = BOOT.get_or_init(|| uuid::Uuid::new_v4().to_string());
    if !authority.may_run_cluster_jobs().await {
        return Ok(None);
    }
    let mut cursor = None;
    for _ in 0..MAX_ACTIVE_JOBS.div_ceil(MAX_PAGE_SIZE) {
        let page = store
            .job_candidates(CandidateQuery {
                node_id: node.into(),
                kinds: vec![JobKind::TranscodePrepare],
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
            let JobPayload::TranscodePrepare {
                requirements,
                target_height,
                file_id,
                ..
            } = &payload
            else {
                continue;
            };
            if !requirements.compatible_with(capabilities)
                || i64::from(*target_height) > capabilities.max_target_height
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
            if !authority.may_run_cluster_jobs().await {
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
            )?;
            let fence = crate::transcode::PretranscodeFence::new(
                projection.clone(),
                active.fence(),
                admission,
            );
            return Ok(Some((projection, active, fence)));
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
    if !authority.may_run_cluster_jobs().await {
        return Ok(None);
    }
    let mut cursor = None;
    for _ in 0..MAX_ACTIVE_JOBS.div_ceil(MAX_PAGE_SIZE) {
        let page = store
            .job_candidates(CandidateQuery {
                node_id: node.into(),
                kinds: vec![JobKind::FragmentIndexBuild, JobKind::ArtifactHydrate],
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
            if !authority.may_run_cluster_jobs().await {
                return Ok(None);
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
            )?;
            return Ok(Some((projection, active, admission)));
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::cluster::coordination::UnclusteredJobAuthority;
    use plurx_core::store::background_jobs::{
        CancelJob, EnqueueJob, JobKind, JobPayload, JobRequest, JobState,
    };
    use plurx_core::store::SqliteStore;

    async fn active() -> (Arc<dyn Store>, String, ActiveBackgroundJob) {
        active_with_deadline(None).await
    }

    async fn active_with_deadline(
        local_deadline: Option<Duration>,
    ) -> (Arc<dyn Store>, String, ActiveBackgroundJob) {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
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
            Arc::new(UnclusteredJobAuthority),
            job.token.expect("token"),
            local_deadline.map_or(deadline, |duration| Instant::now() + duration),
            JobKind::FragmentIndexBuild,
        )
        .expect("start");
        (store, id, active)
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
        let (store, id, active) = active().await;
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
