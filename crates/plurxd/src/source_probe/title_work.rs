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
    scanner: String,
    backend_digest: String,
    file_id: i64,
    probe_revision: i64,
    playback_requested: bool,
    playback_recipe: Option<String>,
    live_after: i64,
}
impl TitleTask {
    /// Discovery is bounded by the existing runner tick. Durable demand survives
    /// crashes/queue pressure; the shared request identity suppresses duplicates.
    pub(super) async fn start(
        state: &AppState,
        boot: &str,
        after: &mut i64,
        live_after: &mut i64,
        playback_only: bool,
    ) -> Result<Option<Self>, StoreError> {
        let authority = state.jobs.execution_authority();
        if !authority.may_execute_job(JobKind::DvTitleScan).await {
            return Ok(None);
        }
        // Maintenance retains its early physical idle check; only a real
        // viewer request may use the companion admission below.
        let mut ordinary_admission = if playback_only {
            None
        } else {
            let Some(admission) = state.transcode.admit_dv_title(false).await else {
                return Ok(None);
            };
            Some(admission)
        };
        // Empty viewer queues must not hash installed bundles or submit the
        // maintenance outbox's replicated reopen operation on every probe pass.
        let live_demands = if playback_only {
            let demands = state
                .store
                .dv_title_playback_demands(*live_after, now_ms(), None)
                .await?;
            if demands.is_empty() {
                *live_after = 0;
                return Ok(None);
            }
            let mut dispatchable = Vec::new();
            for demand in demands {
                *live_after = demand.probe_revision;
                let Some(id) = demand.active_job_id.as_deref() else {
                    continue;
                };
                let Some(job) = state.store.background_job(id).await? else {
                    continue;
                };
                let now = now_ms();
                let healthy_owner = job
                    .token
                    .as_ref()
                    .is_some_and(|token| token.lease_expires_ms > now);
                if job.state == plurx_core::store::background_jobs::JobState::Running
                    && healthy_owner
                {
                    continue;
                }
                if job.not_before_ms > now
                    || job.failed_attempts >= job.attempt_limit
                    || (job.retry_deadline_ms != 0 && job.retry_deadline_ms <= now)
                {
                    continue;
                }
                dispatchable.push(demand);
            }
            if dispatchable.is_empty() {
                return Ok(None);
            }
            Some(dispatchable)
        } else {
            None
        };
        let tools = match ToolSet::capture().await {
            Ok(Some(tools)) => tools,
            Ok(None) => return Ok(None),
            Err(error) => {
                tracing::debug!(%error,"title scanner unavailable");
                return Ok(None);
            }
        };
        let probe_pipeline = if playback_only {
            String::new()
        } else {
            let Some(pipeline) = plurx_core::scan::probe::pipeline_digest().await else {
                return Ok(None);
            };
            pipeline
        };
        let scanner = tools.title_scanner_digest();
        let demands = match live_demands {
            Some(demands) => demands,
            None => {
                state
                    .store
                    .dv_title_demands(&scanner, *after, now_ms())
                    .await?
            }
        };
        if demands.is_empty() {
            if playback_only {
                return Ok(None);
            }
            *after = 0;
            #[cfg(target_os = "linux")]
            requalify_stale_fel_titles(state, &tools, &scanner).await;
            return Ok(None);
        }
        for mut demand in demands {
            if playback_only {
                *live_after = demand.probe_revision;
                if demand.scanner_digest.as_deref() != Some(scanner.as_str()) {
                    continue;
                }
            } else {
                *after = demand.probe_revision;
                // Live work was considered independently above; an ordinary
                // turn must not acquire its more permissive companion class.
                if demand.playback_requested {
                    continue;
                }
            }
            let Some(file) = eligible_file(state, &demand, &tools.backend_digest()).await? else {
                continue;
            };
            // The local index pass reads this title next, and reads its RPUs
            // in that same read: do not read the file twice.
            if !demand.playback_requested && state.jobs.title_scan_awaits_index_pass(file.id) {
                continue;
            }
            let playback_recipe = if demand.playback_requested {
                let backend = state
                    .transcode
                    .fel_identity(&tools.backend_digest())
                    .await
                    .ok();
                let selected = plurx_core::transcode::dv_fel_qualification::pending_recipes(
                    &demand.probe_json,
                    file.size,
                    file.mtime,
                )
                .into_iter()
                .find(|request| {
                    Some(&request.backend_identity) == backend.as_ref()
                        && demand.playback_recipes.contains(&request.identity)
                })
                .map(|request| request.identity);
                let Some(identity) = selected else {
                    continue;
                };
                demand.playback_recipes = vec![identity.clone()];
                Some(identity)
            } else {
                None
            };
            let admission = if let Some(admission) = ordinary_admission.take() {
                admission
            } else {
                let Some(admission) = state
                    .transcode
                    .admit_dv_title(demand.playback_requested)
                    .await
                else {
                    return Ok(None);
                };
                admission
            };
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
            let mut fel_requests: Vec<_> =
                plurx_core::transcode::dv_fel_qualification::pending_recipes(
                    &demand.probe_json,
                    file.size,
                    file.mtime,
                )
                .into_iter()
                .map(|r| r.identity)
                .collect();
            fel_requests.sort();
            let identity = hex::encode(Sha256::digest(
                serde_json::to_vec(&(
                    "dv-title-v2-exact-fel-recipes",
                    demand.file_id,
                    demand.probe_revision,
                    &scanner,
                    &fel_requests,
                ))
                .expect("title request identity"),
            ));
            let now = now_ms();
            let id = if let Some(id) = demand
                .active_job_id
                .clone()
                .or_else(|| demand.queued_job_id.clone())
            {
                id
            } else {
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
                        dedupe_key: format!(
                            "dv-title-generation:{}:{}:{}",
                            file.id, demand.probe_revision, scanner
                        ),
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
                match outcome {
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
            )
            .await?;
            let cancel = CancellationToken::new();
            let file_id = file.id;
            let probe_revision = demand.probe_revision;
            let playback_requested = demand.playback_requested;
            let backend_digest = tools.backend_digest();
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
                scanner,
                backend_digest,
                file_id,
                probe_revision,
                playback_requested,
                playback_recipe,
                live_after: 0,
            }));
        }
        Ok(None)
    }

    /// Scheduler checks only locally readable, compatible, live ordinary work.
    /// Durable SQL excludes stale coordinators and other resource blockers.
    pub(super) async fn should_yield(&mut self, state: &AppState) -> Result<bool, StoreError> {
        let now = now_ms();
        if self.playback_requested {
            // Owned interest is checked directly, never through a page that
            // unrelated renewed viewers can push us out of.
            let own = state
                .store
                .dv_title_playback_demands(0, now, Some(self.file_id))
                .await?;
            for demand in own {
                if demand.probe_revision == self.probe_revision
                    && self
                        .playback_recipe
                        .as_ref()
                        .is_some_and(|recipe| demand.playback_recipes.contains(recipe))
                    && demand.scanner_digest.as_deref() == Some(self.scanner.as_str())
                    && eligible_file(state, &demand, &self.backend_digest)
                        .await?
                        .is_some()
                {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        let demands = state
            .store
            .dv_title_playback_demands(self.live_after, now, None)
            .await?;
        if demands.is_empty() {
            self.live_after = 0;
        }
        for demand in demands {
            self.live_after = demand.probe_revision;
            if demand.scanner_digest.as_deref() != Some(self.scanner.as_str())
                || eligible_file(state, &demand, &self.backend_digest)
                    .await?
                    .is_none()
            {
                continue;
            }
            if demand.file_id == self.file_id && demand.probe_revision == self.probe_revision {
                return Ok(true); // Promote this already-owned maintenance job.
            }
            if let Some(candidate) = demand.queued_job_id.as_deref() {
                if state
                    .store
                    .dv_title_can_preempt(candidate, &self.job_id, &state.node_id, now)
                    .await?
                {
                    return Ok(true);
                }
            }
        }
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

/// Scheduling interest never substitutes for a locally readable current source
/// or for the backend able to satisfy its exact pending FEL recipe.
async fn eligible_file(
    state: &AppState,
    demand: &TitleDemand,
    backend_digest: &str,
) -> Result<Option<plurx_core::domain::MediaFile>, StoreError> {
    let Some(file) = state.store.get_file(demand.file_id).await? else {
        return Ok(None);
    };
    if file.size != demand.source_size
        || file.mtime != demand.source_mtime
        || snapshot(&file.path).await != Some((file.size, file.mtime))
        || tokio::fs::File::open(&file.path).await.is_err()
    {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    {
        let pending = plurx_core::transcode::dv_fel_qualification::pending_recipes(
            &demand.probe_json,
            file.size,
            file.mtime,
        );
        if demand.playback_requested && pending.is_empty() {
            return Ok(None);
        }
        if !pending.is_empty() {
            let backend = state.transcode.fel_identity(backend_digest).await.ok();
            if !pending.iter().any(|request| {
                Some(&request.backend_identity) == backend.as_ref()
                    && (!demand.playback_requested
                        || demand.playback_recipes.contains(&request.identity))
            }) {
                return Ok(None);
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = backend_digest;
    Ok(Some(file))
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
    let reused = reused.or_else(|| {
        job.checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.get("fel_title_aggregate"))
            .and_then(|value| serde_json::from_value::<TitleOutput>(value.clone()).ok())
            .filter(|out| {
                out.file_id == file.id
                    && out.probe_revision == demand.probe_revision
                    && out.source_size == file.size
                    && out.source_mtime == file.mtime
                    && out.scanner_digest == tools.title_scanner_digest()
                    && out.stats.complete
            })
            .map(|out| out.stats)
    });
    let source_version = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .ok();
    let result = acquire_title(
        demand.playback_requested,
        reused,
        super::run_source_admitted_probe(
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
            super::dv_title::scan(&file, &demand.probe_json, &tools),
        ),
        qualify_owned(
            &state,
            &file,
            &demand,
            &tools,
            &admission,
            &cancel,
            &shutdown,
            &fence,
            &source_version,
        ),
    )
    .await;
    if let Ok(TitleAcquisition::QualificationOnly(reused)) = result {
        // The independently stored quality receipt can serve the viewer now.
        // Complete-title statistics remain separate maintenance work: never
        // use a live admission to scan the full source or invent an aggregate.
        let settlement =
            if snapshot(&file.path).await != Some((demand.source_size, demand.source_mtime)) {
                JobSettlement::Stop {
                    error_code: "title_source_changed".into(),
                }
            } else {
                JobSettlement::Yield {
                    error_code: Some(
                        if shutdown.is_cancelled() || cancel.is_cancelled() {
                            "title_interrupted"
                        } else {
                            "title_qualification_continuation"
                        }
                        .into(),
                    ),
                    not_before_ms: now_ms().saturating_add(1000),
                    checkpoint: reused.map(|stats| {
                        serde_json::json!({"fel_title_aggregate":TitleOutput {
                            file_id: file.id, probe_revision: demand.probe_revision,
                            source_size: demand.source_size, source_mtime: demand.source_mtime,
                            scanner_digest: tools.title_scanner_digest(), stats,
                        }})
                    }),
                }
            };
        if let Err(error) = fence.settle(settlement).await {
            tracing::warn!(%error,"title qualification continuation unavailable");
        }
        active.finish().await;
        drop(admission);
        return false;
    }
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
            Ok(TitleAcquisition::Complete(stats)) => {
                let output = TitleOutput {
                    file_id: file.id,
                    probe_revision: demand.probe_revision,
                    source_size: demand.source_size,
                    source_mtime: demand.source_mtime,
                    scanner_digest: tools.title_scanner_digest(),
                    stats,
                };
                let pending_here = match (
                    state.store.get_file_probe_json(file.id).await,
                    state.transcode.fel_identity(&tools.backend_digest()).await,
                ) {
                    (Ok(Some(document)), Ok(backend)) => {
                        plurx_core::transcode::dv_fel_qualification::pending_recipes(
                            &document, file.size, file.mtime,
                        )
                        .iter()
                        .any(|request| request.backend_identity == backend)
                    }
                    _ => true,
                };
                if pending_here {
                    Some(JobSettlement::Yield {
                        error_code: Some("fel_recipe_pending".into()),
                        not_before_ms: now_ms().saturating_add(5000),
                        checkpoint: Some(serde_json::json!({"fel_title_aggregate":output})),
                    })
                } else {
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
            }
            Ok(TitleAcquisition::QualificationOnly(_)) => {
                unreachable!("live work returned after yielding")
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

#[expect(
    clippy::too_many_arguments,
    reason = "qualification retains the title attempt's source, fence and admission"
)]
async fn qualify_owned(
    state: &AppState,
    file: &plurx_core::domain::MediaFile,
    demand: &TitleDemand,
    tools: &ToolSet,
    admission: &crate::transcode::FragmentAdmission,
    cancel: &CancellationToken,
    shutdown: &CancellationToken,
    fence: &crate::background_jobs::JobFence,
    source_version: &Option<String>,
) {
    let qualification = cancel.child_token();
    match super::run_source_admitted_probe(
        &state.transcode,
        admission,
        &qualification,
        shutdown,
        None,
        fence,
        || async {
            source_version.is_some()
                && tools.is_current().await
                && crate::fragment_index_cluster::inspect_source(file)
                    .await
                    .ok()
                    == *source_version
        },
        async {
            if demand.playback_requested {
                super::fel_qualification::qualify_requested_if_due(
                    state,
                    file,
                    &demand.probe_json,
                    tools,
                    admission,
                    &qualification,
                    demand
                        .playback_recipes
                        .first()
                        .map(String::as_str)
                        .unwrap_or(""),
                )
                .await
            } else {
                super::fel_qualification::qualify_if_due(
                    state,
                    file,
                    &demand.probe_json,
                    tools,
                    admission,
                    &qualification,
                )
                .await
            }
        },
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
}

/// A viewer's bounded quality receipt and the maintenance aggregate are
/// independent outputs. Constructing a scan future never starts its I/O.
enum TitleAcquisition {
    QualificationOnly(Option<plurx_core::scan::dv_title::DvTitleStats>),
    Complete(plurx_core::scan::dv_title::DvTitleStats),
}

async fn acquire_title<S, Q>(
    playback_requested: bool,
    reused: Option<plurx_core::scan::dv_title::DvTitleStats>,
    scan: S,
    qualify: Q,
) -> Result<TitleAcquisition, String>
where
    S: Future<Output = Result<plurx_core::scan::dv_title::DvTitleStats, String>>,
    Q: Future<Output = ()>,
{
    if playback_requested {
        qualify.await;
        return Ok(TitleAcquisition::QualificationOnly(reused));
    }
    let stats = match reused {
        Some(stats) => stats,
        None => scan.await?,
    };
    qualify.await;
    Ok(TitleAcquisition::Complete(stats))
}
