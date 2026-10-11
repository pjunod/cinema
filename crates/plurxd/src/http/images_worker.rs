//! Durable artwork preparation. The queue owns execution and delivery; the
//! existing artwork cache owns bytes, read admission, identity and eviction.
use super::*;
use plurx_core::store::background_jobs::{
    CandidateCursor, CandidateQuery, ClaimJob, JobKind, JobPayload, JobSettlement,
};
use plurx_core::store::background_jobs_artwork::{
    ArtworkLocation, ArtworkVariantSpec, ARTWORK_PIPELINE,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub(super) struct CachedLocation {
    observed: Instant,
    location: Option<ArtworkLocation>,
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
async fn pipeline() -> String {
    format!(
        "{ARTWORK_PIPELINE}:{}",
        crate::ffmpeg::fragment_index_engine_digest().await
    )
}
pub(super) async fn spec(
    digest: [u8; 32],
    size: ArtworkSize,
    source: &str,
) -> Option<ArtworkVariantSpec> {
    let format = match FsPath::new(source)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "jpeg",
        "gif" | "png" => "png",
        "webp" => "webp",
        _ => return None,
    };
    let spec = ArtworkVariantSpec {
        source_name: source.into(),
        source_sha256: hex::encode(digest),
        width: size.width()?,
        format: format.into(),
        pipeline: pipeline().await,
    };
    spec.validate().ok()?;
    Some(spec)
}
fn size(spec: &ArtworkVariantSpec) -> Option<ArtworkSize> {
    match spec.width {
        300 => Some(ArtworkSize::W300),
        500 => Some(ArtworkSize::W500),
        780 => Some(ArtworkSize::W780),
        _ => None,
    }
}
fn filename(spec: &ArtworkVariantSpec) -> Option<String> {
    let digest: [u8; 32] = hex::decode(&spec.source_sha256).ok()?.try_into().ok()?;
    let base = derivative_filename(digest, size(spec)?, &spec.source_name)?;
    let (stem, extension) = base.rsplit_once('.')?;
    let pipeline = hex::encode(Sha256::digest(spec.pipeline.as_bytes()));
    Some(format!("{stem}-{}.{extension}", &pipeline[..16]))
}
pub(super) fn location_filename(location: &ArtworkLocation) -> Option<String> {
    location.spec.validate().ok()?;
    let key = location.spec.artifact_key();
    let blob = &location.blob_sha256;
    if blob.len() != 64 || !blob.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("variant-{key}-b{blob}.{}", location.spec.format))
}

pub(super) fn published_key(filename: &str) -> Option<&str> {
    let (stem, extension) = filename.rsplit_once('.')?;
    if !matches!(extension, "jpeg" | "png" | "webp") {
        return None;
    }
    let (key, blob) = stem.strip_prefix("variant-")?.split_once("-b")?;
    (key.len() == 64
        && blob.len() == 64
        && key
            .bytes()
            .chain(blob.bytes())
            .all(|b| b.is_ascii_hexdigit()))
    .then_some(key)
}

pub(super) fn staging_owner(filename: &str) -> Option<(String, i64)> {
    let body = filename.trim_start_matches('.').strip_prefix("work-")?;
    let id = body.get(..36)?;
    uuid::Uuid::parse_str(id).ok()?;
    let fence = body.get(37..)?.split('.').next()?.parse::<i64>().ok()?;
    (body.as_bytes().get(36) == Some(&b'-') && fence > 0).then(|| (id.into(), fence))
}

