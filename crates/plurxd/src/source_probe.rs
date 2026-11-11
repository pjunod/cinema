//! Admitted leaf workers never mutate catalogue identity or scan completion.
#[cfg(target_os = "linux")]
mod dv_title;
#[cfg(target_os = "linux")]
pub(crate) mod fel_qualification;
#[cfg(target_os = "linux")]
mod title_work;

#[cfg(all(test, target_os = "linux"))]
pub(crate) use dv_title::{publish_title_for_control, publish_title_with_tools_for_control};

use crate::state::AppState;
use plurx_core::store::background_jobs_probe::{ProbeOutput, MAX_PROBE_RESULT_BYTES};
use plurx_core::{
    error::{ProbeError, StoreError},
    scan::probe,
    store::background_jobs::{
        CandidateCursor, CandidateQuery, ClaimJob, JobKind, JobPayload, JobSettlement,
    },
};
use std::{path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
async fn snapshot(path: &Path) -> Option<(i64, i64)> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    if !meta.is_file() {
        return None;
    }
    let size = i64::try_from(meta.len()).ok()?;
    let mtime = i64::try_from(
        meta.modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
    )
    .ok()?;
    Some((size, mtime))
}
pub(crate) async fn run(state: AppState, shutdown: CancellationToken) {
    let boot = uuid::Uuid::new_v4().to_string();
    let mut cursor = None;
    let mut idle = crate::background_jobs::IdlePoll::new();
    #[cfg(target_os = "linux")]
    let mut title: Option<title_work::TitleTask> = None;
    #[cfg(target_os = "linux")]
    let mut title_after = 0;
    loop {
        if shutdown.is_cancelled() {
            #[cfg(target_os = "linux")]
            if let Some(work) = title.take() {
                work.stop().await;
            }
            return;
        }
        #[cfg(target_os = "linux")]
        let mut yielded = false;
        #[cfg(target_os = "linux")]
        if let Some(work) = title.as_mut() {
            match work.should_yield(&state).await {
                Ok(true) => {
                    if let Some(work) = title.take() {
                        work.stop().await;
                    }
                    // Revisit ordinary priority order after the child is reaped.
                    cursor = None;
                    yielded = true;
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(%error,"title priority observation unavailable"),
            }
        }
        #[cfg(target_os = "linux")]
        let ordinary_idle = title.is_none();
        #[cfg(not(target_os = "linux"))]
        let ordinary_idle = true;
        let published = if ordinary_idle {
            let published = match pass(&state, &boot, &mut cursor, &shutdown).await {
                Ok(published) => published,
                Err(error) => {
                    tracing::warn!(%error, "leaf probe pass failed");
                    false
                }
            };
            published
        } else {
            false
        };
        #[cfg(target_os = "linux")]
        if title.is_none() && cursor.is_none() && !yielded && !published && !shutdown.is_cancelled()
        {
            match title_work::TitleTask::start(&state, &boot, &mut title_after).await {
                Ok(work) => title = work,
                Err(error) => tracing::warn!(%error,"title demand unavailable"),
            }
        }
        #[cfg(target_os = "linux")]
        {
            let title_poll = async {
                match title.as_mut() {
                    Some(work) => work.work.as_mut().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = shutdown.cancelled() => {},
                _ = title_poll => { title = None; },
                () = tokio::time::sleep(idle.after_completion(published)) => {},
            }
        }
        #[cfg(not(target_os = "linux"))]
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(idle.after_completion(published)) => {}
        }
    }
}

async fn pass(
    state: &AppState,
    boot: &str,
    cursor: &mut Option<CandidateCursor>,
    shutdown: &CancellationToken,
) -> Result<bool, StoreError> {
    let authority = state.jobs.execution_authority();
    if shutdown.is_cancelled() || !authority.may_execute_job(JobKind::MediaProbe).await {
        return Ok(false);
    }
    let mut page = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![JobKind::MediaProbe],
            after: cursor.clone(),
            now_ms: now_ms(),
            limit: 128,
        })
        .await?;
    page.jobs
        .retain(|job| matches!(job.supported_payload(), Ok(JobPayload::MediaProbe { .. })));
    if page.jobs.is_empty() {
        *cursor = page.next;
        return Ok(false);
    }
    let Some(admission) = state.transcode.admit_fragment().await else {
        return Ok(false); // Keep the incoming cursor on physical contention.
    };
    let admission = Arc::new(admission);
    // Reporter discovery may spawn a child, so it remains physically admitted.
    let Some(pipeline) = probe::pipeline_digest().await else {
        return Ok(false);
    };
    let limit = admission.probe_parallelism();
    let mut work = Vec::with_capacity(limit);
    let mut interrupted = false;
    for candidate in page.jobs {
        let Ok(payload @ JobPayload::MediaProbe { .. }) = candidate.supported_payload() else {
            continue;
        };
        let JobPayload::MediaProbe {
            file_id,
            source_size,
            source_mtime,
            ref probe_digest,
            ..
        } = payload
        else {
            unreachable!()
        };
        if probe_digest != &pipeline {
            continue;
        }
        let file = match state.store.get_file(file_id).await {
            Ok(Some(file)) => file,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(%error, "probe candidate read failed");
                interrupted = true;
                break;
            }
        };
        if file.size != source_size
            || file.mtime != source_mtime
            || snapshot(&file.path).await != Some((source_size, source_mtime))
        {
            continue;
        }
        if shutdown.is_cancelled()
            || !state.transcode.fragment_worker_idle(&admission)
            || !authority.may_execute_job(JobKind::MediaProbe).await
        {
            interrupted = true;
            break;
        }
        let now = now_ms();
        let claimed = crate::background_jobs::claim_with_resolution(
            state.store.as_ref(),
            &candidate,
            ClaimJob {
                job_id: candidate.id.clone(),
                expected_revision: candidate.revision,
                node_id: state.node_id.clone(),
                boot_id: boot.into(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::MediaProbe,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            },
        )
        .await;
        let (job, deadline) = match claimed {
            Ok(Some(claimed)) => claimed,
            Ok(None) => continue, // Shared-domain contention creates no attempt.
            Err(error) => {
                tracing::warn!(%error, "probe claim failed");
                interrupted = true;
                break;
            }
        };
        let Some(token) = job.token.clone() else {
            interrupted = true;
            break;
        };
        let active = match crate::background_jobs::ActiveBackgroundJob::start(
            Arc::clone(&state.store),
            Arc::clone(&authority),
            token,
            deadline,
            JobKind::MediaProbe,
        )
        .await
        {
            Ok(active) => active,
            Err(error) => {
                tracing::warn!(%error, "probe heartbeat failed");
                interrupted = true;
                break;
            }
        };
        // Heartbeat starts immediately, even while a second claim is resolved.
        work.push(execute(
            state,
            job,
            payload,
            file,
            active,
            Arc::clone(&admission),
            shutdown,
        ));
        if work.len() == limit {
            break;
        }
    }
    let had_work = !work.is_empty();
    // join_all deliberately does not short-circuit on a failed child. Every
    // child and heartbeat retires before the shared physical permit is freed.
    let results = futures_util::future::join_all(work).await;
    if had_work {
        *cursor = None;
    } else if !interrupted {
        *cursor = page.next;
    }
    Ok(had_work && !interrupted && results.into_iter().all(|published| published))
}

