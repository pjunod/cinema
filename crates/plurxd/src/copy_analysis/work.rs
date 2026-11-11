//! The Analysis consumer of committed copy-range observations.
//!
//! Playback and Source copies send [`CompletedCopyRange`] notifications after
//! their durable commit. This worker coalesces them per exact source, and once
//! the observed rows cover the whole title it runs the remaining metadata-owned
//! steps under the existing Analysis lease: one fenced full-source attestation
//! phase, seal, local blob installation, and the guarded v3 publication through
//! a publisher job. No step here grants media serving or a Source grant; the
//! only physical reads are the attestation phase's own exact custody.
use super::{physical, CompletedCopyRange, CopyObservationSet};
use plurx_core::store::analysis_coordination::{
    AnalysisCoordinationClaim, AnalysisCoordinationSource,
};
use plurx_core::store::background_jobs::{
    ClaimJob, EnqueueJob, EnqueueOutcome, JobKind, JobPublishOutcome, JobRequest,
    PublishCopyIndexV3,
};
use plurx_core::store::{
    AnalysisRequest, CopyIndexV3Artifact, CopyIndexV3HeadState, FragmentIndexEngineProvenance,
    NewAnalysisRequest, Store,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Sources whose observations are retained in memory between notifications.
/// The durable range manifests remain the record; this is only coalescing, so
/// the least recently observed source is the one forgotten.
const MAX_PENDING_SOURCES: usize = 64;

pub(crate) struct PendingSource {
    file: Arc<plurx_core::domain::MediaFile>,
    set: CopyObservationSet,
    observed: Instant,
    /// Not before this instant: a busy source domain or a contended claim
    /// waits for the next dispatch wake after it, never a private timer.
    retry_after: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct PendingSources {
    sources: HashMap<String, PendingSource>,
}

impl PendingSources {
    /// Fold one committed range into its source's set. A contradiction between
    /// two observations of the same source discards both: neither can seal.
    pub(crate) fn observe(&mut self, message: CompletedCopyRange) {
        let Ok(key) = plurx_core::store::copy_index_v3_source_key(
            &message.catalog_namespace,
            &message.source,
        ) else {
            return;
        };
        if let Some(pending) = self.sources.get_mut(&key) {
            if let Err(error) = pending.set.observe(&message) {
                tracing::warn!(
                    file_id = message.source.file_id,
                    ?error,
                    "copy observations of one source disagree; discarding them"
                );
                self.sources.remove(&key);
            } else {
                pending.observed = Instant::now();
            }
            return;
        }
        match CopyObservationSet::new(&message) {
            Ok(set) => {
                if self.sources.len() >= MAX_PENDING_SOURCES {
                    if let Some(oldest) = self
                        .sources
                        .iter()
                        .min_by_key(|(_, pending)| pending.observed)
                        .map(|(key, _)| key.clone())
                    {
                        self.sources.remove(&oldest);
                    }
                }
                self.sources.insert(
                    key,
                    PendingSource {
                        file: Arc::clone(&message.file),
                        set,
                        observed: Instant::now(),
                        retry_after: None,
                    },
                );
            }
            Err(error) => tracing::debug!(
                file_id = message.source.file_id,
                ?error,
                "copy observation refused"
            ),
        }
    }

    /// Complete sets that may be attempted now.
    fn ready(&self, now: Instant) -> Vec<String> {
        let mut keys = self
            .sources
            .iter()
            .filter(|(_, pending)| {
                pending.set.is_complete() && pending.retry_after.is_none_or(|at| at <= now)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }
}

/// What the Analysis dispatcher hands this worker. Every field already exists
/// on the job manager; nothing here is a new authority.
pub(crate) struct CopyAnalysisContext {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) node_id: String,
    pub(crate) cache_dir: PathBuf,
    pub(crate) checkpoint_root: Option<PathBuf>,
    pub(crate) lease_ms: i64,
    pub(crate) renew_every: Duration,
    pub(crate) retry_ms: i64,
    pub(crate) metrics: Arc<crate::state::AnalysisRuntimeMetrics>,
}

#[derive(Debug)]
enum Attempt {
    /// Published now, or a healthy head already serves this generation.
    Done,
    /// Another owner or a legacy request holds this source; try on a later wake.
    Busy,
    /// The attempt failed; the source set is kept for one later retry.
    Retry(String),
    /// The observations can never seal this source as they are.
    Discard(String),
}

/// One dispatcher pass. Single-flight is the registry's: a pass already
/// running leaves new notifications in the bounded inbox for itself.
pub(crate) async fn work_copy_observations(
    registry: &super::CopyAnalysisRegistry,
    context: &CopyAnalysisContext,
) -> bool {
    let Some(mut pending) = registry.begin_pass() else {
        return false;
    };
    for message in registry.take_pending() {
        pending.observe(message);
    }
    let mut progressed = false;
    for key in pending.ready(Instant::now()) {
        let Some(source) = pending.sources.get(&key) else {
            continue;
        };
        let attempt = publish_source(context, &key, source).await;
        match attempt {
            Attempt::Done => {
                progressed = true;
                pending.sources.remove(&key);
            }
            Attempt::Busy => {
                if let Some(source) = pending.sources.get_mut(&key) {
                    source.retry_after = Some(
                        Instant::now() + Duration::from_millis(context.retry_ms.max(1_000) as u64),
                    );
                }
            }
            Attempt::Retry(reason) => {
                tracing::warn!(%reason, "copy index publication will retry");
                if let Some(source) = pending.sources.get_mut(&key) {
                    source.retry_after = Some(
                        Instant::now() + Duration::from_millis(context.retry_ms.max(1_000) as u64),
                    );
                }
            }
            Attempt::Discard(reason) => {
                tracing::warn!(%reason, "copy observations cannot seal this source");
                pending.sources.remove(&key);
            }
        }
    }
    progressed
}

async fn publish_source(
    context: &CopyAnalysisContext,
    key: &str,
    pending: &PendingSource,
) -> Attempt {
    let source = pending.set.source().clone();
    if source.origin_node_id != context.node_id {
        // Only the origin node holds the exact object and may read it.
        return Attempt::Discard("observation is not for this origin".into());
    }
    match context.store.copy_index_v3_head(key).await {
        Ok(Some(head))
            if head.state == CopyIndexV3HeadState::Healthy
                && head.published_generation.as_deref() == Some(&source.requested_generation) =>
        {
            return Attempt::Done;
        }
        Ok(_) => {}
        Err(error) => return Attempt::Retry(error.to_string()),
    }
    let request = match coordination_request(context, &pending.file, &source).await {
        Ok(Some(request)) => request,
        Ok(None) => return Attempt::Busy,
        Err(reason) => return Attempt::Retry(reason),
    };
    let now = crate::state::clock_ms();
    let claim = match context
        .store
        .claim_analysis_coordination(
            &request.request_id,
            source.clone(),
            &context.node_id,
            request.fence,
            now,
            now.saturating_add(context.lease_ms),
        )
        .await
    {
        Ok(Some(claim)) => claim,
        Ok(None) => return Attempt::Busy,
        Err(error) => return Attempt::Retry(error.to_string()),
    };
    let Ok(Some(claimed)) = context.store.analysis_request(&claim.request_id).await else {
        return Attempt::Retry("claimed analysis request is unreadable".into());
    };
    let stop = CancellationToken::new();
    let lost = CancellationToken::new();
    let heartbeat = heartbeat(context, &claimed, stop.clone(), lost.clone());
    let outcome = publish_claimed(context, key, pending, &source, &claim, &claimed, &lost).await;
    // Retire the heartbeat before the outcome write, as the legacy worker does.
    stop.cancel();
    let _ = heartbeat.await;
    match outcome {
        Ok(()) => Attempt::Done,
        Err(attempt) => {
            if !lost.is_cancelled() {
                let now = crate::state::clock_ms();
                let (code, charge) = match &attempt {
                    Attempt::Discard(_) => ("copy_observation_refused", true),
                    _ => ("copy_publication_retry", false),
                };
                let _ = context
                    .store
                    .retry_analysis_request(
                        &claimed,
                        code,
                        now,
                        now.saturating_add(context.retry_ms),
                        charge,
                    )
                    .await;
            }
            attempt
        }
    }
}

/// Join or create the queued coordination request for this exact source. A
/// legacy request for the same file/node keeps its claim; this returns None.
async fn coordination_request(
    context: &CopyAnalysisContext,
    file: &plurx_core::domain::MediaFile,
    source: &AnalysisCoordinationSource,
) -> Result<Option<AnalysisRequest>, String> {
    let now = crate::state::clock_ms();
    let request = context
        .store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            component: "fragment_index".into(),
            pipeline_version: source.pipeline_marker(),
            video_identity: source.video_marker(),
            requested_generation: source.requested_generation.clone(),
            priority: "normal".into(),
            trigger: "background".into(),
            force_rebuild: false,
            target_node_id: source.origin_node_id.clone(),
            not_before_ms: now,
            created_at_ms: now,
        })
        .await
        .map_err(|error| error.to_string())?;
    if request.pipeline_version != source.pipeline_marker() {
        return Ok(None);
    }
    let role = context
        .store
        .establish_analysis_coordination(&request.request_id, source.clone(), now)
        .await
        .map_err(|error| error.to_string())?;
    let Some(role) = role else {
        return Ok(None);
    };
    // Rendezvous: an earlier coordinator for the same exact source wins and
    // this request was retired in the same transaction.
    let current = context
        .store
        .analysis_request(&role.request_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(current.filter(|request| request.state == "queued"))
}

fn heartbeat(
    context: &CopyAnalysisContext,
    request: &AnalysisRequest,
    stop: CancellationToken,
    lost: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let store = Arc::clone(&context.store);
    let request_id = request.request_id.clone();
    let node_id = context.node_id.clone();
    let fence = request.fence;
    let beat = crate::state::LeaseHeartbeat {
        queue: "analysis-request",
        row: request.request_id.clone(),
        fence,
        attempts: request.attempts,
        known_expiry_ms: request.lease_expires_ms,
        lease_ms: context.lease_ms,
        renew_every: context.renew_every,
        metrics: Arc::clone(&context.metrics),
        stop,
        lost,
    };
    tokio::spawn(async move {
        beat.run(move |now, expires_at| {
            let store = Arc::clone(&store);
            let request_id = request_id.clone();
            let node_id = node_id.clone();
            async move {
                store
                    .renew_analysis_request(&request_id, &node_id, fence, now, expires_at)
                    .await
            }
        })
        .await;
    })
}

async fn publish_claimed(
    context: &CopyAnalysisContext,
    key: &str,
    pending: &PendingSource,
    source: &AnalysisCoordinationSource,
    claim: &AnalysisCoordinationClaim,
    request: &AnalysisRequest,
    lost: &CancellationToken,
) -> Result<(), Attempt> {
    let namespace = pending.set.catalog_namespace().to_owned();
    let head = context
        .store
        .ensure_copy_index_v3_head(
            &namespace,
            source.clone(),
            claim.clone(),
            crate::state::clock_ms(),
        )
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?
        .ok_or(Attempt::Busy)?;
    if head.source_key != key || head.desired_generation != source.requested_generation {
        // A quarantine moved this source to its repair generation; these
        // observations belong to the old one.
        return Err(Attempt::Discard("head desires another generation".into()));
    }
    let verified = physical::attest_full_source(
        Arc::clone(&context.store),
        Arc::clone(&pending.file),
        source.clone(),
        claim.clone(),
        request.clone(),
        context.checkpoint_root.clone(),
        lost.clone(),
    )
    .await
    .map_err(Attempt::Retry)?
    .ok_or(Attempt::Busy)?;
    if lost.is_cancelled() {
        return Err(Attempt::Busy);
    }
    let index = pending
        .set
        .seal(&verified)
        .map_err(|error| Attempt::Discard(format!("{error:?}")))?;
    let source_sha256 = verified.sha256().to_owned();
    let engine = FragmentIndexEngineProvenance {
        engine_sha256: crate::ffmpeg::fragment_index_engine_digest().await,
        pipeline_sha256: hex::encode(Sha256::digest(
            format!("plurx/copy-analysis/pipeline/v1\0{}", source.video_identity).as_bytes(),
        )),
    };
    let init_contract = pending.set.init_contract_sha256().to_owned();
    let blob = plurx_core::store::encode_cluster_fragment_index_blob_v3(
        &index,
        &source_sha256,
        &init_contract,
        &engine,
    )
    .map_err(|error| Attempt::Discard(error.to_string()))?;
    let output = plurx_core::store::cluster_fragment_index_output_identity(
        &index,
        &source_sha256,
        &init_contract,
    )
    .map_err(|error| Attempt::Discard(error.to_string()))?;
    let contract_key = plurx_core::store::cluster_fragment_index_v3_key(
        &namespace,
        source.file_id,
        &source_sha256,
        &output,
    )
    .ok_or_else(|| Attempt::Discard("v3 contract key".into()))?;
    let blob_sha256 = plurx_core::store::cluster_fragment_index_blob_sha256(&blob);
    let artifact = CopyIndexV3Artifact {
        blob_sha256: blob_sha256.clone(),
        contract_key: contract_key.clone(),
        catalog_namespace: namespace.clone(),
        file_id: source.file_id,
        source_sha256: source_sha256.clone(),
        contract_version: output.contract_version,
        output_contract_sha256: output.keyframe_table_sha256.clone(),
        init_contract_sha256: init_contract.clone(),
        bytes: i64::try_from(blob.len()).map_err(|_| Attempt::Discard("blob size".into()))?,
        built_by_node_id: context.node_id.clone(),
        built_at_ms: crate::state::clock_ms(),
        producer_engine_sha256: engine.engine_sha256.clone(),
    };
    install_blob(
        &v3_root(&context.cache_dir),
        &contract_key,
        &blob_sha256,
        &blob,
    )
    .await
    .map_err(Attempt::Retry)?;
    let payload =
        plurx_core::store::copy_index_v3_publish_payload(claim, source, &artifact, head.head_epoch)
            .map_err(|error| Attempt::Discard(error.to_string()))?;
    let token = claim_publisher(context, payload, claim, &head.source_key, &contract_key).await?;
    if lost.is_cancelled() {
        return Err(Attempt::Busy);
    }
    let outcome = context
        .store
        .publish_copy_index_v3(PublishCopyIndexV3 {
            token,
            claim: claim.clone(),
            source: source.clone(),
            artifact,
            expected_head_epoch: head.head_epoch,
            now_ms: crate::state::clock_ms(),
        })
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?;
    match outcome {
        JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. } => {
            tracing::info!(
                file_id = source.file_id,
                contract_key,
                blob_sha256,
                "copy index v3 published from playback observations"
            );
            Ok(())
        }
        JobPublishOutcome::LostOwnership => Err(Attempt::Busy),
        JobPublishOutcome::SourceChanged => Err(Attempt::Discard("source changed".into())),
        JobPublishOutcome::ArtifactConflict => {
            Err(Attempt::Retry("copy index v3 artifact conflict".into()))
        }
    }
}

/// Enqueue and claim this node's publisher job. Dedupe binds the exact
/// source generation, contract and coordinator fence, never engine provenance.
async fn claim_publisher(
    context: &CopyAnalysisContext,
    payload: plurx_core::store::background_jobs::JobPayload,
    claim: &AnalysisCoordinationClaim,
    source_key: &str,
    contract_key: &str,
) -> Result<plurx_core::store::background_jobs::JobToken, Attempt> {
    let generation = match &payload {
        plurx_core::store::background_jobs::JobPayload::FragmentIndexPublish {
            requested_generation,
            ..
        } => requested_generation.clone(),
        _ => return Err(Attempt::Discard("publisher payload".into())),
    };
    let dedupe = hex::encode(Sha256::digest(
        format!(
            "plurx/copy-index/publisher/v3\0{source_key}\0{generation}\0{contract_key}\0{}\0{}",
            claim.request_id, claim.fence
        )
        .as_bytes(),
    ));
    let now = crate::state::clock_ms();
    let id = uuid::Uuid::new_v4().to_string();
    let job_id = match context
        .store
        .enqueue_job(EnqueueJob {
            id,
            payload,
            dedupe_key: dedupe.clone(),
            priority: 1,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "copy-index-v3".into(),
                request_id: dedupe.clone(),
                request_digest: dedupe,
                consumer_kind: "copy_index_v3".into(),
                consumer_ref: claim.request_id.clone(),
                target_node_id: None,
                deadline_ms: Some(now.saturating_add(context.lease_ms)),
                retain_identity: false,
            },
        })
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?
    {
        EnqueueOutcome::Accepted { job_id, .. } => job_id,
        EnqueueOutcome::Existing {
            job_id, cancelled, ..
        } if !cancelled => job_id,
        other => return Err(Attempt::Retry(format!("publisher enqueue: {other:?}"))),
    };
    let job = context
        .store
        .background_job(&job_id)
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?
        .ok_or_else(|| Attempt::Retry("publisher job vanished".into()))?;
    let now = crate::state::clock_ms();
    let request = ClaimJob {
        job_id,
        expected_revision: job.revision,
        node_id: context.node_id.clone(),
        boot_id: process_boot_id().to_owned(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        kind: JobKind::FragmentIndexPublish,
        payload_version: 1,
        now_ms: now,
        dispatched_at_ms: now,
    };
    match crate::background_jobs::claim_with_resolution(context.store.as_ref(), &job, request)
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?
    {
        Some((job, _deadline)) => job
            .token
            .ok_or_else(|| Attempt::Retry("publisher claim has no token".into())),
        None => Err(Attempt::Busy),
    }
}

/// One identity per daemon process, so a restarted daemon never resumes a
/// publisher claim it did not make.
fn process_boot_id() -> &'static str {
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

pub(crate) fn v3_root(cache_dir: &Path) -> PathBuf {
    cache_dir.join("fragment-index-v3")
}

/// Content-addressed variant path. The same bytes may serve several logical
/// associations, so the blob digest alone names the file.
pub(crate) fn v3_blob_path(root: &Path, blob_sha256: &str) -> Option<PathBuf> {
    (blob_sha256.len() == 64 && blob_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())).then(
        || {
            root.join(&blob_sha256[..2])
                .join(format!("{blob_sha256}.idx3"))
        },
    )
}