async fn remember(state: &AppState, key: &str, location: Option<ArtworkLocation>) {
    let mut entries = state.artwork_fetch.variants.lock().await;
    if entries.len() >= VERIFIED_ARTWORK_CACHE && !entries.contains_key(key) {
        // Disposable proof cache: dropping an entry restores an authority read,
        // never invents permission to serve an uncommitted filesystem orphan.
        if let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, value)| value.observed)
            .map(|(key, _)| key.clone())
        {
            entries.remove(&oldest);
        }
    }
    entries.insert(
        key.into(),
        CachedLocation {
            observed: Instant::now(),
            location,
        },
    );
}
async fn local_location(state: &AppState, key: &str) -> Option<ArtworkLocation> {
    if let Some(entry) = state.artwork_fetch.variants.lock().await.get(key).cloned() {
        let ttl = if entry.location.is_some() {
            VERIFIED_ARTWORK_TTL
        } else {
            Duration::from_secs(5)
        };
        if entry.observed.elapsed() < ttl {
            return entry.location;
        }
    }
    let location = state
        .store
        .artwork_locations(key, now_ms())
        .await
        .ok()?
        .into_iter()
        .find(|location| location.node_id == state.node_id);
    remember(state, key, location.clone()).await;
    location
}
async fn serve_location(
    state: &AppState,
    location: &ArtworkLocation,
    headers: &HeaderMap,
) -> Result<Option<Response>, ApiError> {
    let Some(name) = location_filename(location) else {
        return Ok(None);
    };
    let root = state.artwork_dir.join(DERIVED_DIR);
    let path = root.join(&name);
    if let Some(digest) = cached_not_modified(&state.artwork_fetch, &path, &name, headers).await {
        if hex::encode(digest) == location.blob_sha256 {
            return Ok(Some(not_modified_artwork_response(digest)));
        }
    }
    match read_verified_local_artwork(&state.artwork_fetch, path.clone(), &name).await {
        LocalArtworkRead::Verified(bytes)
            if hex::encode(bytes.digest) == location.blob_sha256
                && bytes.bytes.len() as i64 == location.bytes =>
        {
            Ok(Some(admitted_artwork_response(
                &path,
                bytes,
                ARTWORK_CACHE_CONTROL,
                &[],
            )))
        }
        LocalArtworkRead::Verified(bytes) => {
            if let Some(identity) = bytes.identity {
                quarantine_corrupt_artwork_digest(
                    &state.artwork_fetch,
                    &root,
                    &name,
                    identity,
                    None,
                    Some(location.blob_sha256.clone()),
                )
                .await;
            }
            Ok(None)
        }
        LocalArtworkRead::Capacity => Err(artwork_capacity_error()),
        _ => Ok(None),
    }
}
pub(super) async fn serve_local_variant(
    state: &AppState,
    spec: &ArtworkVariantSpec,
    headers: &HeaderMap,
) -> Result<Option<Response>, ApiError> {
    let Some(location) = local_location(state, &spec.artifact_key()).await else {
        return Ok(None);
    };
    serve_location(state, &location, headers).await
}
/// Sharing reads a fresh publication selection rather than the disposable
/// twenty-four-hour node-local proof cache. The same closed spec/name grammar
/// applies; no worker or scheduler is started by this read.
pub(super) async fn shared_local_variant(
    state: &AppState,
    spec: &ArtworkVariantSpec,
) -> Option<ArtworkLocation> {
    state
        .store
        .artwork_locations(&spec.artifact_key(), now_ms())
        .await
        .ok()?
        .into_iter()
        .find(|location| {
            location.node_id == state.node_id
                && location.spec.artifact_key() == spec.artifact_key()
                && location_filename(location).is_some()
        })
}
pub(super) async fn serve_peer_variant(
    state: &AppState,
    key: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ApiError::BadRequest("invalid artwork variant key".into()));
    }
    let location = local_location(state, key)
        .await
        .ok_or(ApiError::NotFound("artwork variant"))?;
    serve_location(state, &location, headers)
        .await?
        .ok_or(ApiError::NotFound("artwork variant"))
}

async fn copy_from_peer(
    state: &AppState,
    location: &ArtworkLocation,
    cancel: &CancellationToken,
) -> Option<()> {
    let _peer = state.artwork_fetch.peer_permit()?;
    let _bytes = state.artwork_fetch.local_permit(MAX_ARTWORK_BYTES).await?;
    let client = state.artwork_fetch.client()?;
    let name = format!("variant-{}", location.artifact_key);
    let auth = state.membership.artwork_peer_auth(&name).ok()?;
    let peers = state.membership.reachable_peer_http_urls().await.ok()?;
    let bytes = tokio::select! {
        () = cancel.cancelled() => return None,
        bytes = fetch_peer_artwork_checked(&client, &peers, &name, &auth, Some((&location.blob_sha256, location.bytes))) => bytes?,
    };
    if cancel.is_cancelled() {
        return None;
    }
    let directory = ensure_derived_dir(&state.artwork_dir).await.ok()?;
    let local_name = location_filename(location)?;
    plurx_core::fs_secure::atomic_write_child(&directory, &local_name, &bytes)
        .await
        .ok()?;
    state.artwork_fetch.forget(&local_name).await;
    Some(())
}

