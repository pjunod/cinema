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
        let progressed = match pass(&state, &boot, &mut cursor, &shutdown).await {
            Ok(progressed) => progressed,
            Err(error) => {
                tracing::warn!(%error,"leaf probe pass failed");
                false
            }
        };
        if progressed {
            continue;
        }
        tokio::select! { () = shutdown.cancelled() => return, () = tokio::time::sleep(idle.delay(false)) => {} }
    }
}
async fn pass(
    state: &AppState,
    boot: &str,
    cursor: &mut Option<CandidateCursor>,
    shutdown: &CancellationToken,
) -> Result<bool, StoreError> {
    let authority = state.jobs.execution_authority();
    if !authority.may_execute_job(JobKind::MediaProbe).await {
        return Ok(false);
    }
    let Some(admission) = state.transcode.admit_fragment().await else {
        return Ok(false);
    };
    let Some(pipeline) = probe::pipeline_digest().await else {
        return Ok(false);
    };
    let page = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![JobKind::MediaProbe],
            after: cursor.take(),
            now_ms: now_ms(),
            limit: 128,
        })
        .await?;
    *cursor = page.next;
    for candidate in page.jobs {
        let payload = match candidate.supported_payload() {
            Ok(payload) => payload,
            Err(_) => continue,
        };
        let JobPayload::MediaProbe {
            file_id,
            source_size,
            source_mtime,
            ref probe_digest,
            ..
        } = payload
        else {
            continue;
        };
        if probe_digest != &pipeline {
            continue;
        }
        let Some(file) = state.store.get_file(file_id).await? else {
            continue;
        };
        if file.size != source_size
            || file.mtime != source_mtime
            || snapshot(&file.path).await != Some((source_size, source_mtime))
        {
            continue;
        }
        if shutdown.is_cancelled() || !state.transcode.fragment_worker_idle(&admission) {
            return Ok(false);
        }
        let now = now_ms();
        let Some((job, deadline)) = crate::background_jobs::claim_with_resolution(
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
        .await?
        else {
            continue;
        };
        let active = crate::background_jobs::ActiveBackgroundJob::start(
            Arc::clone(&state.store),
            authority,
            job.token
                .clone()
                .ok_or_else(|| StoreError::Task("probe claim missing token".into()))?,
            deadline,
            JobKind::MediaProbe,
        )?;
        let fence = active.fence();
        let cancel = fence.loss_token().child_token();
        let result = {
            let work =
                plurx_core::process::bounded::cancellable(cancel.clone(), probe::probe(&file.path));
            tokio::pin!(work);
            loop {
                tokio::select! {
                    result = &mut work => break result,
                    () = tokio::time::sleep(Duration::from_millis(250)) => {
                        if shutdown.is_cancelled() || !state.transcode.fragment_worker_idle(&admission) { cancel.cancel(); }
                    }
                }
            }
        };
        // Bounded process cancellation kills and reaps before returning above.
        let settlement = if cancel.is_cancelled() || shutdown.is_cancelled() {
            Some(JobSettlement::Yield {
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
                        match fence.publish_probe(output).await {
                            Ok(true) => None,
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
                        not_before_ms: now_ms().saturating_add(
                            crate::background_jobs::retry_delay_ms(&job.id, job.failed_attempts),
                        ),
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
        return Ok(true);
    }
    Ok(false)
}
