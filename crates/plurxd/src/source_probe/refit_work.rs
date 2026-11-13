//! The F+ curve fit (`JobKind::DvRefit`) of a Profile 7 FEL title, on the
//! same title lane as the complete-title scan and shaped like it
//! (`docs/streaming/FEL-RPU-REFIT-BUILD.md` §3.3): durable demand, the
//! shared request identity, a fenced claim with lease and heartbeat, the
//! held-source helper run labelled for Activity, and one fenced publication.
//!
//! It runs while at least one client platform's F+ switch is on, or a
//! pending disk conversion requires a fit before discarding a FEL,
//! (`playback.dolby_vision_refit_*`, Settings → Developer). It coexists with
//! live sessions on an explicit thread reservation and yields only when a
//! viewer waits for capacity, or to an ordinary media probe; a yield keeps
//! the helper's whole scene lines in the node-local spool and the next
//! attempt resumes there. It is listed, cancellable and retryable from the
//! background-jobs page; a failed or cancelled identity is not demanded
//! again until its source changes or an administrator retries it.
#[cfg(test)]
#[path = "refit_work_tests.rs"]
mod tests;

use super::{
    dv_refit::{HelperRun, RunError, Spool, RESERVED_THREADS},
    now_ms, snapshot,
};
use crate::{
    background_jobs::{ActiveBackgroundJob, RefitPublication},
    state::AppState,
    vodencode::dv_runtime::ToolSet,
};
use plurx_core::{
    error::StoreError,
    store::{
        background_jobs::{
            CancelJob, CandidateCursor, ClaimJob, EnqueueJob, EnqueueOutcome, JobKind, JobPayload,
            JobRequest, JobSettlement,
        },
        dv_refit_jobs::{RebindRefit, RefitDemand, RefitSceneRow, RefitTitle},
    },
    transcode::{
        dv_refit::FIT_VERSION,
        dv_refit_title::{self, RefitSource},
    },
};
use std::{
    future::Future,
    pin::Pin,
    sync::{atomic::AtomicI64, Arc},
};
use tokio_util::sync::CancellationToken;

/// A preemption that kept no new scene does not yield again more than this
/// many times in a row: the next one settles as a retry, which counts
/// against the attempt limit, so yields are never unbounded.
pub(super) const MAX_STALLED_YIELDS: i64 = 8;
const STALLED_YIELDS: &str = "refit_stalled_yields";
/// How often an idle pass drops curve tables superseded more than seven days ago.
const PRUNE_EVERY_MS: i64 = 60 * 60 * 1000;
static LAST_PRUNE_MS: AtomicI64 = AtomicI64::new(0);

pub(super) struct RefitTask {
    pub(super) work: Pin<Box<dyn Future<Output = bool> + Send>>,
    cancel: CancellationToken,
    job_id: String,
    file_id: i64,
    probe_pipeline: String,
    probe_after: Option<CandidateCursor>,
}

/// The fit identity (decision G): the source, the selected video stream,
/// the helper's refit identity and the fit method. The title-scan generation
/// is not part of it.
pub(super) fn identity(source: &RefitSource) -> String {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            "dv-refit-v2",
            source.file_id,
            source.source_size,
            source.source_mtime,
            source.video_index,
            &source.helper_identity,
            FIT_VERSION,
        ))
        .expect("refit request identity"),
    ));
    format!("dv-refit:{digest}")
}

pub(super) fn enqueue_request(source: &RefitSource, now: i64) -> EnqueueJob {
    let key = identity(source);
    let digest = key.trim_start_matches("dv-refit:").to_owned();
    EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: JobPayload::DvRefit {
            file_id: source.file_id,
            source_size: source.source_size,
            source_mtime: source.source_mtime,
            video_index: source.video_index,
            probe_revision: source.probe_revision,
            helper_identity: source.helper_identity.clone(),
            fit_version: FIT_VERSION.into(),
        },
        dedupe_key: key,
        priority: 0,
        not_before_ms: now,
        now_ms: now,
        request: JobRequest {
            scope: "dv-refit".into(),
            request_id: digest.clone(),
            request_digest: digest,
            consumer_kind: "dv_refit".into(),
            consumer_ref: source.file_id.to_string(),
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
    }
}

/// Whether any client platform's F+ switch is on: the demand runs only then.
pub(crate) async fn enabled(state: &AppState) -> Result<bool, StoreError> {
    let keys = plurx_core::store::keys::DV_REFIT_PLATFORMS;
    let settings = state.store.get_settings(&keys).await?;
    Ok(keys.iter().any(|key| {
        plurx_core::store::stored_switch(settings.get(*key).map(String::as_str), false)
    }))
}