async fn prepare(
    state: &AppState,
    job: &plurx_core::store::background_jobs::BackgroundJob,
    fence: &crate::background_jobs::JobFence,
    cancel: &CancellationToken,
) -> Result<bool, String> {
    let payload = job.supported_payload().map_err(|e| e.to_string())?;
    if let JobPayload::ArtifactVerify { artifact_key, .. } = &payload {
        return verify_location(state, artifact_key, fence, cancel).await;
    }
    let (key, supplied) = match payload {
        JobPayload::ArtworkDerivative { artifact_key, spec } => (artifact_key, Some(spec)),
        JobPayload::ArtifactHydrate { artifact_key, .. } => (
            artifact_key
                .strip_prefix("artwork:")
                .ok_or("not artwork")?
                .into(),
            None,
        ),
        _ => return Err("not artwork".into()),
    };
    let locations = state
        .store
        .artwork_locations(&key, now_ms())
        .await
        .map_err(|e| e.to_string())?;
    let canonical = if locations.is_empty() {
        state
            .store
            .artwork_variant(&key)
            .await
            .map_err(|e| e.to_string())?
    } else {
        None
    };
    let original = locations
        .iter()
        .find(|location| location.node_id == state.node_id)
        .or_else(|| locations.first())
        .or(canonical.as_ref());
    let spec = supplied
        .clone()
        .or_else(|| original.map(|location| location.spec.clone()))
        .ok_or("artwork manifest unavailable")?;
    let name = filename(&spec).ok_or("unsupported artwork variant")?;
    let directory = ensure_derived_dir(&state.artwork_dir)
        .await
        .map_err(|e| e.to_string())?;
    let _flight = state
        .artwork_fetch
        .filename(&format!("variant:{key}"))
        .await;
    let mut verified = None;
    let mut generated = false;
    let staging = format!(
        ".work-{}-{}.{}",
        job.id,
        job.fence,
        name.rsplit_once('.').ok_or("missing artwork format")?.1
    );
    if let Some(original) = original {
        let mut destination = original.clone();
        destination.spec = spec.clone();
        let ready_name = location_filename(&destination).ok_or("invalid artwork location name")?;
        if let LocalArtworkRead::Verified(bytes) = read_verified_local_artwork(
            &state.artwork_fetch,
            directory.join(&ready_name),
            &ready_name,
        )
        .await
        {
            if hex::encode(bytes.digest) == original.blob_sha256
                && bytes.bytes.len() as i64 == original.bytes
            {
                verified = Some(bytes);
            }
        }
        if verified.is_none() && copy_from_peer(state, &destination, cancel).await.is_some() {
            if let LocalArtworkRead::Verified(bytes) = read_verified_local_artwork(
                &state.artwork_fetch,
                directory.join(&ready_name),
                &ready_name,
            )
            .await
            {
                if hex::encode(bytes.digest) == original.blob_sha256
                    && bytes.bytes.len() as i64 == original.bytes
                {
                    verified = Some(bytes);
                }
            }
        }
    }
    if verified.is_none() {
        // Delivery never starts a second encoder. Only the one canonical
        // derivative job may rebuild an output whose holders are unavailable.
        if supplied.is_none() {
            return Err("artwork holders unavailable".into());
        }
        if spec.pipeline != pipeline().await
            || !crate::ffmpeg::fragment_index_engine_is_current().await
        {
            return Err("artwork pipeline changed".into());
        }
        let source_path = state.artwork_dir.join(&spec.source_name);
        let LocalArtworkRead::Verified(source) = read_verified_local_artwork(
            &state.artwork_fetch,
            source_path.clone(),
            &spec.source_name,
        )
        .await
        else {
            return Err("artwork source unavailable".into());
        };
        if hex::encode(source.digest) != spec.source_sha256 {
            return Err("artwork source changed".into());
        }
        let identity = source
            .identity
            .ok_or("artwork source identity unavailable")?;
        generated = true;
        generate_derivative_cancellable(
            &state.artwork_fetch,
            DerivativeJob {
                runtime_cache: &state.runtime_cache_dir,
                source_path: &source_path,
                source_name: &spec.source_name,
                source_identity: identity,
                derived_dir: &directory,
                derived_name: &staging,
                size: size(&spec).ok_or("unsupported width")?,
            },
            &crate::ffmpeg::ffmpeg_bin(),
            Some(cancel),
        )
        .await?;
        if !crate::ffmpeg::fragment_index_engine_is_current().await {
            return Err("artwork pipeline changed".into());
        }
        if let LocalArtworkRead::Verified(bytes) =
            read_verified_local_artwork(&state.artwork_fetch, directory.join(&staging), &staging)
                .await
        {
            verified = Some(bytes);
        }
    }
    let bytes = verified.ok_or("artwork output unavailable")?;
    let (width, height) = image_dimensions(&name, &bytes.bytes).ok_or("invalid artwork output")?;
    if width == 0 || width > spec.width || height == 0 {
        return Err("invalid artwork dimensions".into());
    }
    if cancel.is_cancelled() {
        return Ok(false);
    }
    let location = ArtworkLocation {
        artifact_key: key.clone(),
        node_id: state.node_id.clone(),
        spec,
        blob_sha256: hex::encode(bytes.digest),
        bytes: bytes.bytes.len() as i64,
        built_by_node_id: original
            .map(|l| l.built_by_node_id.clone())
            .unwrap_or_else(|| state.node_id.clone()),
        built_at_ms: original.map(|l| l.built_at_ms).unwrap_or_else(now_ms),
        verified_at_ms: now_ms(),
    };
    let ready_name = location_filename(&location).ok_or("invalid artwork output name")?;
    if generated {
        plurx_core::fs_secure::atomic_write_child(&directory, &ready_name, &bytes.bytes)
            .await
            .map_err(|e| e.to_string())?;
        state.artwork_fetch.forget(&ready_name).await;
        remove_derivative_temporary(&directory.join(&staging)).await;
    }
    if cancel.is_cancelled() {
        return Ok(false);
    }
    if !fence
        .publish_artwork(location.clone())
        .await
        .map_err(|e| e.to_string())?
    {
        return Ok(false);
    }
    if generated {
        if let Some(size) = size(&location.spec) {
            record_derivative(size, DerivativeOutcome::Generated);
        }
    }
    remember(state, &key, Some(location)).await;
    Ok(true)
}

