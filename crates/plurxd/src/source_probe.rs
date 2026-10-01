//! Admitted leaf workers never mutate catalogue identity or scan completion.
use crate::state::AppState;
use plurx_core::store::background_jobs_probe::{ProbeOutput, MAX_PROBE_RESULT_BYTES};
use plurx_core::{
    error::StoreError,
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
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        let published = match pass(&state, &boot, &mut cursor, &shutdown).await {
            Ok(published) => published,
            Err(error) => {
                tracing::warn!(%error, "leaf probe pass failed");
                false
            }
        };
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
        ) {
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
    let result = run_admitted_probe(
        &state.transcode,
        &admission,
        &cancel,
        shutdown,
        progress.as_ref(),
        probe::probe_single_threaded(&file.path),
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
async fn run_admitted_probe<F>(
    transcode: &crate::transcode::TranscodeManager,
    admission: &crate::transcode::FragmentAdmission,
    cancel: &CancellationToken,
    shutdown: &CancellationToken,
    progress: Option<&crate::state::AnalysisProgressGuard>,
    work: F,
) -> Result<plurx_core::domain::ProbeResult, plurx_core::error::ProbeError>
where
    F: std::future::Future<
        Output = Result<plurx_core::domain::ProbeResult, plurx_core::error::ProbeError>,
    >,
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

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::store::{SqliteStore, Store};
    use plurx_core::transcode::{EncoderCaps, Pipeline};

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
                    Ok(Default::default())
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