/// The demand's source for the stream its probe selects, or `None` when the
/// probe names no usable video stream.
pub(super) fn source_of(demand: &RefitDemand, helper: &str) -> Option<RefitSource> {
    let document: serde_json::Value = serde_json::from_str(&demand.probe_json).ok()?;
    let index = super::dv_title::selected_video_index(&document)?;
    Some(RefitSource {
        file_id: demand.file_id,
        source_size: demand.source_size,
        source_mtime: demand.source_mtime,
        video_index: i64::from(index),
        probe_revision: demand.probe_revision,
        helper_identity: helper.to_owned(),
    })
}

/// A demand whose stream and source already have a table or a terminal
/// outcome under an older title-scan generation only moves that generation.
pub(super) fn rebind_of(demand: &RefitDemand, source: &RefitSource) -> Option<RebindRefit> {
    let key = identity(source);
    let known = demand.stored_video_index == Some(source.video_index)
        || demand
            .terminal
            .iter()
            .any(|t| t.identity == key && t.video_index == source.video_index);
    known.then(|| RebindRefit {
        file_id: source.file_id,
        source_size: source.source_size,
        source_mtime: source.source_mtime,
        video_index: source.video_index,
        helper_identity: source.helper_identity.clone(),
        fit_version: FIT_VERSION.into(),
        identity: key,
        probe_revision: source.probe_revision,
    })
}

async fn prune(state: &AppState) {
    let now = now_ms();
    let last = LAST_PRUNE_MS.load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(last) < PRUNE_EVERY_MS {
        return;
    }
    LAST_PRUNE_MS.store(now, std::sync::atomic::Ordering::Relaxed);
    if let Err(error) = state.store.prune_dv_refit(now).await {
        tracing::debug!(%error, "F+ retention unavailable");
    }
}

impl RefitTask {
    /// Starts at most one fit, after the title lane found no title scan to
    /// run. `after` walks the demand by file across ticks.
    pub(super) async fn start(
        state: &AppState,
        boot: &str,
        after: &mut i64,
    ) -> Result<Option<Self>, StoreError> {
        let authority = state.jobs.execution_authority();
        if !authority.may_execute_job(JobKind::DvRefit).await {
            return Ok(None);
        }
        let tools = match ToolSet::capture().await {
            Ok(Some(tools)) => tools,
            Ok(None) => return Ok(None),
            Err(error) => {
                tracing::debug!(%error, "F+ helper unavailable");
                return Ok(None);
            }
        };
        let Some(probe_pipeline) = plurx_core::scan::probe::pipeline_digest().await else {
            return Ok(None);
        };
        Self::start_with(state, boot, after, tools, probe_pipeline).await
    }

