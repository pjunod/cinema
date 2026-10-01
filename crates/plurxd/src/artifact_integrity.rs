//! Node-local integrity reads are durable, admitted, bounded and generation-fenced.
use crate::state::AppState;
use plurx_core::{
    error::StoreError,
    store::{
        background_jobs::{
            CandidateQuery, ClaimJob, EnqueueJob, JobKind, JobPayload, JobRequest, JobSettlement,
        },
        background_jobs_integrity::TranscodeVerificationCandidate,
        Store,
    },
    transcode::manifest,
};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub(crate) const SCRUB_BYTES: u64 = manifest::MAX_OBJECT_BYTES + manifest::MAX_MANIFEST_BYTES + 2;
pub(crate) fn reserve_scrub_bytes(remaining: &mut u64, bytes: u64) -> bool {
    if bytes > *remaining {
        return false;
    }
    *remaining -= bytes;
    true
}
const MAX_OBJECTS: usize = 4096;
const MAX_WALL: Duration = Duration::from_secs(60);
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub(crate) struct Inspection {
    pub valid: bool,
    pub next_object_index: i64,
    pub bytes: u64,
}
/// None means preempted, never corruption. Reads finish before the caller releases
/// its cache reader or physical admission; a cancelled read is not detached.
pub(crate) async fn inspect(
    root: &Path,
    candidate: &TranscodeVerificationCandidate,
    can_continue: impl Fn() -> bool,
) -> Option<Inspection> {
    let started = Instant::now();
    let allowed = || can_continue() && started.elapsed() < MAX_WALL;
    if !allowed() {
        return None;
    }
    let invalid = |bytes| {
        Some(Inspection {
            valid: false,
            next_object_index: candidate.scrub_object_index,
            bytes,
        })
    };
    let Some(directory) =
        crate::cachekeep::validated_entry_dir(root, &candidate.relative_dir).await
    else {
        return if allowed() { invalid(0) } else { None };
    };
    let loaded = manifest::load_with_budget(&directory, SCRUB_BYTES).await;
    if !allowed() {
        return None;
    }
    let (manifest, charged) = match loaded {
        Ok(Some(value)) => value,
        Ok(None) => return None,
        Err(_) => return invalid(manifest::MAX_MANIFEST_BYTES + 1),
    };
    if manifest.manifest_digest != candidate.manifest_digest || manifest.objects.is_empty() {
        return invalid(charged);
    }
    let mut remaining = SCRUB_BYTES - charged;
    let start = candidate.scrub_object_index.max(0) as usize % manifest.objects.len();
    let mut checked = 0;
    for object in manifest
        .objects
        .iter()
        .cycle()
        .skip(start)
        .take(manifest.objects.len().min(MAX_OBJECTS))
    {
        if !allowed() {
            break;
        }
        let reserved = object.bytes.saturating_add(1);
        if !reserve_scrub_bytes(&mut remaining, reserved) {
            break;
        }
        let verified = manifest
            .verify_object_cooperative(&directory, &object.name, &allowed)
            .await;
        if !allowed() {
            // An interrupted object's false result is never evidence of corruption.
            break;
        }
        if !matches!(verified, Ok(true)) {
            return invalid(SCRUB_BYTES - remaining);
        }
        checked += 1;
    }
    if !can_continue() || checked == 0 {
        return None;
    }
    Some(Inspection {
        valid: true,
        next_object_index: ((start + checked) % manifest.objects.len()) as i64,
        bytes: SCRUB_BYTES - remaining,
    })
}