pub(super) async fn request(state: &AppState, spec: ArtworkVariantSpec) {
    let key = spec.artifact_key();
    {
        let mut demands = state.artwork_fetch.demands.lock().await;
        if demands
            .get(&key)
            .is_some_and(|seen| seen.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        if demands.len() >= VERIFIED_ARTWORK_CACHE && !demands.contains_key(&key) {
            if let Some(oldest) = demands
                .iter()
                .min_by_key(|(_, seen)| **seen)
                .map(|(key, _)| key.clone())
            {
                demands.remove(&oldest);
            }
        }
        demands.insert(key.clone(), Instant::now());
    }
    match state
        .store
        .enqueue_artwork_demand(spec, &state.node_id, now_ms())
        .await
    {
        Ok(_) => state.artwork_fetch.wake.notify_one(),
        Err(error) => {
            state.artwork_fetch.demands.lock().await.remove(&key);
            tracing::warn!(%error, "could not persist artwork demand");
        }
    }
}

async fn verify_location(
    state: &AppState,
    key: &str,
    fence: &crate::background_jobs::JobFence,
    cancel: &CancellationToken,
) -> Result<bool, String> {
    let key = key.strip_prefix("artwork:").ok_or("not artwork")?;
    let location = state
        .store
        .artwork_locations(key, now_ms())
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|location| location.node_id == state.node_id);
    let Some(location) = location else {
        fence
            .settle(JobSettlement::Stop {
                error_code: "verification_location_retired".into(),
            })
            .await
            .map_err(|error| error.to_string())?;
        return Ok(true);
    };
    let name = location_filename(&location).ok_or("invalid artwork locator")?;
    // Re-hash bytes even if a foreground read populated the disposable digest cache.
    state.artwork_fetch.forget(&name).await;
    let result = read_verified_local_artwork(
        &state.artwork_fetch,
        state.artwork_dir.join(DERIVED_DIR).join(&name),
        &name,
    )
    .await;
    if cancel.is_cancelled() {
        return Ok(false);
    }
    let valid = match result {
        LocalArtworkRead::Capacity => {
            cancel.cancel();
            return Ok(false);
        }
        LocalArtworkRead::Verified(bytes) => {
            hex::encode(bytes.digest) == location.blob_sha256
                && bytes.bytes.len() as i64 == location.bytes
        }
        _ => false,
    };
    let published = fence
        .verify_artwork(location, valid)
        .await
        .map_err(|error| error.to_string())?;
    if published {
        remember(state, key, None).await;
    }
    Ok(published)
}

