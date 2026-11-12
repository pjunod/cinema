//! The F+ curve fit (`JobKind::DvRefit`) of a Profile 7 FEL title, on the
//! same title lane as the complete-title scan and shaped like it
//! (`docs/streaming/FEL-RPU-REFIT-BUILD.md` §3.3): durable demand, the
//! shared request identity, a fenced claim with lease and heartbeat, the
//! held-source helper run labelled for Activity, and one fenced publication.
//! It yields to ordinary media probes as the title scan does, and it is
//! listed, cancellable and retryable from the background-jobs page, because
//! it decodes a whole title on the CPU.
#[cfg(test)]
#[path = "refit_work_tests.rs"]
mod tests;

use super::{dv_refit::HelperRun, dv_refit::RunError, now_ms, snapshot};
use crate::{
    background_jobs::ActiveBackgroundJob, state::AppState, vodencode::dv_runtime::ToolSet,
};
use plurx_core::{
    error::StoreError,
    store::{
        background_jobs::{
            CandidateCursor, ClaimJob, EnqueueJob, EnqueueOutcome, JobKind, JobPayload, JobRequest,
            JobSettlement,
        },
        dv_refit_jobs::{RefitDemand, RefitTitle, StagedScene},
    },
    transcode::{dv_refit::FIT_VERSION, dv_refit_title},
};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(super) struct RefitTask {
    pub(super) work: Pin<Box<dyn Future<Output = bool> + Send>>,
    cancel: CancellationToken,
    job_id: String,
    probe_pipeline: String,
    probe_after: Option<CandidateCursor>,
}

/// The request identity: the source, the title-scan generation whose facts
/// the fit reads, the helper and the fit method (§3.3).
pub(super) fn request_identity(demand: &RefitDemand, helper: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            "dv-refit-v1",
            demand.file_id,
            demand.source_size,
            demand.source_mtime,
            demand.probe_revision,
            helper,
            FIT_VERSION,
        ))
        .expect("refit request identity"),
    ))
}

