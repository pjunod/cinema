//! Adapt the established all-track extractor to one common durable owner.
use super::*;
use crate::background_jobs::{ActiveBackgroundJob, JobFence};
use plurx_core::store::background_jobs::{
    BackgroundJob, EnqueueOutcome, JobKind, JobPayload, JobSettlement,
};

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
        if !self.subtitle_source_queue_enabled().await
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
                                let _ = self
                                    .store
                                    .cancel_analysis_request_admin(&id, clock_ms())
                                    .await;
                                continue;
                            }
                            match self.store.enqueue_subtitle_job(request, clock_ms()).await {
                                Ok(EnqueueOutcome::QueueFull) => break,
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
        let Some(admission) = transcode.admit_fragment().await else {
            return false;
        };
        let Some((job, active)) = self.claim_subtitle_work(None).await else {
            return false;
        };
        self.run_subtitle_work(
            job,
            active,
            transcode.runtime_cache_dir(),
            Some((transcode, &admission)),
        )
        .await;
        drop(admission);
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
        let Some(_admission) = admissions.try_admit_bundle(
            0,
            budget,
            &crate::admission::TranscodeResourceEstimate {
                hardware_slot: false,
                cpu_threads: 1,
                decoder_threads: None,
            },
            crate::admission::Priority::Live,
        ) else {
            return false;
        };
        let Some((job, active)) = self.claim_subtitle_work(Some(request_id)).await else {
            return false;
        };
        crate::telemetry::record_subtitle_source(
            crate::telemetry::SubtitleSourceMetric::ForegroundSelfClaim,
        );
        self.run_subtitle_work(job, active, runtime_cache, None)
            .await
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
    ) -> bool {
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
                let cancellation = CancellationToken::new();
                let loss = fence.loss_token();
                let work =
                    self.resolve_durable_subtitle(&request, runtime_cache, &fence, &cancellation);
                tokio::pin!(work);
                let mut pressure = tokio::time::interval(Duration::from_millis(250));
                loop {
                    tokio::select! {
                        biased;
                        () = loss.cancelled() => { cancellation.cancel(); break work.await; }
                        _ = pressure.tick() => {
                            if background.is_some_and(|(manager, permit)| !manager.fragment_worker_idle(permit)) {
                                cancellation.cancel(); let _ = work.await;
                                break Err(AnalysisResolutionError::Retry { code: "foreground_preempted", charge_attempt: false });
                            }
                        }
                        result = &mut work => break result,
                    }
                }
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
