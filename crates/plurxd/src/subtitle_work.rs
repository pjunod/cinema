//! Adapt the established all-track extractor to one common durable owner.
use super::*;
use crate::background_jobs::{ActiveBackgroundJob, JobFence};
use plurx_core::process::bounded::{self, OriginalWorkKeeper, OriginalWorkUncertainty};
use plurx_core::store::background_jobs::{
    BackgroundJob, EnqueueOutcome, JobKind, JobPayload, JobSettlement,
};

// Keep foreground pressure registered across the bounded wait so existing
// background workers observe it and cooperatively release their CPU permits.
async fn admit_foreground_subtitle(
    admissions: &crate::admission::Admissions,
    budget: usize,
    deadline: tokio::time::Instant,
) -> Option<crate::admission::TranscodePermit> {
    let mut waiting = admissions.software_pool().queue_for_capacity();
    loop {
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        if let Some(permit) = admissions.try_admit_bundle(
            0,
            budget,
            &crate::admission::TranscodeResourceEstimate {
                hardware_slot: false,
                cpu_threads: 1,
                decoder_threads: None,
            },
            crate::admission::Priority::Live,
        ) {
            return Some(permit);
        }
        waiting.mark_capacity_blocked();
        #[cfg(target_os = "linux")]
        let _ = waiting.request_disposable_background_yield();
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return None;
        }
        tokio::time::sleep(crate::transcode::ADMISSION_POLL.min(deadline - now)).await;
    }
}

struct OriginalForegroundSubtitleRead {
    permit: Arc<crate::admission::TranscodePermit>,
    cleanup_unknown: std::sync::atomic::AtomicBool,
}

impl OriginalWorkKeeper for OriginalForegroundSubtitleRead {
    fn retain_uncertainty(&self, _reason: OriginalWorkUncertainty) {
        if !self.cleanup_unknown.swap(true, Ordering::AcqRel) {
            std::mem::forget(Arc::clone(&self.permit));
        }
    }

    #[cfg(target_os = "linux")]
    fn requires_group_quiescence(&self) -> bool {
        true
    }
}

async fn yield_unadmitted_subtitle(active: ActiveBackgroundJob) {
    if let Err(error) = active
        .fence()
        .settle(JobSettlement::Yield {
            error_code: None,
            checkpoint: None,
            not_before_ms: clock_ms().saturating_add(5_000),
        })
        .await
    {
        tracing::warn!(%error, "yielding an unadmitted subtitle claim");
    }
    active.finish().await;
}

async fn await_subtitle_work<F, C, CF, N, NF, B>(
    mut work: std::pin::Pin<&mut F>,
    loss: &CancellationToken,
    cancellation: &CancellationToken,
    pressure: &mut tokio::time::Interval,
    source_changed: N,
    source_may_continue: C,
    background_preempted: B,
) -> Result<(), AnalysisResolutionError>
where
    F: std::future::Future<Output = Result<(), AnalysisResolutionError>>,
    C: Fn() -> CF,
    CF: std::future::Future<Output = bool>,
    N: Fn() -> NF,
    NF: std::future::Future<Output = ()>,
    B: Fn() -> bool,
{
    // The producer may hold the job-fence mutex across an awaited Store write.
    // Keep pressure observations inside competing branch futures so waiting
    // for that mutex cannot stop polling the producer that must release it.
    loop {
        tokio::select! {
            biased;
            () = loss.cancelled() => { cancellation.cancel(); return work.await; }
            () = cancellation.cancelled() => break,
            preempted = async {
                pressure.tick().await;
                background_preempted() || !source_may_continue().await
            } => {
                if preempted { break; }
            }
            preempted = async {
                source_changed().await;
                !source_may_continue().await
            } => {
                if preempted { break; }
            }
            result = &mut work => return result,
        }
    }
    cancellation.cancel();
    // An original completion can retire its own source authority before
    // pressure arrives. Preserve that committed success after joining.
    if work.await.is_ok() {
        Ok(())
    } else {
        Err(AnalysisResolutionError::Retry {
            code: "foreground_preempted",
            charge_attempt: false,
        })
    }
}

impl JobManager {
    pub(crate) fn share_subtitle_admissions(&self, admissions: crate::admission::Admissions) {
        *self
            .subtitle_admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = admissions;
    }