pub(super) fn enqueue_request(demand: &RefitDemand, helper: &str, now: i64) -> EnqueueJob {
    let identity = request_identity(demand, helper);
    EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: JobPayload::DvRefit {
            file_id: demand.file_id,
            source_size: demand.source_size,
            source_mtime: demand.source_mtime,
            probe_revision: demand.probe_revision,
            helper_digest: helper.into(),
            fit_version: FIT_VERSION.into(),
        },
        dedupe_key: format!("dv-refit:{identity}"),
        priority: 0,
        not_before_ms: now,
        now_ms: now,
        request: JobRequest {
            scope: "dv-refit".into(),
            request_id: identity.clone(),
            request_digest: identity,
            consumer_kind: "dv_refit".into(),
            consumer_ref: demand.file_id.to_string(),
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
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
        let helper = tools.refit_helper_digest();
        let demands = state
            .store
            .dv_refit_demands(&helper, FIT_VERSION, *after, 16)
            .await?;
        if demands.is_empty() {
            *after = 0;
            return Ok(None);
        }
        let Some(admission) = state.transcode.admit_dv_refit().await else {
            return Ok(None);
        };
        for demand in demands {
            *after = demand.file_id;
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
            let now = now_ms();
            let id = if let Some(id) = demand.queued_job_id.clone() {
                id
            } else {
                match state
                    .store
                    .enqueue_job(enqueue_request(&demand, &helper, now))
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
            let work = execute(
                state.clone(),
                file,
                demand,
                tools,
                admission,
                job,
                active,
                cancel.clone(),
            );
            return Ok(Some(Self {
                work: Box::pin(work),
                cancel,
                job_id: id,
                probe_pipeline,
                probe_after: None,
            }));
        }
        Ok(None)
    }

    pub(super) async fn should_yield(&mut self, state: &AppState) -> Result<bool, StoreError> {
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

/// The rows a finished helper run publishes: a solved title, or a recorded
/// refusal. Solving is CPU work over every scene, so it runs off the runtime.
pub(super) async fn rows(
    run: HelperRun,
    demand: &RefitDemand,
    helper: &str,
) -> Result<(RefitTitle, Vec<StagedScene>), String> {
    match run {
        HelperRun::Stats(output) => {
            let demand = demand.clone();
            let helper = helper.to_owned();
            tokio::task::spawn_blocking(move || {
                dv_refit_title::fit(output).rows(
                    demand.file_id,
                    demand.source_size,
                    demand.source_mtime,
                    demand.probe_revision,
                    &helper,
                )
            })
            .await
            .map_err(|error| format!("F+ solve: {error}"))?
        }
        HelperRun::Refused(reason) => Ok((
            dv_refit_title::refusal(
                demand.file_id,
                demand.source_size,
                demand.source_mtime,
                demand.probe_revision,
                helper,
                &reason,
            ),
            Vec::new(),
        )),
    }
}

/// How an attempt that read the title settles: publication, or the
/// settlement that says why not. `None` once published.
pub(super) async fn settle_run(
    fence: &crate::background_jobs::JobFence,
    job: &plurx_core::store::background_jobs::BackgroundJob,
    demand: &RefitDemand,
    helper: &str,
    run: Result<HelperRun, RunError>,
) -> Option<JobSettlement> {
    let retry = |code: &str| JobSettlement::Retry {
        error_code: code.into(),
        not_before_ms: now_ms().saturating_add(crate::background_jobs::retry_delay_ms(
            &job.id,
            job.failed_attempts,
        )),
    };
    let run = match run {
        Ok(run) => run,
        Err(RunError::Transient(error)) => {
            tracing::warn!(target: "plurxd::dv", file_id = demand.file_id, %error, "F+ fit did not finish");
            return Some(retry("refit_failed"));
        }
        Err(RunError::Malformed(error)) => {
            // The same bits through the same helper print the same thing.
            tracing::warn!(target: "plurxd::dv", file_id = demand.file_id, %error,
                "F+ helper output refused; nothing was stored");
            return Some(JobSettlement::Fail {
                error_code: "refit_output_malformed".into(),
            });
        }
    };
    let (title, scenes) = match rows(run, demand, helper).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(target: "plurxd::dv", file_id = demand.file_id, %error, "F+ solve failed");
            return Some(retry("refit_solve_failed"));
        }
    };
    let verdict = title.verdict;
    let gain = title.gain;
    match fence.publish_dv_refit(title, scenes).await {
        Ok(true) => {
            tracing::info!(target: "plurxd::dv", file_id = demand.file_id, ?verdict, ?gain,
                "F+ curves stored");
            None
        }
        Ok(false) => Some(JobSettlement::Stop {
            error_code: "refit_publication_fenced".into(),
        }),
        Err(error) => {
            tracing::warn!(%error, "F+ publication unavailable");
            Some(retry("refit_publication_failed"))
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one owned fit attempt retains source, tools, physical and durable admission"
)]
async fn execute(
    state: AppState,
    file: plurx_core::domain::MediaFile,
    demand: RefitDemand,
    tools: Arc<ToolSet>,
    admission: crate::transcode::FragmentAdmission,
    job: plurx_core::store::background_jobs::BackgroundJob,
    active: ActiveBackgroundJob,
    shutdown: CancellationToken,
) -> bool {
    let fence = active.fence();
    let cancel = fence.loss_token().child_token();
    let helper = tools.refit_helper_digest();
    let source_version = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .ok();
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
        super::dv_refit::run(&file, &demand.probe_json, &tools),
    )
    .await;
    let mut published = false;
    let settlement = if shutdown.is_cancelled() || cancel.is_cancelled() {
        // A manual cancel or a source supersession already invalidated the
        // token, so this cannot requeue it; a scheduler preemption has not,
        // and its Yield keeps the same durable demand. Either way the
        // attempt's staged scenes, if any, go with it.
        Some(JobSettlement::Yield {
            error_code: Some("refit_interrupted".into()),
            not_before_ms: now_ms().saturating_add(5000),
            checkpoint: None,
        })
    } else if snapshot(&file.path).await != Some((demand.source_size, demand.source_mtime)) {
        Some(JobSettlement::Stop {
            error_code: "refit_source_changed".into(),
        })
    } else {
        let settlement = settle_run(&fence, &job, &demand, &helper, run).await;
        published = settlement.is_none();
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