    pub(super) async fn start_with(
        state: &AppState,
        boot: &str,
        after: &mut i64,
        tools: Arc<ToolSet>,
        probe_pipeline: String,
    ) -> Result<Option<Self>, StoreError> {
        let authority = state.jobs.execution_authority();
        let helper = tools.refit_helper_identity();
        let include_all = enabled(state).await?;
        let demands = state
            .store
            .dv_refit_demands(&helper, FIT_VERSION, include_all, *after, 16)
            .await?;
        if demands.is_empty() {
            *after = 0;
            prune(state).await;
            return Ok(None);
        }
        let mut admission = None;
        for demand in demands {
            *after = demand.file_id;
            let Some(source) = source_of(&demand, &helper) else {
                continue;
            };
            if let Some(rebind) = rebind_of(&demand, &source) {
                state.store.rebind_dv_refit(rebind).await?;
                continue;
            }
            let Some(file) = state.store.get_file(demand.file_id).await? else {
                continue;
            };
            if file.size != demand.source_size
                || file.mtime != demand.source_mtime
                || snapshot(&file.path).await != Some((file.size, file.mtime))
                || tokio::fs::File::open(&file.path).await.is_err()
            {
                continue;
            }
            // The local index pass and the title scan read this title next:
            // do not read it under them.
            if state.jobs.title_scan_awaits_index_pass(file.id) {
                continue;
            }
            if admission.is_none() {
                admission = state.transcode.admit_dv_refit(RESERVED_THREADS).await;
            }
            if admission.is_none() {
                return Ok(None);
            }
            let now = now_ms();
            let key = identity(&source);
            let id = match &demand.queued {
                Some(queued) if queued.identity == key => queued.job_id.clone(),
                stale => {
                    // A queued job for an identity that is no longer current
                    // (an older helper or source) can never publish.
                    if let Some(stale) = stale {
                        state
                            .store
                            .cancel_job(CancelJob {
                                job_id: stale.job_id.clone(),
                                now_ms: now,
                            })
                            .await?;
                    }
                    match state
                        .store
                        .enqueue_job(enqueue_request(&source, now))
                        .await?
                    {
                        EnqueueOutcome::Accepted { job_id, .. }
                        | EnqueueOutcome::Existing {
                            job_id,
                            cancelled: false,
                            ..
                        } => job_id,
                        _ => continue,
                    }
                }
            };
            let Some(candidate) = state.store.background_job(&id).await? else {
                continue;
            };
            let Some((job, deadline)) = crate::background_jobs::claim_with_resolution(
                state.store.as_ref(),
                &candidate,
                ClaimJob {
                    job_id: id.clone(),
                    expected_revision: candidate.revision,
                    node_id: state.node_id.clone(),
                    boot_id: boot.into(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::DvRefit,
                    payload_version: 1,
                    now_ms: now,
                    dispatched_at_ms: now,
                },
            )
            .await?
            else {
                continue;
            };
            let token = job
                .token
                .clone()
                .ok_or_else(|| StoreError::Task("claimed F+ fit has no token".into()))?;
            let active = ActiveBackgroundJob::start(
                Arc::clone(&state.store),
                Arc::clone(&authority),
                token,
                deadline,
                JobKind::DvRefit,
            )
            .await?;
            let cancel = CancellationToken::new();
            let file_id = file.id;
            let work = execute(
                state.clone(),
                file,
                source,
                tools,
                admission.take().expect("admitted"),
                job,
                active,
                cancel.clone(),
            );
            return Ok(Some(Self {
                work: Box::pin(work),
                cancel,
                job_id: id,
                file_id,
                probe_pipeline,
                probe_after: None,
            }));
        }
        Ok(None)
    }

    pub(super) async fn should_yield(&mut self, state: &AppState) -> Result<bool, StoreError> {
        if !enabled(state).await? {
            let disk_needs_fit = state.store.dv_conversion(self.file_id).await?
                .is_some_and(|row| matches!(row.state,
                    plurx_core::store::DvConversionState::Queued
                    | plurx_core::store::DvConversionState::Running));
            if !disk_needs_fit {
                // Existing stop/execute ownership reaps and yields with the
                // spool intact. Re-enabling can resume the same identity.
                return Ok(true);
            }
        }
        super::title_work::probe_preempts(
            state,
            &self.job_id,
            &self.probe_pipeline,
            &mut self.probe_after,
        )
        .await
    }

    pub(super) async fn stop(mut self) -> bool {
        self.cancel.cancel();
        // Never drop the child future to release the lane: reap, settle, finish.
        self.work.as_mut().await
    }
}

/// The rows a helper answer publishes: a solved title, or a recorded
/// refusal. Solving is CPU work over every scene, so it runs off the runtime.
pub(super) async fn rows(
    run: HelperRun,
    source: &RefitSource,
) -> Result<(RefitTitle, Vec<RefitSceneRow>), String> {
    match run {
        HelperRun::Stats(output) => {
            let source = source.clone();
            tokio::task::spawn_blocking(move || dv_refit_title::fit(output).rows(&source))
                .await
                .map_err(|error| format!("F+ solve: {error}"))?
        }
        HelperRun::Refused(reason) => Ok((dv_refit_title::refusal(source, &reason), Vec::new())),
    }
}

/// How an attempt that has the helper's answer settles: publication, or the
/// settlement that says why not. `None` once published, and when the
/// attempt lost the title (its token no longer settles anything).
pub(super) async fn settle_run(
    fence: &crate::background_jobs::JobFence,
    job: &plurx_core::store::background_jobs::BackgroundJob,
    source: &RefitSource,
    run: Result<HelperRun, RunError>,
) -> Option<JobSettlement> {
    let retry = |code: &str| JobSettlement::Retry {
        error_code: code.into(),
        not_before_ms: now_ms().saturating_add(crate::background_jobs::retry_delay_ms(
            &job.id,
            job.failed_attempts,
        )),
    };
    let fail = |code: &str| JobSettlement::Fail {
        error_code: code.into(),
    };
    let run = match run {
        Ok(run) => run,
        Err(RunError::Transient(error)) => {
            tracing::warn!(target: "plurxd::dv", file_id = source.file_id, %error, "F+ fit did not finish");
            return Some(retry("refit_failed"));
        }
        Err(RunError::Malformed(error)) => {
            // The same bits through the same helper print the same thing.
            tracing::warn!(target: "plurxd::dv", file_id = source.file_id, %error,
                "F+ helper output refused; nothing was stored");
            return Some(fail("refit_output_malformed"));
        }
    };
    let (title, scenes) = match rows(run, source).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(target: "plurxd::dv", file_id = source.file_id, %error, "F+ solve failed");
            return Some(fail("refit_solve_failed"));
        }
    };
    let verdict = title.verdict;
    let gain = title.gain;
    match fence.publish_dv_refit(title, scenes).await {
        Ok(RefitPublication::Published) => {
            tracing::info!(target: "plurxd::dv", file_id = source.file_id, ?verdict, ?gain,
                "F+ curves stored");
            None
        }
        Ok(RefitPublication::Fenced) => None,
        Ok(RefitPublication::Invalid(error)) => {
            tracing::warn!(target: "plurxd::dv", file_id = source.file_id, %error,
                "F+ table refused before publication");
            Some(fail("refit_rows_invalid"))
        }
        Err(error) => {
            tracing::warn!(%error, "F+ publication unavailable");
            Some(retry("refit_publication_failed"))
        }
    }
}