    async fn admit_subtitle_intent(&self, request: AnalysisRequest) -> Result<(), StoreError> {
        match self.store.enqueue_subtitle_job(request, clock_ms()).await? {
            EnqueueOutcome::Accepted { .. }
            | EnqueueOutcome::Existing {
                cancelled: false, ..
            } => Ok(()),
            result => Err(StoreError::Task(format!("subtitle admission: {result:?}"))),
        }
    }

    pub(super) async fn work_subtitle_queue(
        self: &Arc<Self>,
        transcode: &TranscodeManager,
    ) -> bool {
        if !crate::state::lane_subtitle_source_queue_enabled(&self.applied).await
            || !self
                .job_authority
                .may_execute_job(JobKind::SubtitleExtract)
                .await
        {
            return false;
        }
        if self.may_run_cluster_jobs().await {
            match self.store.subtitle_job_intents(128).await {
                Ok(intents) => {
                    for id in intents {
                        if let Ok(Some(request)) = self.store.analysis_request(&id).await {
                            if !request.video_identity.is_empty()
                                || !request.target_node_id.is_empty()
                            {
                                // Malformed legacy demand must not poison the next outbox page.
                                crate::store_result::observe(
                                    crate::store_result::Operation::CancelStaleSubtitleIntent,
                                    crate::store_result::Discard::BestEffort,
                                    self.store
                                        .cancel_analysis_request_admin(&id, clock_ms())
                                        .await,
                                );
                                continue;
                            }
                            match self.store.enqueue_subtitle_job(request, clock_ms()).await {
                                Ok(EnqueueOutcome::QueueFull) => break,
                                Ok(
                                    EnqueueOutcome::RequestFenced
                                    | EnqueueOutcome::SourceChanged
                                    | EnqueueOutcome::NoDemand
                                    | EnqueueOutcome::DomainTerminal
                                    | EnqueueOutcome::Conflict,
                                ) => {
                                    // Immutable intent no longer matches a claimable source.
                                    // Retire it so a full page of stale rows cannot starve demand.
                                    crate::store_result::observe(
                                        crate::store_result::Operation::CancelStaleSubtitleIntent,
                                        crate::store_result::Discard::BestEffort,
                                        self.store
                                            .cancel_analysis_request_admin(&id, clock_ms())
                                            .await,
                                    );
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    tracing::debug!(%error, request_id = %id, "subtitle outbox entry deferred")
                                }
                            }
                        }
                    }
                }
                Err(error) => tracing::warn!(%error, "reading subtitle outbox"),
            }
        }
        // Walking candidate pages and resolving a claim does not own a
        // codec. Check the existing idle/heavy lane first, then reserve the
        // actual worker after the durable claim.'
        if !transcode.fragment_worker_may_start() {
            return false;
        }
        let Some((job, active)) = self.claim_subtitle_work(None).await else {
            return false;
        };
        let Some(admission) = transcode.admit_subtitle_source().await.map(Arc::new) else {
            yield_unadmitted_subtitle(active).await;
            return false;
        };
        let Some(original) = OriginalDisposableSourceRead::register(Arc::clone(&admission)) else {
            yield_unadmitted_subtitle(active).await;
            return false;
        };
        let cancellation = original.pressure.cancel.clone();
        self.run_subtitle_work(
            job,
            active,
            transcode.runtime_cache_dir(),
            Some((transcode, &admission)),
            original,
            cancellation,
        )
        .await;
        true
    }

    /// Playback's bounded fallback joins the same computation and resources.
    /// It keeps its foreground scheduling policy, never a second analysis lease.
    pub(crate) async fn self_claim_subtitle_source(
        self: &Arc<Self>,
        request_id: &str,
        runtime_cache: &Path,
    ) -> bool {
        if !self.subtitle_source_queue_enabled().await
            || !self
                .job_authority
                .may_execute_job(JobKind::SubtitleExtract)
                .await
        {
            return false;
        }
        let Ok(Some(request)) = self.store.analysis_request(request_id).await else {
            return false;
        };
        if request.priority != "foreground" || self.admit_subtitle_intent(request).await.is_err() {
            return false;
        }
        let budget = self
            .store
            .get_setting(keys::SW_POOL_THREADS)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_else(crate::admission::software_budget);
        let admissions = self
            .subtitle_admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let deadline = tokio::time::Instant::now() + crate::admission::QUEUE_WAIT;
        let Some((job, active)) = self.claim_subtitle_work(Some(request_id)).await else {
            return false;
        };
        let Some(permit) = admit_foreground_subtitle(&admissions, budget, deadline).await else {
            yield_unadmitted_subtitle(active).await;
            return false;
        };
        crate::telemetry::record_subtitle_source(
            crate::telemetry::SubtitleSourceMetric::ForegroundSelfClaim,
        );
        let original = Arc::new(OriginalForegroundSubtitleRead {
            permit: Arc::new(permit),
            cleanup_unknown: std::sync::atomic::AtomicBool::new(false),
        });
        let jobs = Arc::clone(self);
        let runtime_cache = runtime_cache.to_owned();
        let cancellation = CancellationToken::new();
        // The route can stop waiting at any point. Its departure requests
        // cleanup from this same worker, which still owns the job and permit.
        let _demand = cancellation.clone().drop_guard();
        tokio::spawn(async move {
            jobs.run_subtitle_work(job, active, &runtime_cache, None, original, cancellation)
                .await
        })
        .await
        .unwrap_or(false)
    }