pub(crate) async fn run(state: AppState) {
    let boot = uuid::Uuid::new_v4().to_string();
    let mut cursor: Option<CandidateCursor> = None;
    let mut pacing = crate::background_jobs::IdlePoll::new();
    let mut next_verification = Instant::now();
    loop {
        if Instant::now() >= next_verification
            && state
                .jobs
                .execution_authority()
                .may_execute_job(JobKind::ArtifactVerify)
                .await
        {
            next_verification = Instant::now() + Duration::from_secs(60);
            match state
                .store
                .artwork_verification_candidates(&state.node_id)
                .await
            {
                Ok(locations) => {
                    for location in locations.into_iter().take(8) {
                        let generation =
                            format!("{}:{}", location.blob_sha256, location.built_at_ms);
                        if let Err(error) = crate::artifact_integrity::enqueue_artifact(
                            &state.store,
                            &state.node_id,
                            format!("artwork:{}", location.artifact_key),
                            &generation,
                            now_ms(),
                        )
                        .await
                        {
                            tracing::warn!(%error,"artwork verification admission failed");
                        }
                    }
                }
                Err(error) => tracing::warn!(%error,"artwork verification discovery failed"),
            }
        }
        let mapped_progress = super::jellyfin_artwork_pass(&state).await;
        let progressed = pass(&state, &boot, &mut cursor)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "durable artwork pass failed");
                false
            });
        let progressed = progressed || mapped_progress;
        tokio::select! {
            () = state.artwork_fetch.wake.notified() => {},
            () = tokio::time::sleep(pacing.after_completion(progressed)) => {},
        }
    }
}
async fn pass(
    state: &AppState,
    boot: &str,
    cursor: &mut Option<CandidateCursor>,
) -> Result<bool, StoreError> {
    let authority = state.jobs.execution_authority();
    if authority.may_run_cluster_jobs().await {
        state.jobs.enqueue_artifact_deliveries().await;
    }
    let mut kinds = Vec::new();
    for kind in [
        JobKind::ArtworkDerivative,
        JobKind::ArtifactHydrate,
        JobKind::ArtifactVerify,
    ] {
        if authority.may_execute_job(kind).await {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        return Ok(false);
    }
    let mut page = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds,
            after: cursor.clone(),
            now_ms: now_ms(),
            limit: 128,
        })
        .await?;
    page.jobs.retain(|job| match job.supported_payload() {
        Ok(JobPayload::ArtworkDerivative { .. }) => true,
        Ok(JobPayload::ArtifactHydrate {
            artifact_key,
            target_node_id,
        })
        | Ok(JobPayload::ArtifactVerify {
            artifact_key,
            target_node_id,
            ..
        }) => artifact_key.starts_with("artwork:") && target_node_id == state.node_id,
        _ => false,
    });
    if page.jobs.is_empty() {
        *cursor = page.next;
        return Ok(false);
    }
    let Some(admission) = state.transcode.admit_fragment().await else {
        return Ok(false);
    };
    let Some(_derive) = state.artwork_fetch.derive_permit().await else {
        return Ok(false);
    };

    let local_pipeline = pipeline().await;
    for candidate in page.jobs {
        let kind = match candidate.supported_payload() {
            Ok(JobPayload::ArtworkDerivative { spec, .. }) if spec.pipeline == local_pipeline => {
                if tokio::fs::File::open(state.artwork_dir.join(&spec.source_name))
                    .await
                    .is_err()
                {
                    continue;
                }
                JobKind::ArtworkDerivative
            }
            Ok(JobPayload::ArtifactHydrate {
                artifact_key,
                target_node_id,
            }) if artifact_key.starts_with("artwork:") && target_node_id == state.node_id => {
                JobKind::ArtifactHydrate
            }
            Ok(JobPayload::ArtifactVerify {
                artifact_key,
                target_node_id,
            }) if artifact_key.starts_with("artwork:") && target_node_id == state.node_id => {
                JobKind::ArtifactVerify
            }
            _ => continue,
        };
        if !authority.may_execute_job(kind).await
            || !state.transcode.fragment_worker_idle(&admission)
        {
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
                kind,
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
            Arc::clone(&authority),
            job.token
                .clone()
                .ok_or_else(|| StoreError::Task("artwork claim missing token".into()))?,
            deadline,
            kind,
        )?;
        let fence = active.fence();
        let cancel = fence.loss_token().child_token();
        let result = {
            let work = prepare(state, &job, &fence, &cancel);
            tokio::pin!(work);
            loop {
                tokio::select! {
                    result = &mut work => break result,
                    () = tokio::time::sleep(Duration::from_millis(250)) => {
                        if !state.transcode.fragment_worker_idle(&admission) { cancel.cancel(); }
                    }
                }
            }
        };
        if let Ok(JobPayload::ArtworkDerivative { spec, .. }) = job.supported_payload() {
            if let Some(base) = filename(&spec) {
                if let Some((_, extension)) = base.rsplit_once('.') {
                    let staging = format!(".work-{}-{}.{}", job.id, job.fence, extension);
                    remove_derivative_temporary(&state.artwork_dir.join(DERIVED_DIR).join(staging))
                        .await;
                }
            }
        }
        let published = matches!(result, Ok(true));
        if !published {
            let settlement = if cancel.is_cancelled() {
                JobSettlement::Yield {
                    error_code: Some("worker_interrupted".into()),
                    not_before_ms: now_ms().saturating_add(5000),
                    checkpoint: None,
                }
            } else {
                if let Err(error) = result {
                    tracing::warn!(job = job.id, %error, "artwork preparation failed");
                }
                JobSettlement::Retry {
                    error_code: "artwork_unavailable".into(),
                    not_before_ms: now_ms().saturating_add(crate::background_jobs::retry_delay_ms(
                        &job.id,
                        job.failed_attempts,
                    )),
                }
            };
            if let Err(error) = fence.settle(settlement).await {
                tracing::warn!(%error, "artwork settlement unavailable");
            }
        }
        active.finish().await;
        // admission and the derive permit outlive all reads, writers and the
        // joined cancellation collector above.
        *cursor = None;
        return Ok(published);
    }
    *cursor = page.next;
    Ok(false)
}