/// Install under the node's disk budget with the same staged-write and rename
/// discipline as v2 blobs. An existing identical blob is accepted as is.
async fn install_blob(
    root: &Path,
    contract_key: &str,
    blob_sha256: &str,
    blob: &[u8],
) -> Result<(), String> {
    let path = v3_blob_path(root, blob_sha256).ok_or("invalid v3 blob digest")?;
    if plurx_core::store::cluster_fragment_index_blob_sha256(blob) != blob_sha256 {
        return Err("v3 blob digest mismatch".into());
    }
    if let Ok(existing) = tokio::fs::read(&path).await {
        if plurx_core::store::cluster_fragment_index_blob_sha256(&existing) == blob_sha256 {
            return Ok(());
        }
    }
    let parent = path.parent().ok_or("v3 blob path has no parent")?;
    let reservation = crate::playback_cache::reserve(parent, blob.len() as u64)
        .await
        .map_err(|error| format!("fragment-index disk budget: {error}"))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let staging = parent.join(format!(".{contract_key}.{}.tmp", uuid::Uuid::new_v4()));
    let written = async {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging)
            .await?;
        file.write_all(blob).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&staging, &path).await
    }
    .await;
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(format!("install {}: {error}", path.display()));
    }
    if let Some(reservation) = reservation {
        reservation.commit(blob.len() as u64);
    }
    Ok(())
}