    async fn claim_subtitle_work(
        &self,
        filter: Option<&str>,
    ) -> Option<(BackgroundJob, ActiveBackgroundJob)> {
        use sha2::Digest;
        let pipeline = hex::encode(sha2::Sha256::digest(
            subtitle_source_pipeline_version().await.as_bytes(),
        ));
        match crate::background_jobs::claim_subtitle(
            Arc::clone(&self.store),
            Arc::clone(&self.job_authority),
            self.coordinator.node_id(),
            filter,
            &pipeline,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(%error, "claiming durable subtitles");
                None
            }
        }
    }

    async fn run_subtitle_work(
        self: &Arc<Self>,
        job: BackgroundJob,
        active: ActiveBackgroundJob,
        runtime_cache: &Path,
        background: Option<(&TranscodeManager, &crate::transcode::FragmentAdmission)>,
        keeper: Arc<dyn OriginalWorkKeeper>,
        cancellation: CancellationToken,
    ) -> bool {
        let mut close = OriginalSourceReadClose(Some(Arc::clone(&keeper)));
        let fence = active.fence();
        let JobPayload::SubtitleExtract {
            source_generation, ..
        } = job.supported_payload().expect("typed subtitle claim")
        else {
            unreachable!()
        };
        let request = self.store.analysis_request(&source_generation).await;
        let result = match request {
            Ok(Some(request)) => {
                let _progress = self.start_analysis_progress(
                    (&request.request_id, &request.target_node_id),
                    request.file_id,
                    "subtitle_source",
                    "probing",
                    request.source_size.max(0) as u64,
                    0,
                );
                let loss = fence.loss_token();
                let work = crate::background_jobs::budgeted(
                    fence.source_budget(),
                    plurx_core::process::bounded::cancellable(
                        cancellation.clone(),
                        self.resolve_durable_subtitle(
                            &request,
                            runtime_cache,
                            &fence,
                            &cancellation,
                        ),
                    ),
                );
                let work = bounded::with_original_work_keeper(keeper, work);
                tokio::pin!(work);
                let mut pressure = tokio::time::interval(Duration::from_millis(250));
                await_subtitle_work(
                    work.as_mut(),
                    &loss,
                    &cancellation,
                    &mut pressure,
                    || crate::source_custody::source_pressure_changed().notified(),
                    || async { fence.source_may_continue().await.unwrap_or(false) },
                    || {
                        background
                            .is_some_and(|(manager, permit)| !manager.fragment_worker_idle(permit))
                    },
                )
                .await
            }
            Ok(None) => Err(AnalysisResolutionError::Terminal("source_deleted")),
            Err(_) => Err(AnalysisResolutionError::Retry {
                code: "source_catalog_read_failed",
                charge_attempt: true,
            }),
        };
        let succeeded = result.is_ok();
        let settlement = match result {
            Ok(()) | Err(AnalysisResolutionError::ClaimLost) => None,
            Err(AnalysisResolutionError::Terminal(code)) => Some(JobSettlement::Fail {
                error_code: code.into(),
            }),
            Err(AnalysisResolutionError::Retry {
                code,
                charge_attempt: true,
            }) => Some(JobSettlement::Retry {
                error_code: code.into(),
                not_before_ms: clock_ms().saturating_add(crate::background_jobs::retry_delay_ms(
                    &job.id,
                    job.failed_attempts,
                )),
            }),
            Err(AnalysisResolutionError::Retry {
                charge_attempt: false,
                ..
            }) => Some(JobSettlement::Yield {
                error_code: None,
                checkpoint: None,
                not_before_ms: clock_ms().saturating_add(5_000),
            }),
        };
        if let Some(settlement) = settlement {
            if let Err(error) = fence.settle(settlement).await {
                tracing::warn!(%error, "settling subtitle attempt");
            }
        }
        active.finish().await;
        close.0 = None;
        succeeded
    }

    async fn resolve_durable_subtitle(
        &self,
        request: &AnalysisRequest,
        runtime_cache: &Path,
        fence: &JobFence,
        lost: &CancellationToken,
    ) -> Result<(), AnalysisResolutionError> {
        let file = match self.store.get_file(request.file_id).await {
            Ok(Some(file))
                if file.size == request.source_size && file.mtime == request.source_mtime =>
            {
                file
            }
            Ok(_) => return Err(AnalysisResolutionError::Terminal("source_superseded")),
            Err(_) => {
                return Err(AnalysisResolutionError::Retry {
                    code: "source_catalog_read_failed",
                    charge_attempt: true,
                })
            }
        };
        self.set_analysis_progress_totals(
            &request.request_id,
            &request.target_node_id,
            file.size.max(0) as u64,
            file.duration_ms.unwrap_or_default(),
        );
        self.resolve_subtitle_source_request(
            request,
            self.coordinator.node_id(),
            &file,
            runtime_cache,
            None,
            fence,
            lost,
        )
        .await
    }
}