/// How an interrupted attempt settles. A preemption that kept new scenes
/// yields (the next attempt resumes there); one that kept nothing new adds
/// to a stall count, and past [`MAX_STALLED_YIELDS`] it retries instead.
pub(super) fn interrupted(
    job: &plurx_core::store::background_jobs::BackgroundJob,
    kept_before: usize,
    kept_after: usize,
) -> JobSettlement {
    let stalled = if kept_after > kept_before {
        0
    } else {
        job.checkpoint
            .as_ref()
            .and_then(|c| c.get(STALLED_YIELDS))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
            .saturating_add(1)
    };
    if stalled > MAX_STALLED_YIELDS {
        return JobSettlement::Retry {
            error_code: "refit_preempted".into(),
            not_before_ms: now_ms().saturating_add(crate::background_jobs::retry_delay_ms(
                &job.id,
                job.failed_attempts,
            )),
        };
    }
    JobSettlement::Yield {
        error_code: Some("refit_interrupted".into()),
        not_before_ms: now_ms().saturating_add(5000),
        checkpoint: Some(serde_json::Value::Object(
            [(STALLED_YIELDS.to_owned(), stalled.into())]
                .into_iter()
                .collect(),
        )),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one owned fit attempt retains source, tools, physical and durable admission"
)]
pub(super) async fn execute(
    state: AppState,
    file: plurx_core::domain::MediaFile,
    source: RefitSource,
    tools: Arc<ToolSet>,
    admission: crate::transcode::FragmentAdmission,
    job: plurx_core::store::background_jobs::BackgroundJob,
    active: ActiveBackgroundJob,
    shutdown: CancellationToken,
) -> bool {
    let fence = active.fence();
    let cancel = fence.loss_token().child_token();
    let spool = Spool::new(
        Spool::root(state.transcode.runtime_cache_dir()),
        source.file_id,
        source.source_size,
        source.source_mtime,
        source.video_index,
        &source.helper_identity,
    );
    let kept_before = spool.partial().await.len();
    let source_version = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .ok();
    let index = u8::try_from(source.video_index).unwrap_or(u8::MAX);
    let run = super::run_source_admitted_probe(
        &state.transcode,
        &admission,
        &cancel,
        &shutdown,
        None,
        &fence,
        || async {
            source_version.is_some()
                && tools.is_current().await
                && crate::fragment_index_cluster::inspect_source(&file)
                    .await
                    .ok()
                    == source_version
        },
        super::dv_refit::run(&file, index, &tools, &spool),
    )
    .await;
    let mut published = false;
    let settlement = if shutdown.is_cancelled() || cancel.is_cancelled() {
        // A scheduler preemption (a waiting viewer, a probe, shutdown) keeps
        // the token, and its Yield requeues the same identity; the run's
        // whole scene lines are in the spool to resume from. A cancel from
        // the jobs page or a source change (the dv_refit supersession
        // trigger) already moved the job out of `running`, so this settles
        // nothing and the identity's terminal outcome stands.
        Some(interrupted(&job, kept_before, spool.partial().await.len()))
    } else if snapshot(&file.path).await != Some((source.source_size, source.source_mtime)) {
        Some(JobSettlement::Stop {
            error_code: "refit_source_changed".into(),
        })
    } else {
        let settlement = settle_run(&fence, &job, &source, run).await;
        published = settlement.is_none()
            && state
                .store
                .background_job(&job.id)
                .await
                .ok()
                .flatten()
                .is_some_and(|job| {
                    job.state == plurx_core::store::background_jobs::JobState::Succeeded
                });
        settlement
    };
    if let Some(settlement) = settlement {
        if let Err(error) = fence.settle(settlement).await {
            tracing::warn!(%error, "F+ settlement unavailable");
        }
    }
    active.finish().await;
    drop(admission);
    published
}