async fn execute(
    state: &AppState,
    job: plurx_core::store::background_jobs::BackgroundJob,
    payload: JobPayload,
    file: plurx_core::domain::MediaFile,
    active: crate::background_jobs::ActiveBackgroundJob,
    admission: Arc<crate::transcode::FragmentAdmission>,
    shutdown: &CancellationToken,
) -> bool {
    let JobPayload::MediaProbe {
        source_size,
        source_mtime,
        ..
    } = payload
    else {
        return false;
    };
    let fence = active.fence();
    let cancel = fence.loss_token().child_token();
    let progress = job.token.as_ref().map(|token| {
        state
            .jobs
            .start_probe_progress(token, file.id, file.size.max(0) as u64)
    });
    let source_version = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .ok();
    let probe_digest = match &payload {
        JobPayload::MediaProbe { probe_digest, .. } => probe_digest,
        _ => unreachable!(),
    };
    let result = run_source_admitted_probe(
        &state.transcode,
        &admission,
        &cancel,
        shutdown,
        progress.as_ref(),
        &fence,
        || async {
            source_version.is_some()
                && crate::fragment_index_cluster::inspect_source(&file)
                    .await
                    .ok()
                    == source_version
                && probe::pipeline_digest().await.as_ref() == Some(probe_digest)
        },
        async {
            let input =
                crate::source_input::SourceInput::open_background(&file, fence.source_budget())
                    .await
                    .map_err(|reason| ProbeError::Transient {
                        path: file.path.display().to_string(),
                        reason,
                    })?;
            let result = plurx_core::process::source_lifetime::mediated(
                probe::probe_single_threaded(std::path::Path::new(input.url())),
            )
            .await;
            input
                .close_and_wait()
                .await
                .map_err(|error| ProbeError::Transient {
                    path: file.path.display().to_string(),
                    reason: format!("joining probe source input: {error}"),
                })?;
            let mut result = result?;
            if source_version.is_none()
                || crate::fragment_index_cluster::inspect_source(&file)
                    .await
                    .ok()
                    != source_version
            {
                return Err(ProbeError::Transient {
                    path: file.path.display().to_string(),
                    reason: "source changed during admitted probe".into(),
                });
            }
            // The private capability URL deliberately has no media extension.
            // Preserve the catalog container classification from the real path.
            if let Some(raw) = result.raw_json.as_deref() {
                if let Ok(mut document) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(format) = document
                        .get_mut("format")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        format.insert(
                            "filename".into(),
                            serde_json::Value::String(file.path.to_string_lossy().into_owned()),
                        );
                    }
                    result.raw_json = serde_json::to_string(&document).ok();
                }
            }
            result.container = file
                .path
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_lowercase);
            Ok(result)
        },
    )
    .await;
    // Bounded process cancellation kills and reaps before returning above.
    let mut published = false;
    let settlement = if cancel.is_cancelled() || shutdown.is_cancelled() {
        Some(JobSettlement::Yield {
            error_code: Some("worker_interrupted".into()),
            not_before_ms: now_ms().saturating_add(5000),
            checkpoint: None,
        })
    } else if snapshot(&file.path).await != Some((source_size, source_mtime)) {
        Some(JobSettlement::Stop {
            error_code: "probe_source_changed".into(),
        })
    } else {
        match result {
            Ok(probe) => {
                let output = ProbeOutput {
                    source: payload,
                    probe,
                };
                if serde_json::to_vec(&output)
                    .map_or(true, |bytes| bytes.len() > MAX_PROBE_RESULT_BYTES)
                {
                    Some(JobSettlement::Stop {
                        error_code: "probe_result_too_large".into(),
                    })
                } else {
                    if let Some(progress) = &progress {
                        progress.stage("publishing");
                    }
                    match fence.publish_probe(output).await {
                        Ok(true) => {
                            published = true;
                            None
                        }
                        Ok(false) => Some(JobSettlement::Stop {
                            error_code: "probe_coordinator_changed".into(),
                        }),
                        Err(error) => {
                            tracing::warn!(%error,"probe publication unavailable");
                            Some(JobSettlement::Retry {
                                error_code: "probe_publish_failed".into(),
                                not_before_ms: now_ms().saturating_add(30_000),
                            })
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(job=job.id,%error,"leaf probe failed");
                Some(JobSettlement::Retry {
                    error_code: "probe_failed".into(),
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
            tracing::warn!(%error,"probe settlement unavailable");
        }
    }
    active.finish().await;
    published
}

/// Cancellation is observed while awaiting the child, never by dropping its
/// collector. bounded::cancellable returns only after the process is reaped.
#[cfg(any(not(target_os = "linux"), test))]
async fn run_admitted_probe<F, T, E>(
    transcode: &crate::transcode::TranscodeManager,
    admission: &crate::transcode::FragmentAdmission,
    cancel: &CancellationToken,
    shutdown: &CancellationToken,
    progress: Option<&crate::state::AnalysisProgressGuard>,
    work: F,
) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
{
    if shutdown.is_cancelled() || !transcode.fragment_worker_idle(admission) {
        cancel.cancel();
    }
    let work = plurx_core::process::bounded::cancellable(cancel.clone(), work);
    tokio::pin!(work);
    loop {
        tokio::select! {
            result = &mut work => return result,
            () = tokio::time::sleep(Duration::from_millis(250)) => {
                if shutdown.is_cancelled() || !transcode.fragment_worker_idle(admission) { cancel.cancel(); }
                if let Some(progress) = progress { progress.stage("probing"); }
            }
        }
    }
}

/// Source pressure suspends the exact owned scan; CPU/GPU preemption keeps
/// the ordinary cancel-and-reap policy. The ledger, never the notification,
/// authorizes slot release/reacquisition under the existing job fence.
#[expect(
    clippy::too_many_arguments,
    reason = "owned scan retains both physical and durable controls plus source/tool identity validation"
)]
pub(crate) async fn run_source_admitted_probe<F, T, E, V, VF>(
    transcode: &crate::transcode::TranscodeManager,
    admission: &crate::transcode::FragmentAdmission,
    cancel: &CancellationToken,
    shutdown: &CancellationToken,
    progress: Option<&crate::state::AnalysisProgressGuard>,
    fence: &crate::background_jobs::JobFence,
    validate_resume: V,
    work: F,
) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
    V: Fn() -> VF,
    VF: std::future::Future<Output = bool>,
{
    let source_budget = fence.source_budget();
    let work = crate::background_jobs::budgeted(Arc::clone(&source_budget), work);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (fence, validate_resume);
        run_admitted_probe(transcode, admission, cancel, shutdown, progress, work).await
    }
    #[cfg(target_os = "linux")]
    {
        use plurx_core::process::bounded::{SourcePauseController, SourcePausePhase};
        let control = SourcePauseController::new_slot_gated(Duration::from_secs(18 * 60 * 60));
        let _pause_binding = source_budget.bind_pause_controller(control.clone());
        // Close the claim-to-first-read race before polling the producer. An
        // empty scope can acknowledge source quiescence without spawning.
        if !fence.source_may_continue().await.unwrap_or(false) {
            fence.source_budget().invalidate_grant();
            control.request_pause();
        }
        let work = plurx_core::process::bounded::pausable(
            control.clone(),
            plurx_core::process::bounded::cancellable(cancel.clone(), work),
        );
        tokio::pin!(work);
        let mut source_paused = false;
        let mut pacing_hold = false;
        let mut finite_hold_only = false;
        let mut finite_hold_pressure_revision = None;
        loop {
            if shutdown.is_cancelled()
                || !transcode.fragment_worker_idle(admission)
                || control.pause_budget_exhausted()
            {
                cancel.cancel();
            }
            let observation = control.observation();
            if observation.phase == SourcePausePhase::Paused
                && !source_paused
                && !cancel.is_cancelled()
            {
                match fence.pause_source().await {
                    Ok(true) => {
                        source_paused = true;
                        if finite_hold_pressure_revision
                            != Some(crate::source_custody::source_pressure_changed().revision())
                        {
                            // A foreground change may have occurred while
                            // this owner awaited the pause Store RPC.
                            finite_hold_only = false;
                        }
                        if finite_hold_only && control.quota_waiters() != 0 {
                            // Physical stop was proved before the RPC. Only
                            // its acknowledgement plus a live finite wait
                            // may begin active-time credit.
                            if !control.confirm_pause_slot(observation.revision) {
                                cancel.cancel();
                            }
                        }
                        tracing::debug!(
                            pause_revision = observation.revision,
                            source_pacing_hold = pacing_hold,
                            "owned source scan paused with its claim retained"
                        );
                    }
                    _ => cancel.cancel(),
                }
            }
            if source_paused
                && source_budget.pacing_waiters() != 0
                && control.quota_waiters() != 0
                && !cancel.is_cancelled()
            {
                let pressure = crate::source_custody::source_pressure_changed();
                if finite_hold_only && finite_hold_pressure_revision == Some(pressure.revision()) {
                    control.confirm_pause_slot(observation.revision);
                } else {
                    // A global projection notification may belong to another
                    // domain. Stop credit before the Store query; the same
                    // fence and a stable notification revision can restore
                    // this finite hold while the process remains stopped.
                    match revalidate_finite_source_hold(
                        &control,
                        observation.revision,
                        || source_budget.pacing_waiters() != 0,
                        || pressure.revision(),
                        || async { fence.source_may_continue().await.unwrap_or(false) },
                    )
                    .await
                    {
                        Ok(revision) => {
                            finite_hold_pressure_revision = revision;
                            finite_hold_only = revision.is_some();
                        }
                        Err(()) => cancel.cancel(),
                    }
                }
            }
            if source_paused && source_budget.pacing_waiters() == 0 && !cancel.is_cancelled() {
                // Validation and slot reacquisition are store/authority work,
                // never a finite service hold. The child remains stopped.
                control.suspend_pause_credit(observation.revision);
                if !validate_resume().await {
                    cancel.cancel();
                } else if fence.resume_source().await.unwrap_or(false) {
                    fence.source_budget().invalidate_grant();
                    if !control.resume(observation.revision) {
                        cancel.cancel();
                    } else {
                        source_paused = false;
                        finite_hold_only = false;
                        finite_hold_pressure_revision = None;
                        tracing::debug!(
                            pause_ms = observation.paused_for.as_millis(),
                            source_pacing_hold = pacing_hold,
                            timing_class = if pacing_hold {
                                "source_paced"
                            } else {
                                "source_pressure"
                            },
                            "owned source scan resumed after source-slot reacquisition"
                        );
                        pacing_hold = false;
                    }
                }
            }
            tokio::select! {
                result = &mut work => {
                    // A completed/no-child stage cannot let the next stage
                    // access source while this same claim's slot is paused.
                    while source_paused && !cancel.is_cancelled() {
                        control.suspend_pause_credit(control.observation().revision);
                        if shutdown.is_cancelled() || !transcode.fragment_worker_idle(admission)
                            || control.pause_budget_exhausted() || !validate_resume().await { cancel.cancel(); break; }
                        if fence.resume_source().await.unwrap_or(false) {
                            control.resume(control.observation().revision);
                            break;
                        }
                        tokio::select! {
                            ()=cancel.cancelled()=>{},
                            ()=crate::source_custody::source_pressure_changed().notified()=>{},
                            ()=tokio::time::sleep(Duration::from_millis(250))=>{},
                        }
                    }
                    return result;
                },
                ()=crate::source_custody::source_pressure_changed().notified()=>{
                    if source_paused {
                        // This notification is not finite-byte evidence.
                        // Recheck the same fence on the next owner iteration.
                        finite_hold_only = false;
                        finite_hold_pressure_revision = None;
                        control.suspend_pause_credit(control.observation().revision);
                    }
                },
                ()=tokio::time::sleep(Duration::from_millis(250))=>{},
            }
            if !cancel.is_cancelled() {
                let paced = source_budget.pacing_waiters() != 0;
                if !source_paused {
                    let pressure_revision =
                        crate::source_custody::source_pressure_changed().revision();
                    let pressure = !fence.source_may_continue().await.unwrap_or(false)
                        || crate::source_custody::source_pressure_changed().revision()
                            != pressure_revision;
                    if pressure || paced {
                        pacing_hold |= paced;
                        if control.observation().phase == SourcePausePhase::Running {
                            finite_hold_only = paced && !pressure;
                            finite_hold_pressure_revision =
                                finite_hold_only.then_some(pressure_revision);
                        } else if pressure {
                            finite_hold_only = false;
                            finite_hold_pressure_revision = None;
                        }
                        if pressure && control.observation().phase == SourcePausePhase::Running {
                            source_budget.invalidate_grant();
                        }
                        control.request_pause();
                    }
                }
            }
            if let Some(progress) = progress {
                progress.stage("probing");
            }
        }
    }
}

/// The owner has already received its pause RPC acknowledgement. Recheck the
/// same fence before restoring finite-hold credit after a pressure signal.
/// The authority RPC and any uncertain interval remain active execution time.
#[cfg(target_os = "linux")]
async fn revalidate_finite_source_hold<Live, Revision, Check, CheckFuture>(
    control: &plurx_core::process::bounded::SourcePauseController,
    pause_revision: u64,
    finite_wait_live: Live,
    pressure_revision: Revision,
    may_continue: Check,
) -> Result<Option<u64>, ()>
where
    Live: Fn() -> bool,
    Revision: Fn() -> u64,
    Check: FnOnce() -> CheckFuture,
    CheckFuture: std::future::Future<Output = bool>,
{
    use plurx_core::process::bounded::SourcePausePhase;
    control.suspend_pause_credit(pause_revision);
    let before = pressure_revision();
    let permitted = may_continue().await;
    let observation = control.observation();
    if !permitted
        || pressure_revision() != before
        || !finite_wait_live()
        || control.quota_waiters() == 0
        || observation.revision != pause_revision
        || observation.phase != SourcePausePhase::Paused
    {
        return Ok(None);
    }
    if !control.confirm_pause_slot(pause_revision) {
        return Err(());
    }
    if pressure_revision() != before || !finite_wait_live() || control.quota_waiters() == 0 {
        control.suspend_pause_credit(pause_revision);
        return Ok(None);
    }
    Ok(Some(before))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::store::{SqliteStore, Store};
    use plurx_core::transcode::{EncoderCaps, Pipeline};

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn unrelated_pressure_signal_rechecks_finite_hold_and_charges_rpc_time() {
        use plurx_core::process::bounded::SourcePauseController;
        use std::sync::atomic::{AtomicU64, Ordering};

        let control = SourcePauseController::new_slot_gated(Duration::from_secs(5));
        let quota_wait = control.finite_quota_wait();
        let revision = control.request_pause();
        assert!(control.confirm_pause_slot(revision));

        let signal = Arc::new(AtomicU64::new(0));
        signal.fetch_add(1, Ordering::AcqRel); // unrelated domain projection
        let signal_for_query = Arc::clone(&signal);
        let control_for_query = control.clone();
        let (query_started, query_started_rx) = tokio::sync::oneshot::channel();
        let query_release = Arc::new(tokio::sync::Notify::new());
        let release_for_query = Arc::clone(&query_release);
        let recheck = tokio::spawn(async move {
            revalidate_finite_source_hold(
                &control_for_query,
                revision,
                || true,
                || signal_for_query.load(Ordering::Acquire),
                move || async move {
                    query_started.send(()).expect("query start observed");
                    release_for_query.notified().await;
                    true
                },
            )
            .await
        });
        query_started_rx.await.expect("authority query started");
        // The group remains physically stopped, yet the Store wait spends
        // active time until the same fence has been checked again.
        tokio::time::timeout(
            Duration::from_millis(250),
            control.wait_active_budget(Duration::from_millis(20)),
        )
        .await
        .expect("uncredited RPC wait spends active budget");

        query_release.notify_one();
        assert_eq!(recheck.await.expect("recheck task"), Ok(Some(1)));
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                control.wait_active_budget(Duration::from_millis(20)),
            )
            .await
            .is_err(),
            "same-fence finite hold restores active-time credit"
        );
        drop(quota_wait);
        tokio::time::timeout(
            Duration::from_millis(250),
            control.wait_active_budget(Duration::from_millis(20)),
        )
        .await
        .expect("closed quota guard spends active budget");
    }

    #[tokio::test]
    async fn probe_capacity_refusal_preserves_a_nonfinal_candidate_cursor() {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
        use plurx_core::store::background_jobs::{EnqueueJob, JobRequest};
        use plurx_core::store::background_jobs_probe::{generation, ProbeCoordinator};
        let dir = crate::test_tempdir().expect("scratch");
        let root = dir.path();
        let state = AppState::new(
            "test".into(),
            Arc::new(SqliteStore::open_in_memory().expect("store")),
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
                runtime_cache: root.join("runtime"),
                renditions: root.join("renditions"),
            },
            "test-node".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(16)),
        );
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "probes".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        let item = state
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "candidate".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file = state
            .store
            .upsert_file(
                item,
                &root.join("movie.mkv").to_string_lossy(),
                100,
                1,
                &Default::default(),
            )
            .await
            .expect("file");
        let now = now_ms();
        let plurx_core::cluster::coordination::LeaseClaim::Acquired(lease) = state
            .store
            .acquire_lease("repair:probe", "test-node", now, now + 60_000)
            .await
            .expect("coordinator")
        else {
            panic!("lease");
        };
        let coordinator = ProbeCoordinator::from_lease(&lease);
        let plurx_core::cluster::coordination::LeaseClaim::Acquired(scan_lease) = state
            .store
            .acquire_lease(
                &format!("scan:library:{}", library.id),
                "test-node",
                now,
                now + 60_000,
            )
            .await
            .expect("scan coordinator")
        else {
            panic!("lease");
        };
        let scan_coordinator = ProbeCoordinator::from_lease(&scan_lease);
        let digest = "a".repeat(64);
        for index in 0..200 {
            let coordinator = if index < 100 {
                &coordinator
            } else {
                &scan_coordinator
            };
            let id = uuid::Uuid::new_v4().to_string();
            let outcome = state
                .store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: JobPayload::MediaProbe {
                        file_id: file,
                        source_generation: generation(file, 100, 1, coordinator, &digest),
                        source_size: 100,
                        source_mtime: 1,
                        probe_digest: digest.clone(),
                        coordinator: coordinator.clone(),
                    },
                    dedupe_key: format!("candidate:{index}"),
                    priority: 1,
                    not_before_ms: now,
                    now_ms: now,
                    request: JobRequest {
                        scope: "candidate".into(),
                        request_id: id.clone(),
                        request_digest: "b".repeat(64),
                        consumer_kind: "probe".into(),
                        consumer_ref: id,
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("enqueue");
            assert!(matches!(
                outcome,
                plurx_core::store::background_jobs::EnqueueOutcome::Accepted { .. }
            ));
        }
        let query = CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![JobKind::MediaProbe],
            after: None,
            now_ms: now_ms(),
            limit: 32,
        };
        let first = state
            .store
            .job_candidates(query.clone())
            .await
            .expect("first page");
        let mut cursor = first.next;
        assert!(cursor.is_some());
        let next_query = CandidateQuery {
            after: cursor.clone(),
            limit: 128,
            ..query
        };
        let expected = state
            .store
            .job_candidates(next_query.clone())
            .await
            .expect("second page");
        assert!(expected.next.is_some(), "tested page is nonfinal");
        let before = serde_json::to_value(&cursor).expect("cursor");
        let admission = state.transcode.admit_fragment().await.expect("busy slot");
        assert!(
            !pass(&state, "boot", &mut cursor, &CancellationToken::new())
                .await
                .expect("refused")
        );
        assert_eq!(serde_json::to_value(&cursor).expect("cursor"), before);
        assert!(state
            .store
            .job_attempts(&expected.jobs[0].id)
            .await
            .expect("attempts")
            .is_empty());
        drop(admission);
        let resumed = state
            .store
            .job_candidates(CandidateQuery {
                after: cursor,
                ..next_query
            })
            .await
            .expect("resume");
        assert_eq!(resumed.jobs[0].id, expected.jobs[0].id);
        assert!(state.transcode.admit_fragment().await.is_some());
    }

    #[tokio::test]
    async fn probe_batch_keeps_capacity_until_sibling_reaped_and_yields_to_playback() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        store
            .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "2")
            .await
            .expect("budget");
        let dir = crate::test_tempdir().expect("scratch");
        let manager = Arc::new(crate::transcode::TranscodeManager::new(
            store,
            dir.path().into(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let admission = Arc::new(manager.admit_fragment().await.expect("batch"));
        assert_eq!(admission.probe_parallelism(), 2);
        let mut tasks = Vec::new();
        let mut cancellations = Vec::new();
        let mut markers = Vec::new();
        for index in 0..2 {
            let marker = dir.path().join(format!("probe-{index}.pid"));
            markers.push(marker.clone());
            let cancel = CancellationToken::new();
            cancellations.push(cancel.clone());
            let manager = Arc::clone(&manager);
            let admission = Arc::clone(&admission);
            tasks.push(tokio::spawn(async move {
                let work = async {
                    plurx_core::process::bounded::output(
                        "/bin/sh",
                        &[
                            "-c".into(),
                            "echo $$ > \"$1\"; exec sleep 60".into(),
                            "probe".into(),
                            marker.into_os_string(),
                        ],
                        Duration::from_secs(65),
                        1024,
                        plurx_core::process::ChildWork::background("probe batch regression"),
                    )
                    .await
                    .map_err(|error| plurx_core::error::ProbeError::Spawn(error.to_string()))?;
                    Ok::<plurx_core::domain::ProbeResult, plurx_core::error::ProbeError>(
                        Default::default(),
                    )
                };
                run_admitted_probe(
                    &manager,
                    &admission,
                    &cancel,
                    &CancellationToken::new(),
                    None,
                    work,
                )
                .await
            }));
        }
        drop(admission);
        tokio::time::timeout(Duration::from_secs(5), async {
            while markers.iter().any(|marker| !marker.exists()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both children overlap");
        assert!(
            manager.admit_fragment().await.is_none(),
            "no third heavy worker"
        );
        cancellations[0].cancel(); // Same signal propagated by a lost durable lease.
        let first = tasks.remove(0);
        assert!(tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .expect("first reaped")
            .expect("task")
            .is_err());
        assert!(
            manager.background_worker_in_use(),
            "sibling still owns physical capacity"
        );
        assert!(manager.admit_fragment().await.is_none());
        let playback = manager.test_mark_live_waiting();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), tasks.remove(0))
                .await
                .expect("playback reaps sibling")
                .expect("task")
                .is_err()
        );
        assert_eq!(manager.test_software_threads_in_use(), 0);
        for marker in markers {
            let pid = std::fs::read_to_string(marker).expect("pid");
            #[allow(clippy::disallowed_methods)]
            let status = std::process::Command::new("/bin/kill")
                .args(["-0", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("liveness check");
            assert!(!status.success(), "child was physically reaped");
        }
        assert!(
            manager.admit_fragment().await.is_none(),
            "playback still has priority"
        );
        drop(playback);
        assert!(manager.admit_fragment().await.is_some());
    }
}