#[cfg(test)]
mod foreground_admission_tests {
    use super::admit_foreground_subtitle;
    use crate::admission::{Admissions, Priority, TranscodeResourceEstimate};

    #[tokio::test(start_paused = true)]
    async fn subtitle_pressure_preserves_committed_success_after_join() {
        for notification in [false, true] {
            let loss = super::CancellationToken::new();
            let cancellation = super::CancellationToken::new();
            let work = async {
                cancellation.cancelled().await;
                Ok(())
            };
            tokio::pin!(work);
            let mut pressure = tokio::time::interval_at(
                tokio::time::Instant::now()
                    + std::time::Duration::from_secs(if notification { 3600 } else { 0 }),
                std::time::Duration::from_secs(3600),
            );
            let result = super::await_subtitle_work(
                work.as_mut(),
                &loss,
                &cancellation,
                &mut pressure,
                || async {
                    if !notification {
                        std::future::pending::<()>().await;
                    }
                },
                || async { false },
                || false,
            )
            .await;
            assert!(
                cancellation.is_cancelled(),
                "pressure path must join the worker"
            );
            assert!(
                result.is_ok(),
                "committed completion was discarded: {result:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn subtitle_pressure_checks_keep_polling_the_write_that_holds_the_fence() {
        use std::future::Future;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::task::Poll;
        use tokio::sync::{oneshot, Mutex, Notify};

        for notification in [false, true] {
            let state = Mutex::new(());
            let checked = AtomicBool::new(false);
            let changed = Notify::new();
            let loss = super::CancellationToken::new();
            let cancellation = super::CancellationToken::new();
            let (complete_write, write_reply) = oneshot::channel();
            // JobFence::write_subtitle retains its state mutex across the
            // Store write. Only polling this owner can consume the reply,
            // release the mutex and proceed to joined-source settlement.
            let work = async {
                let _state = state.lock().await;
                write_reply.await.expect("Store write reply");
                Ok(())
            };
            tokio::pin!(work);
            let delay = if notification { 3600 } else { 1 };
            let mut pressure = tokio::time::interval_at(
                tokio::time::Instant::now() + std::time::Duration::from_secs(delay),
                std::time::Duration::from_secs(3600),
            );
            let driver = super::await_subtitle_work(
                work.as_mut(),
                &loss,
                &cancellation,
                &mut pressure,
                || changed.notified(),
                || async {
                    checked.store(true, Ordering::SeqCst);
                    let _state = state.lock().await;
                    true
                },
                || false,
            );
            tokio::pin!(driver);
            std::future::poll_fn(|cx| {
                assert!(driver.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            assert!(state.try_lock().is_err(), "write retains fence state");
            if notification {
                changed.notify_waiters();
            } else {
                tokio::time::advance(std::time::Duration::from_secs(1)).await;
            }
            std::future::poll_fn(|cx| {
                assert!(driver.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            assert!(
                checked.load(Ordering::SeqCst),
                "pressure waits for fence state"
            );
            complete_write
                .send(())
                .expect("deliver committed write reply");
            let result = tokio::time::timeout(std::time::Duration::from_millis(20), driver)
                .await
                .expect("pressure must keep polling the write owner");
            assert!(result.is_ok(), "joined work reaches settlement");
            assert!(!cancellation.is_cancelled());
            assert!(state.try_lock().is_ok(), "write releases fence state");
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(start_paused = true)]
    async fn requested_subtitle_yield_keeps_cpu_until_original_work_returns() {
        let admissions = Admissions::new();
        let held = admissions
            .try_admit_bundle(
                0,
                2,
                &TranscodeResourceEstimate {
                    hardware_slot: false,
                    cpu_threads: 1,
                    decoder_threads: None,
                },
                Priority::Background,
            )
            .expect("actual one-thread background reservation");
        let registered = held
            .register_disposable_background()
            .expect("registered original work");
        let cancellation = registered.cancel.clone();
        let (stop_seen, saw_stop) = tokio::sync::oneshot::channel();
        let (allow_original_close, original_may_close) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let _held = held;
            let _registered = registered;
            let work = async {
                cancellation.cancelled().await;
                stop_seen.send(()).expect("report actual cancellation");
                original_may_close.await.expect("allow actual closure");
                Err(super::AnalysisResolutionError::ClaimLost)
            };
            tokio::pin!(work);
            let mut ordinary_pressure = tokio::time::interval_at(
                tokio::time::Instant::now() + std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(60),
            );
            super::await_subtitle_work(
                work.as_mut(),
                &super::CancellationToken::new(),
                &cancellation,
                &mut ordinary_pressure,
                std::future::pending,
                || async { true },
                || false,
            )
            .await
        });
        let shared = admissions.clone();
        let deadline = tokio::time::Instant::now() + crate::admission::QUEUE_WAIT;
        let waiter =
            tokio::spawn(async move { admit_foreground_subtitle(&shared, 2, deadline).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), saw_stop)
            .await
            .expect("direct registered yield, before polling pressure")
            .expect("original observed the request");
        assert_eq!(admissions.software_in_use(), 1);
        assert!(
            !waiter.is_finished(),
            "cancellation is not refund authority"
        );
        allow_original_close.send(()).expect("owner can close");
        assert!(matches!(
            worker.await.expect("original worker joined"),
            Err(super::AnalysisResolutionError::Retry {
                code: "foreground_preempted",
                charge_attempt: false,
            }),
        ));
        let foreground = waiter.await.expect("foreground waiter joined");
        assert!(foreground.is_some());
        drop(foreground);
        assert_eq!(admissions.software_in_use(), 0);
    }

    fn background(admissions: &Admissions) -> crate::admission::TranscodePermit {
        admissions
            .try_admit_bundle(
                0,
                2,
                &TranscodeResourceEstimate {
                    hardware_slot: false,
                    cpu_threads: 2,
                    decoder_threads: None,
                },
                Priority::Speculative,
            )
            .expect("real background CPU holder")
    }

    #[tokio::test(start_paused = true)]
    async fn foreground_subtitle_admission_timeout_releases_live_waiter() {
        let admissions = Admissions::new();
        let held = background(&admissions);
        let started = tokio::time::Instant::now();
        let deadline = started + crate::admission::QUEUE_WAIT;
        assert!(admit_foreground_subtitle(&admissions, 2, deadline)
            .await
            .is_none());
        assert_eq!(started.elapsed(), crate::admission::QUEUE_WAIT);
        assert!(!admissions.live_is_waiting());
        assert_eq!(admissions.software_in_use(), 2);
        drop(held);
        drop(background(&admissions));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_foreground_subtitle_admission_releases_live_waiter() {
        let admissions = Admissions::new();
        let held = background(&admissions);
        let shared = admissions.clone();
        let deadline = tokio::time::Instant::now() + crate::admission::QUEUE_WAIT;
        let waiting =
            tokio::spawn(async move { admit_foreground_subtitle(&shared, 2, deadline).await });
        tokio::task::yield_now().await;
        assert!(
            admissions.live_is_waiting(),
            "foreground registers pressure before sleeping"
        );
        waiting.abort();
        assert!(waiting
            .await
            .expect_err("cancelled admission")
            .is_cancelled());
        assert!(!admissions.live_is_waiting());
        assert_eq!(admissions.software_in_use(), 2);
        drop(held);
        drop(background(&admissions));
    }
}
