//! One owned future inside the existing source-probe scheduler. No new daemon.
#[cfg(test)]
#[path = "title_work_tests.rs"]
mod tests;

use super::{now_ms, snapshot};
use crate::{
    background_jobs::ActiveBackgroundJob, state::AppState, vodencode::dv_runtime::ToolSet,
};
use plurx_core::{
    error::StoreError,
    store::{
        background_jobs::{
            CandidateCursor, CandidateQuery, ClaimJob, EnqueueJob, EnqueueOutcome, JobKind,
            JobPayload, JobRequest, JobSettlement,
        },
        dv_title_jobs::{TitleDemand, TitleOutput},
    },
};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(super) struct TitleTask {
    pub(super) work: Pin<Box<dyn Future<Output = bool> + Send>>,
    cancel: CancellationToken,
    job_id: String,
    probe_pipeline: String,
    probe_after: Option<CandidateCursor>,
}
impl TitleTask {
    /// Discovery is bounded by the existing runner tick. Durable demand survives
    /// crashes/queue pressure; the shared request identity suppresses duplicates.
    pub(super) async fn start(
        state: &AppState,
        boot: &str,
        after: &mut i64,
    ) -> Result<Option<Self>, StoreError> {
        let authority = state.jobs.execution_authority();
        if !authority.may_execute_job(JobKind::DvTitleScan).await {
            return Ok(None);
        }
        let Some(admission) = state.transcode.admit_dv_title().await else {
            return Ok(None);
        };
        let tools = match ToolSet::capture().await {
            Ok(Some(tools)) => tools,
            Ok(None) => return Ok(None),
            Err(error) => {
                tracing::debug!(%error,"title scanner unavailable");
                return Ok(None);
            }
        };
        let Some(probe_pipeline) = plurx_core::scan::probe::pipeline_digest().await else {
            return Ok(None);
        };
        let scanner = tools.title_scanner_digest();
        let demands = state.store.dv_title_demands(&scanner, *after).await?;
        if demands.is_empty() {
            *after = 0;
            #[cfg(target_os = "linux")]
            requalify_stale_fel_titles(state, &tools, &scanner).await;
            return Ok(None);
        }
        for mut demand in demands {
            *after = demand.probe_revision;
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
            // The local index pass reads this title next, and reads its RPUs
            // in that same read: do not read the file twice.
            if state.jobs.title_scan_awaits_index_pass(file.id) {
                continue;
            }
            let Some(revision) = state
                .store
                .bind_dv_title_demand(demand.probe_revision, &scanner)
                .await?
            else {
                continue;
            };
            let reused = reusable_aggregate(&demand, revision);
            demand.probe_revision = revision;
            use sha2::{Digest, Sha256};
            let identity = hex::encode(Sha256::digest(
                serde_json::to_vec(&(
                    "dv-title-v1",
                    demand.file_id,
                    demand.probe_revision,
                    &scanner,
                ))
                .expect("title request identity"),
            ));
            let now = now_ms();
            let outcome = state
                .store
                .enqueue_job(EnqueueJob {
                    id: uuid::Uuid::new_v4().to_string(),
                    payload: JobPayload::DvTitleScan {
                        file_id: file.id,
                        source_size: file.size,
                        source_mtime: file.mtime,
                        probe_revision: demand.probe_revision,
                        scanner_digest: scanner.clone(),
                    },
                    dedupe_key: format!("dv-title:{identity}"),
                    priority: 0,
                    not_before_ms: now,
                    now_ms: now,
                    request: JobRequest {
                        scope: "dv-title".into(),
                        request_id: identity.clone(),
                        request_digest: identity,
                        consumer_kind: "dv_title_scan".into(),
                        consumer_ref: file.id.to_string(),
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await?;
            let id = match outcome {
                EnqueueOutcome::Accepted { job_id, .. }
                | EnqueueOutcome::Existing {
                    job_id,
                    cancelled: false,
                    ..
                } => job_id,
                _ => continue,
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
                    kind: JobKind::DvTitleScan,
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
                .ok_or_else(|| StoreError::Task("claimed title has no token".into()))?;
            let active = ActiveBackgroundJob::start(
                Arc::clone(&state.store),
                Arc::clone(&authority),
                token,
                deadline,
                JobKind::DvTitleScan,
            )?;
            let cancel = CancellationToken::new();
            let work = execute(
                state.clone(),
                file,
                demand,
                reused,
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

    /// Scheduler checks only locally readable, compatible, live ordinary work.
    /// Durable SQL excludes stale coordinators and other resource blockers.
    pub(super) async fn should_yield(&mut self, state: &AppState) -> Result<bool, StoreError> {
        if !state
            .jobs
            .execution_authority()
            .may_execute_job(JobKind::MediaProbe)
            .await
        {
            return Ok(false);
        }
        let now = now_ms();
        let page = state
            .store
            .job_candidates(CandidateQuery {
                node_id: state.node_id.clone(),
                kinds: vec![JobKind::MediaProbe],
                after: self.probe_after.clone(),
                now_ms: now,
                limit: 128,
            })
            .await?;
        // Keep the keyset walk across scheduler ticks. A fixed first page can
        // hide runnable probes behind locally unreadable or stale candidates.
        self.probe_after = page.next;
        for job in page.jobs {
            let Ok(JobPayload::MediaProbe {
                file_id,
                source_size,
                source_mtime,
                probe_digest,
                ..
            }) = job.supported_payload()
            else {
                continue;
            };
            if probe_digest != self.probe_pipeline {
                continue;
            }
            let Some(file) = state.store.get_file(file_id).await? else {
                continue;
            };
            if (file.size, file.mtime) != (source_size, source_mtime)
                || snapshot(&file.path).await != Some((source_size, source_mtime))
                || tokio::fs::File::open(&file.path).await.is_err()
            {
                continue;
            }
            if state
                .store
                .probe_can_preempt_title(&job.id, &self.job_id, &state.node_id, now)
                .await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) async fn stop(mut self) -> bool {
        self.cancel.cancel();
        // Never drop the child future to release the lane: reap, settle, finish.
        self.work.as_mut().await
    }
}

/// A renderer, recipe or encoder change leaves every FEL verdict measured
/// under the old identity. While no title work waits, ask for a bounded batch
/// of those titles again: their demand is rebound, and the title job rescans
/// and re-qualifies them. Title demand no longer follows the renderer, so this
/// is what keeps "a changed renderer re-qualifies every title" true; the
/// request is throttled per title like a play's.
#[cfg(target_os = "linux")]
async fn requalify_stale_fel_titles(state: &AppState, tools: &ToolSet, scanner: &str) {
    if !state.transcode.fel_qualification_capable() {
        return;
    }
    let identity = match state.transcode.fel_identity(&tools.backend_digest()).await {
        Ok(identity) => identity,
        Err(error) => {
            tracing::debug!(%error, "FEL identity unavailable for re-qualification");
            return;
        }
    };
    match state.store.dv_fel_stale_titles(&identity, 16).await {
        Ok(titles) => {
            for file_id in titles {
                state
                    .transcode
                    .request_fel_qualification(file_id, scanner, &identity)
                    .await;
            }
        }
        Err(error) => tracing::warn!(%error, "listing FEL titles to re-qualify"),
    }
}

/// The aggregate this title's job may settle with instead of reading the
/// file: one stored under the generation the worker's bind left unchanged.
/// Only a publisher bound to that generation's scanner stores one (an index
/// pass's ride-along, or an earlier job), and a bind to another scanner
/// rotates the generation and drops it in the same transaction — so a
/// generation that moved, or a document read before it moved, reuses nothing.
pub(super) fn reusable_aggregate(
    demand: &TitleDemand,
    bound_revision: i64,
) -> Option<plurx_core::scan::dv_title::DvTitleStats> {
    if bound_revision != demand.probe_revision {
        return None;
    }
    plurx_core::scan::dv_title::DvTitleStats::from_probe_json(&demand.probe_json)
}

#[expect(
    clippy::too_many_arguments,
    reason = "one owned title attempt retains source, tools, physical and durable admission"
)]
async fn execute(
    state: AppState,
    file: plurx_core::domain::MediaFile,
    demand: TitleDemand,
    reused: Option<plurx_core::scan::dv_title::DvTitleStats>,
    tools: Arc<ToolSet>,
    admission: crate::transcode::FragmentAdmission,
    job: plurx_core::store::background_jobs::BackgroundJob,
    active: ActiveBackgroundJob,
    shutdown: CancellationToken,
) -> bool {
    let fence = active.fence();
    let cancel = fence.loss_token().child_token();
    let result = match reused {
        Some(stats) => Ok(stats),
        None => {
            super::run_admitted_probe(
                &state.transcode,
                &admission,
                &cancel,
                &shutdown,
                None,
                super::dv_title::scan(&file, &demand.probe_json, &tools),
            )
            .await
        }
    };
    let mut published = false;
    let settlement = if shutdown.is_cancelled() || cancel.is_cancelled() {
        // A manual cancel/source supersession has already invalidated our token,
        // so this cannot turn it back into queued work. A scheduler preemption
        // has not: its ordinary Yield retains the same durable demand.
        Some(JobSettlement::Yield {
            error_code: Some("title_interrupted".into()),
            not_before_ms: now_ms().saturating_add(5000),
            checkpoint: None,
        })
    } else if snapshot(&file.path).await != Some((demand.source_size, demand.source_mtime)) {
        Some(JobSettlement::Stop {
            error_code: "title_source_changed".into(),
        })
    } else {
        match result {
            Ok(stats) => {
                // A Profile 7 FEL title is qualified for the reconstructed
                // 8.1 here, once, while this job holds it. Foreground work
                // preempts only the qualification: the aggregate is still
                // published, and the verdict stays missing until a later play
                // asks again.
                let qualification = cancel.child_token();
                match super::run_admitted_probe(
                    &state.transcode,
                    &admission,
                    &qualification,
                    &shutdown,
                    None,
                    super::fel_qualification::qualify_if_due(
                        &state,
                        &file,
                        &demand.probe_json,
                        &tools,
                        &admission,
                        &qualification,
                    ),
                )
                .await
                {
                    Ok(super::fel_qualification::Qualification::Stored(verdict)) => {
                        tracing::debug!(target: "plurxd::dv", file_id = file.id,
                            admitted = verdict.admitted, "FEL qualification stored");
                    }
                    Ok(outcome) => {
                        tracing::debug!(target: "plurxd::dv", file_id = file.id, ?outcome, "FEL qualification");
                    }
                    Err(error) => {
                        tracing::warn!(target: "plurxd::dv", file_id = file.id, %error,
                            "FEL qualification did not complete; the title keeps the plain conversion");
                    }
                }
                let output = TitleOutput {
                    file_id: file.id,
                    probe_revision: demand.probe_revision,
                    source_size: demand.source_size,
                    source_mtime: demand.source_mtime,
                    scanner_digest: tools.title_scanner_digest(),
                    stats,
                };
                match fence.publish_dv_title(output).await {
                    Ok(true) => {
                        published = true;
                        None
                    }
                    Ok(false) => Some(JobSettlement::Stop {
                        error_code: "title_publication_fenced".into(),
                    }),
                    Err(error) => {
                        tracing::warn!(%error,"title publication unavailable");
                        Some(JobSettlement::Retry {
                            error_code: "title_publication_failed".into(),
                            not_before_ms: now_ms().saturating_add(
                                crate::background_jobs::retry_delay_ms(
                                    &job.id,
                                    job.failed_attempts,
                                ),
                            ),
                        })
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error,"complete-title scan failed");
                Some(JobSettlement::Retry {
                    error_code: "title_scan_failed".into(),
                    not_before_ms: now_ms().saturating_add(crate::background_jobs::retry_delay_ms(
                        &job.id,
                        job.failed_attempts,
                    )),
                })
            }
        }
    };
    if let Some(settlement) = settlement {
        if let Err(error) = fence.settle(settlement).await {
            tracing::warn!(%error,"title settlement unavailable");
        }
    }
    active.finish().await;
    drop(admission);
    published
}