async fn enqueue(
    store: &Arc<dyn Store>,
    node: &str,
    candidate: &TranscodeVerificationCandidate,
    now: i64,
) -> Result<(), StoreError> {
    let key = plurx_core::store::background_jobs_transcode::artifact_key(
        &candidate.recipe_hash,
        &candidate.manifest_digest,
    );
    enqueue_artifact(
        store,
        node,
        key,
        &format!(
            "{}:{}:{}",
            candidate.relative_dir, candidate.publication_generation, candidate.scrub_object_index
        ),
        now,
    )
    .await
}
pub(crate) async fn enqueue_artifact(
    store: &Arc<dyn Store>,
    node: &str,
    key: String,
    generation: &str,
    now: i64,
) -> Result<(), StoreError> {
    let payload = JobPayload::ArtifactVerify {
        artifact_key: key.clone(),
        target_node_id: node.into(),
    };
    let digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&payload).map_err(|error| StoreError::Task(error.to_string()))?,
    ));
    // One request per holder/cursor/hour. A failed job cannot be recreated on
    // every scheduler tick; explicit repair uses a separate finite state machine.
    let request = hex::encode(Sha256::digest(format!(
        "{digest}:{generation}:{}",
        now / 3_600_000
    )));
    store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            dedupe_key: format!("verify:{digest}"),
            priority: 0,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "artifact-verify".into(),
                request_id: request,
                request_digest: digest,
                consumer_kind: "artifact_verify".into(),
                consumer_ref: key,
                target_node_id: None,
                deadline_ms: Some(now.saturating_add(300_000)),
                retain_identity: false,
            },
        })
        .await?;
    Ok(())
}
pub(crate) async fn run(state: AppState, shutdown: CancellationToken) {
    let boot = uuid::Uuid::new_v4().to_string();
    let mut next_discovery = Instant::now();
    let mut cursor = None;
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        let result = async {
            let authority = state.jobs.execution_authority();
            if !authority.may_execute_job(JobKind::ArtifactVerify).await {
                return Ok(false);
            }
            let Some((root, node)) = state.transcode.cache_location() else {
                return Ok(false);
            };
            if Instant::now() >= next_discovery {
                next_discovery = Instant::now() + Duration::from_secs(60);
                for candidate in state
                    .store
                    .transcode_verification_candidates(node, None)
                    .await?
                    .into_iter()
                    .take(8)
                {
                    enqueue(&state.store, node, &candidate, now_ms()).await?;
                }
            }
            let Some(admission) = state.transcode.admit_fragment().await else {
                return Ok(false);
            };
            let page = state
                .store
                .job_candidates(CandidateQuery {
                    node_id: node.into(),
                    kinds: vec![JobKind::ArtifactVerify],
                    after: cursor.take(),
                    now_ms: now_ms(),
                    limit: 128,
                })
                .await?;
            cursor = page.next;
            for candidate in page.jobs {
                let Ok(JobPayload::ArtifactVerify { artifact_key, .. }) =
                    candidate.supported_payload()
                else {
                    continue;
                };
                if !artifact_key.starts_with("transcode:") {
                    continue;
                }
                let location = state
                    .store
                    .transcode_verification_candidates(node, Some(&artifact_key))
                    .await?
                    .into_iter()
                    .next();
                let readers = state.transcode.cache_readers();
                if location
                    .as_ref()
                    .is_some_and(|location| readers.has_reader(&location.recipe_hash))
                {
                    continue;
                }
                let reader = location
                    .as_ref()
                    .and_then(|location| readers.begin_read(&location.recipe_hash));
                if location.is_some() && reader.is_none() {
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
                        node_id: node.into(),
                        boot_id: boot.clone(),
                        claim_id: uuid::Uuid::new_v4().to_string(),
                        kind: JobKind::ArtifactVerify,
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
                    job.token.clone().ok_or_else(|| {
                        StoreError::Task("verification claim missing token".into())
                    })?,
                    deadline,
                    JobKind::ArtifactVerify,
                )?;
                let fence = active.fence();
                let settlement = if let Some(location) = location {
                    let loss = fence.loss_token();
                    let allowed = || {
                        !shutdown.is_cancelled()
                            && !loss.is_cancelled()
                            && state.transcode.fragment_worker_idle(&admission)
                            && !readers.has_readers_besides(&location.recipe_hash)
                    };
                    match inspect(root, &location, allowed).await {
                        None => Some(JobSettlement::Yield {
                            error_code: Some("worker_interrupted".into()),
                            not_before_ms: now_ms().saturating_add(5000),
                            checkpoint: None,
                        }),
                        Some(inspection) => {
                            tracing::debug!(
                                bytes = inspection.bytes,
                                valid = inspection.valid,
                                "artifact verification completed"
                            );
                            match fence
                                .verify_transcode(
                                    location,
                                    inspection.valid,
                                    inspection.next_object_index,
                                )
                                .await
                            {
                                Ok(true) => None,
                                Ok(false) => Some(JobSettlement::Stop {
                                    error_code: "verification_generation_changed".into(),
                                }),
                                Err(error) => {
                                    tracing::warn!(%error, "verification publication unavailable");
                                    Some(JobSettlement::Retry {
                                        error_code: "verification_publish_failed".into(),
                                        not_before_ms: now_ms().saturating_add(30_000),
                                    })
                                }
                            }
                        }
                    }
                } else {
                    Some(JobSettlement::Stop {
                        error_code: "verification_location_retired".into(),
                    })
                };
                if let Some(settlement) = settlement {
                    fence.settle(settlement).await?;
                }
                active.finish().await;
                drop(reader);
                return Ok::<_, StoreError>(true);
            }
            Ok(false)
        }
        .await;
        match result {
            Ok(true) => continue,
            Err(error) => tracing::warn!(%error,"artifact verification pass failed"),
            Ok(false) => {}
        }
        tokio::select! { () = shutdown.cancelled() => return, () = tokio::time::sleep(Duration::from_secs(5)) => {} }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FixtureInspection {
    pub corrupt: usize,
    pub scrub_bytes: u64,
}
#[cfg(test)]
pub(crate) async fn fixture_verify_and_sweep(
    store: &Arc<dyn Store>,
    root: &Path,
    node: &str,
    readers: &crate::cachekeep::ActiveCacheReaders,
    now: i64,
) -> FixtureInspection {
    use plurx_core::store::{
        background_jobs::{ClaimOutcome, JobPublishOutcome},
        background_jobs_integrity::VerifyTranscode,
    };
    let mut result = FixtureInspection::default();
    for location in store
        .transcode_verification_candidates(node, None)
        .await
        .expect("verification locations")
    {
        if readers.has_reader(&location.recipe_hash) {
            continue;
        }
        let reader = readers
            .begin_read(&location.recipe_hash)
            .expect("verification reader");
        let id = uuid::Uuid::new_v4().to_string();
        let clock = now * 1000;
        store
            .enqueue_job(EnqueueJob {
                id: id.clone(),
                payload: JobPayload::ArtifactVerify {
                    artifact_key: plurx_core::store::background_jobs_transcode::artifact_key(
                        &location.recipe_hash,
                        &location.manifest_digest,
                    ),
                    target_node_id: node.into(),
                },
                dedupe_key: id.clone(),
                priority: 0,
                not_before_ms: clock,
                now_ms: clock,
                request: JobRequest {
                    scope: "fixture-integrity".into(),
                    request_id: id.clone(),
                    request_digest: "a".repeat(64),
                    consumer_kind: "artifact_verify".into(),
                    consumer_ref: id.clone(),
                    target_node_id: None,
                    deadline_ms: Some(clock + 300_000),
                    retain_identity: false,
                },
            })
            .await
            .expect("enqueue verification");
        let job = store
            .background_job(&id)
            .await
            .expect("lookup")
            .expect("verification job");
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: id,
                expected_revision: job.revision,
                node_id: node.into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::ArtifactVerify,
                payload_version: 1,
                now_ms: clock,
                dispatched_at_ms: clock,
            })
            .await
            .expect("claim verification")
        else {
            panic!("verification not claimed")
        };
        let inspection = inspect(root, &location, || {
            !readers.has_readers_besides(&location.recipe_hash)
        })
        .await
        .expect("inspection");
        assert!(matches!(
            store
                .verify_transcode_job(VerifyTranscode {
                    token: job.token.expect("token"),
                    recipe_hash: location.recipe_hash,
                    manifest_digest: location.manifest_digest,
                    relative_dir: location.relative_dir,
                    publication_generation: location.publication_generation,
                    valid: inspection.valid,
                    next_object_index: inspection.next_object_index,
                    now_ms: clock,
                })
                .await
                .expect("publish verification"),
            JobPublishOutcome::Published { .. }
        ));
        result.corrupt += usize::from(!inspection.valid);
        result.scrub_bytes += inspection.bytes;
        drop(reader);
    }
    crate::cachekeep::sweep_with_readers(store, root, node, readers, now).await;
    result
}