#[cfg(test)]
pub(super) async fn run_one(state: &AppState) -> Result<bool, StoreError> {
    pass(state, &uuid::Uuid::new_v4().to_string(), &mut None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn a_renamed_but_unpublished_variant_cannot_be_served() {
        let state = super::super::tests::derivative_state();
        let source = "orphan.png";
        super::super::tests::write_test_image(
            &state.artwork_dir.join(source),
            "color=c=blue:s=640x360",
            &["-frames:v", "1"],
        );
        let bytes = tokio::fs::read(state.artwork_dir.join(source))
            .await
            .expect("source");
        let spec = spec(Sha256::digest(&bytes).into(), ArtworkSize::W300, source)
            .await
            .expect("spec");
        let root = ensure_derived_dir(&state.artwork_dir)
            .await
            .expect("directory");
        let name = filename(&spec).expect("name");
        tokio::fs::write(root.join(&name), b"uncommitted owner output")
            .await
            .expect("orphan");
        assert!(serve_local_variant(&state, &spec, &HeaderMap::new())
            .await
            .expect("lookup")
            .is_none());
        let response = serve_derivative(&state, source, &HeaderMap::new(), ArtworkSize::W300)
            .await
            .expect("fallback");
        assert_eq!(
            response.headers().get("x-plurx-artwork").expect("fallback"),
            "original-fallback"
        );
        drop(response);
        assert!(run_one(&state).await.expect("durable worker"));
        assert!(serve_local_variant(&state, &spec, &HeaderMap::new())
            .await
            .expect("published lookup")
            .is_some());
        // A rewritten inode must be checked against the committed digest,
        // quarantined, and refused even if a previous hit cached the proof.
        let location = state
            .store
            .artwork_locations(&spec.artifact_key(), now_ms())
            .await
            .expect("location")
            .remove(0);
        let ready_name = location_filename(&location).expect("ready name");
        assert_eq!(
            published_key(&ready_name),
            Some(spec.artifact_key().as_str())
        );
        super::super::tests::age_past_the_orphan_grace(&root.join(&ready_name)).await;
        let mut orphan = location.clone();
        orphan.blob_sha256 = "a".repeat(64);
        let orphan_name = location_filename(&orphan).expect("orphan name");
        tokio::fs::write(root.join(&orphan_name), b"abandoned generation")
            .await
            .expect("orphan");
        super::super::tests::age_past_the_orphan_grace(&root.join(&orphan_name)).await;
        let publishing = state
            .artwork_fetch
            .derive_permit()
            .await
            .expect("publishing");
        assert_eq!(
            sweep_derived_orphans(&state).await,
            0,
            "GC must exclude publication"
        );
        assert!(root.join(&orphan_name).exists());
        drop(publishing);
        assert_eq!(sweep_derived_orphans(&state).await, 1);
        assert!(root.join(&ready_name).exists(), "published bytes retained");
        assert!(
            !root.join(&orphan_name).exists(),
            "unpublished digest reclaimed"
        );
        plurx_core::fs_secure::atomic_write_child(&root, &ready_name, b"corrupt replacement")
            .await
            .expect("replace");
        assert!(serve_local_variant(&state, &spec, &HeaderMap::new())
            .await
            .expect("corrupt lookup")
            .is_none());
        assert!(!root.join(ready_name).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artwork_cancellation_reaps_the_encoder_before_returning() {
        let dir = crate::test_tempdir().expect("directory");
        let program = dir.path().join("encoder");
        let announced = dir.path().join("pid");
        crate::write_test_executable(
            &program,
            format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 300\n",
                announced.display()
            ),
            0o700,
        );
        let source = dir.path().join("source.png");
        tokio::fs::write(&source, b"source").await.expect("source");
        let identity = open_local_artwork(source.clone())
            .await
            .expect("identity")
            .identity;
        let cancel = CancellationToken::new();
        let coordinator = ArtworkCoordinator::new();
        let operation = generate_derivative_cancellable(
            &coordinator,
            DerivativeJob {
                runtime_cache: dir.path(),
                source_path: &source,
                source_name: "source.png",
                source_identity: identity,
                derived_dir: dir.path(),
                derived_name: "variant.png",
                size: ArtworkSize::W300,
            },
            program.to_str().expect("program"),
            Some(&cancel),
        );
        tokio::pin!(operation);
        let mut observed = false;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    result = &mut operation => break result,
                    () = tokio::time::sleep(Duration::from_millis(10)) => {
                        if announced.exists() { observed = true; cancel.cancel(); }
                    }
                }
            }
        })
        .await
        .expect("bounded cancellation");
        assert!(observed);
        assert!(result.is_err());
        let pid: i32 = std::fs::read_to_string(announced)
            .expect("pid")
            .trim()
            .parse()
            .expect("number");
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "encoder must have exited"
        );
        assert!(!dir.path().join("variant.png").exists());
    }
}
