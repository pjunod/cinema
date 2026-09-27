//! Manifest-addressed peer copies. Transfers use the ordinary cache ownership
//! guards; only exact queue publication advertises the final generation.
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use plurx_core::error::StoreError;
use plurx_core::fs_secure::SecureDirectory;
use plurx_core::store::background_jobs::{
    BackgroundJob, CandidateCursor, CandidateQuery, ClaimJob, JobKind, JobPayload, JobSettlement,
    TranscodeJobOutput,
};
use plurx_core::store::background_jobs_transcode::{artifact_key, parse_key, TranscodeCopySource};
use plurx_core::transcode::manifest::{self, GenerationManifest};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::peer_transport::{PeerAuthMode, PeerTransport};
use crate::state::AppState;

pub(crate) const PREFIX: &str = "/internal/media/cache-copy/";
static READS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));
static PEERS: LazyLock<Mutex<HashMap<String, Arc<Semaphore>>>> = LazyLock::new(Default::default);

pub(crate) fn route_eligible(path: &str) -> bool {
    let Some(suffix) = path.strip_prefix(PREFIX) else {
        return false;
    };
    let parts = suffix.split('/').collect::<Vec<_>>();
    matches!(parts.as_slice(), [recipe, digest, object]
        if parse_key(&artifact_key(recipe,digest)).is_some()
        && (*object == "manifest" || object.parse::<usize>().is_ok_and(|index| index < manifest::MAX_OBJECTS)))
}
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
fn permits(peer: &str) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), StatusCode> {
    let global = READS
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let peer = {
        let mut peers = PEERS.lock().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if peers.len() >= 128 && !peers.contains_key(peer) {
            peers.retain(|_, semaphore| Arc::strong_count(semaphore) > 1);
            if peers.len() >= 128 {
                return Err(StatusCode::TOO_MANY_REQUESTS);
            }
        }
        peers
            .entry(peer.into())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }
    .try_acquire_owned()
    .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    Ok((global, peer))
}
struct VerifiedStream<S> {
    reader: S,
    _snapshot: Option<manifest::VerifiedObjectLease>,
    _cache: crate::cachekeep::CacheReadGuard,
    _global: OwnedSemaphorePermit,
    _peer: OwnedSemaphorePermit,
}
impl<S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin> Stream for VerifiedStream<S> {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.reader).poll_next(context)
    }
}
fn response(body: Body, length: u64) -> Result<Response, StatusCode> {
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "private, no-store".parse().expect("literal"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/octet-stream".parse().expect("literal"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        length
            .to_string()
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    Ok(response)
}

pub(crate) async fn serve(
    State(state): State<AppState>,
    Path((recipe, digest, object)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let path = format!("{PREFIX}{recipe}/{digest}/{object}");
    if !route_eligible(&path) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let peer = super::internal_media::authorize(&state, &headers, "GET", &path, &[]).await?;
    let (global, peer) = permits(&peer)?;
    let cache = state
        .transcode
        .cache_readers()
        .begin_read(&recipe)
        .ok_or(StatusCode::CONFLICT)?;
    let (root, node) = state
        .transcode
        .cache_location()
        .ok_or(StatusCode::NOT_FOUND)?;
    let location = state
        .store
        .transcode_copy_sources(&artifact_key(&recipe, &digest))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .into_iter()
        .find(|source| source.node_id == node)
        .ok_or(StatusCode::NOT_FOUND)?;
    let directory = crate::cachekeep::validated_entry_dir(root, &location.relative_dir)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let manifest = manifest::load(&directory)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if manifest.manifest_digest != digest {
        return Err(StatusCode::NOT_FOUND);
    }
    if object == "manifest" {
        let bytes = serde_json::to_vec(&manifest).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if bytes.len() as u64 > manifest::MAX_MANIFEST_BYTES {
            return Err(StatusCode::NOT_FOUND);
        }
        let length = bytes.len() as u64;
        return response(
            Body::from_stream(VerifiedStream {
                reader: futures_util::stream::iter([Ok(Bytes::from(bytes))]),
                _snapshot: None,
                _cache: cache,
                _global: global,
                _peer: peer,
            }),
            length,
        );
    }
    let index = object
        .parse::<usize>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let object = manifest.objects.get(index).ok_or(StatusCode::NOT_FOUND)?;
    let verified = manifest
        .open_verified_object(&directory, &object.name)
        .await
        .map_err(|error| {
            if error.is_capacity() {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::NOT_FOUND
            }
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    let length = verified.bytes;
    response(
        Body::from_stream(VerifiedStream {
            reader: tokio_util::io::ReaderStream::with_capacity(
                verified.file,
                crate::media_sessions::MEDIA_BODY_READ_BUFFER,
            ),
            _snapshot: Some(verified.lease),
            _cache: cache,
            _global: global,
            _peer: peer,
        }),
        length,
    )
}

/// Both manifest and object requests share one bounded transfer path. Hashing
/// each network chunk avoids a detached CPU task and detects overlong bodies
/// before they can consume more than one object's fixed memory ceiling.
#[allow(clippy::too_many_arguments)]
async fn fetch(
    transport: &PeerTransport,
    peer: &str,
    base: &str,
    path: &str,
    maximum: u64,
    expected: Option<&str>,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(60));
    let response = tokio::select! {
        () = cancel.cancelled() => return Err("copy cancelled".into()),
        response = transport.request_stream(peer, base, reqwest::Method::GET, path, Vec::new(), deadline, PeerAuthMode::ExactRequest) => response.map_err(|error| format!("peer request: {error:?}"))?,
    };
    receive_body(response, maximum, expected, deadline, cancel).await
}

async fn receive_body(
    mut response: reqwest::Response,
    maximum: u64,
    expected: Option<&str>,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    if maximum > manifest::MAX_OBJECT_BYTES
        || !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > maximum)
    {
        return Err("peer refused copy or advertised an oversized object".into());
    }
    let mut bytes = Vec::with_capacity(maximum.min(64 * 1024) as usize);
    let mut hash = Sha256::new();
    loop {
        let chunk = tokio::select! {
            () = cancel.cancelled() => return Err("copy cancelled".into()),
            chunk = tokio::time::timeout_at(deadline, response.chunk()) => chunk.map_err(|_| "peer body deadline")?.map_err(|error| error.to_string())?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        if (bytes.len() as u64).saturating_add(chunk.len() as u64) > maximum {
            return Err("peer object exceeded its bound".into());
        }
        hash.update(&chunk);
        bytes.extend_from_slice(&chunk);
    }
    if expected.is_some_and(|expected| {
        bytes.len() as u64 != maximum || hex::encode(hash.finalize()) != expected
    }) {
        return Err("peer object does not match its manifest".into());
    }
    Ok(bytes)
}

async fn local_output(
    state: &AppState,
    source: &TranscodeCopySource,
    cancel: &CancellationToken,
) -> Option<TranscodeJobOutput> {
    let (root, node) = state.transcode.cache_location()?;
    if source.node_id != node {
        return None;
    }
    let _reader = state
        .transcode
        .cache_readers()
        .begin_read(&source.recipe_hash)?;
    let directory = crate::cachekeep::validated_entry_dir(root, &source.relative_dir).await?;
    let manifest = manifest::load(&directory).await.ok()?;
    if manifest.manifest_digest != source.manifest_digest {
        return None;
    }
    for object in &manifest.objects {
        if cancel.is_cancelled()
            || !manifest
                .verify_object(&directory, &object.name)
                .await
                .ok()?
        {
            return None;
        }
    }
    Some(TranscodeJobOutput {
        recipe_hash: source.recipe_hash.clone(),
        manifest_digest: source.manifest_digest.clone(),
        recipe_version: source.recipe_version,
        relative_dir: source.relative_dir.clone(),
        bytes: source.bytes,
        expected_previous_bytes: None,
    })
}

async fn prepare(
    state: &AppState,
    transport: &PeerTransport,
    job: &BackgroundJob,
    fence: &crate::background_jobs::JobFence,
    cancel: &CancellationToken,
) -> Result<bool, String> {
    let JobPayload::ArtifactHydrate {
        artifact_key: key, ..
    } = job.supported_payload().map_err(|error| error.to_string())?
    else {
        return Err("not a cache copy".into());
    };
    let (recipe, digest) = parse_key(&key).ok_or("invalid cache copy identity")?;
    // Pin the recipe through verification and publication, including reuse of
    // an already-local generation. GC cannot remove it in that interval.
    let _reader = state
        .transcode
        .cache_readers()
        .begin_read(recipe)
        .ok_or("cache eviction owns recipe")?;
    let sources = state
        .store
        .transcode_copy_sources(&key)
        .await
        .map_err(|error| error.to_string())?;
    for source in &sources {
        if let Some(output) = local_output(state, source, cancel).await {
            if cancel.is_cancelled() {
                return Ok(false);
            }
            return fence
                .publish_transcode(output)
                .await
                .map_err(|error| error.to_string());
        }
    }
    if cancel.is_cancelled() {
        return Ok(false);
    }
    let (root, node) = state
        .transcode
        .cache_location()
        .ok_or("local cache is unavailable")?;
    if node != state.node_id {
        return Err("local cache node identity differs".into());
    }
    // A corrupt old location must be invalidated by its exact immutable identity
    // before a replacement may be published. Existing readers keep their pins.
    for source in sources.iter().filter(|source| source.node_id == node) {
        state
            .store
            .invalidate_cache_entry(recipe, node, "local", &source.relative_dir, Some(digest))
            .await
            .map_err(|error| error.to_string())?;
    }
    let peers = state
        .membership
        .operations_peers()
        .await
        .map_err(|error| error.to_string())?;
    let holders = sources
        .iter()
        .filter_map(|source| {
            if source.node_id == node {
                return None;
            }
            peers
                .iter()
                .find(|peer| peer.node_id == source.node_id && peer.reachable)
                .and_then(|peer| peer.http_base.as_deref())
                .map(|base| (source, base))
        })
        .take(3)
        .collect::<Vec<_>>();
    let mut received: Option<(GenerationManifest, Vec<u8>, usize)> = None;
    let deadline = Instant::now() + Duration::from_secs(600);
    for (index, (source, base)) in holders.iter().enumerate() {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let path = format!("{PREFIX}{recipe}/{digest}/manifest");
        if let Ok(encoded) = fetch(
            transport,
            &source.node_id,
            base,
            &path,
            manifest::MAX_MANIFEST_BYTES,
            None,
            deadline,
            cancel,
        )
        .await
        {
            if let Ok(manifest) = manifest::decode(&encoded) {
                if manifest.manifest_digest == digest {
                    received = Some((manifest, encoded, index));
                    break;
                }
            }
        }
    }
    let (manifest, encoded, mut preferred) =
        received.ok_or("no verified transcode holder reachable")?;
    let recipe_version = holders[preferred].0.recipe_version;
    let bytes = manifest
        .objects
        .iter()
        .try_fold(encoded.len() as u64, |total, object| {
            total.checked_add(object.bytes)
        })
        .ok_or("copy size overflow")?;
    let budget = crate::cachekeep::budget_bytes_fallible(&state.store)
        .await
        .map_err(|error| error.to_string())?
        .ok_or("cache byte budget is disabled")?;
    let used = state
        .store
        .cache_bytes(node)
        .await
        .map_err(|error| error.to_string())?;
    if bytes > i64::MAX as u64 || used.saturating_add(bytes as i64) > budget {
        return Err("cache byte budget has no room for copy".into());
    }
    let name = format!("{recipe}-j{}-f{}", job.id, job.fence);
    let _staging = state
        .transcode
        .cache_readers()
        .begin_staging(&name)
        .ok_or("staging cleanup owns directory")?;
    let root_cap = SecureDirectory::open(root)
        .await
        .map_err(|error| error.to_string())?;
    let staging_parent = root_cap
        .create_child_directory(crate::cachekeep::STAGING)
        .await
        .map_err(|error| error.to_string())?;
    let staging = staging_parent
        .create_child_directory(&name)
        .await
        .map_err(|error| error.to_string())?;
    let transfer = async {
        for (index, object) in manifest.objects.iter().enumerate() {
            let path = format!("{PREFIX}{recipe}/{digest}/{index}");
            let mut received = None;
            // All candidates advertise the same immutable manifest. A corrupt
            // object on one holder must not hide a healthy copy on another.
            for offset in 0..holders.len() {
                if cancel.is_cancelled() {
                    return Ok(false);
                }
                let selected = (preferred + offset) % holders.len();
                let (source, base) = holders[selected];
                if let Ok(bytes) = fetch(
                    transport,
                    &source.node_id,
                    base,
                    &path,
                    object.bytes,
                    Some(&object.sha256),
                    deadline,
                    cancel,
                )
                .await
                {
                    preferred = selected;
                    received = Some(bytes);
                    break;
                }
            }
            let bytes = received.ok_or("no holder supplied a verified object")?;
            if cancel.is_cancelled() {
                return Ok(false);
            }
            staging
                .atomic_write_child(&object.name, &bytes)
                .await
                .map_err(|error| error.to_string())?;
        }
        staging
            .atomic_write_child(manifest::MANIFEST_FILE, &encoded)
            .await
            .map_err(|error| error.to_string())?;
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let relative = format!("{}/{name}", &recipe[..2]);
        let _publication = state
            .transcode
            .cache_readers()
            .begin_publication(recipe, &relative)
            .ok_or("cache cleanup owns final directory")?;
        root_cap
            .create_child_directory(&recipe[..2])
            .await
            .map_err(|error| error.to_string())?;
        staging_parent
            .rename_child_to(&name, &root.join(&recipe[..2]), &name)
            .await
            .map_err(|error| error.to_string())?;
        // Never delete a renamed generation after an ambiguous commit reply.
        // The ordinary ownership-aware orphan sweep resolves that case.
        if cancel.is_cancelled() {
            return Ok(false);
        }
        fence
            .publish_transcode(TranscodeJobOutput {
                recipe_hash: recipe.into(),
                manifest_digest: digest.into(),
                recipe_version,
                relative_dir: relative,
                bytes: bytes as i64,
                expected_previous_bytes: None,
            })
            .await
            .map_err(|error| error.to_string())
    }
    .await;
    if let Err(error) = staging_parent
        .remove_child_tree(&name, manifest::MAX_OBJECTS + 8, 1)
        .await
    {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%error, job = job.id, "could not remove copy staging");
        }
    }
    transfer
}

async fn pass(
    state: &AppState,
    transport: &PeerTransport,
    boot: &str,
    cursor: &mut Option<CandidateCursor>,
) -> Result<bool, StoreError> {
    let authority = state.jobs.execution_authority();
    if authority.may_run_cluster_jobs().await {
        state.jobs.enqueue_artifact_deliveries().await;
    }
    if state.transcode.cache_location().is_none()
        || !authority.may_execute_job(JobKind::ArtifactHydrate).await
    {
        return Ok(false);
    }
    let Some(admission) = state.transcode.admit_fragment().await else {
        return Ok(false);
    };
    let page = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![JobKind::ArtifactHydrate],
            after: cursor.take(),
            now_ms: now_ms(),
            limit: 128,
        })
        .await?;
    *cursor = page.next;
    for candidate in page.jobs {
        let Ok(JobPayload::ArtifactHydrate {
            artifact_key,
            target_node_id,
        }) = candidate.supported_payload()
        else {
            continue;
        };
        if parse_key(&artifact_key).is_none() || target_node_id != state.node_id {
            continue;
        }
        // Legacy metadata recovery and temporarily absent holders do not
        // consume this target's finite failure budget. Its demand deadline
        // still bounds how long it can remain pending.
        if state
            .store
            .transcode_copy_sources(&artifact_key)
            .await?
            .is_empty()
        {
            continue;
        }
        if !state.transcode.fragment_worker_idle(&admission) {
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
                kind: JobKind::ArtifactHydrate,
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
            state.store.clone(),
            authority.clone(),
            job.token
                .clone()
                .ok_or_else(|| StoreError::Task("copy claim missing token".into()))?,
            deadline,
            JobKind::ArtifactHydrate,
        )?;
        let fence = active.fence();
        let cancel = fence.loss_token().child_token();
        let result = {
            let work = prepare(state, transport, &job, &fence, &cancel);
            tokio::pin!(work);
            let mut pressure = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    result = &mut work => break result,
                    _ = pressure.tick() => { if !state.transcode.fragment_worker_idle(&admission) { cancel.cancel(); } }
                }
            }
        };
        if !matches!(result, Ok(true)) {
            if let Err(error) = result {
                tracing::warn!(job = job.id, %error, "transcode copy deferred");
            }
            let settlement = if cancel.is_cancelled() {
                JobSettlement::Yield {
                    checkpoint: None,
                    not_before_ms: now_ms().saturating_add(5_000),
                }
            } else {
                JobSettlement::Retry {
                    error_code: "transcode_copy_unavailable".into(),
                    not_before_ms: now_ms().saturating_add(crate::background_jobs::retry_delay_ms(
                        &job.id,
                        job.failed_attempts,
                    )),
                }
            };
            if let Err(error) = fence.settle(settlement).await {
                tracing::warn!(%error, "transcode copy settlement unavailable");
            }
        }
        active.finish().await;
        return Ok(true);
    }
    Ok(false)
}

pub(crate) async fn run(state: AppState) {
    let transport = PeerTransport::new(state.membership.clone());
    let boot = uuid::Uuid::new_v4().to_string();
    let mut cursor = None;
    let mut pacing = crate::background_jobs::IdlePoll::new();
    loop {
        let progressed = pass(&state, &transport, &boot, &mut cursor)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "transcode copy pass failed");
                false
            });
        tokio::time::sleep(pacing.delay(progressed)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manifest_body_holds_transfer_capacity_and_cache_pin_until_drop() {
        let readers = crate::cachekeep::ActiveCacheReaders::default();
        let global = Arc::new(Semaphore::new(1));
        let peer = Arc::new(Semaphore::new(1));
        let body = Body::from_stream(VerifiedStream {
            reader: futures_util::stream::iter([Ok(Bytes::from_static(b"manifest"))]),
            _snapshot: None,
            _cache: readers.begin_read("recipe").expect("pin"),
            _global: global.clone().try_acquire_owned().expect("global"),
            _peer: peer.clone().try_acquire_owned().expect("peer"),
        });
        let response = response(body, 8).expect("response");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "8");
        assert!(readers.begin_eviction("recipe").is_err());
        assert_eq!(global.available_permits(), 0);
        assert_eq!(peer.available_permits(), 0);
        drop(response);
        assert!(readers.begin_eviction("recipe").is_ok());
        assert_eq!(global.available_permits(), 1);
        assert_eq!(peer.available_permits(), 1);
    }

    #[tokio::test]
    async fn copies_reject_household_bearers_before_reading_cache() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer household-token".parse().expect("bearer header"),
        );
        let result = serve(
            State(state),
            Path(("a".repeat(64), "b".repeat(64), "manifest".into())),
            headers,
        )
        .await;
        assert_eq!(
            result.expect_err("unsigned cache copy"),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn peer_copy_bodies_reject_tampering_overflow_and_stalled_cancellation() {
        use axum::{routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/good", get(|| async { "hello" }))
                    .route("/wrong", get(|| async { "jello" }))
                    .route(
                        "/long",
                        get(|| async {
                            Body::from_stream(futures_util::stream::iter([
                                Ok::<_, std::io::Error>(Bytes::from_static(b"hello")),
                                Ok(Bytes::from_static(b"!")),
                            ]))
                        }),
                    )
                    .route(
                        "/stalled",
                        get(|| async {
                            Body::from_stream(futures_util::stream::pending::<
                                Result<Bytes, std::io::Error>,
                            >())
                        }),
                    ),
            )
            .await
            .expect("server");
        });
        let client = reqwest::Client::new();
        let digest = hex::encode(Sha256::digest(b"hello"));
        let cancel = CancellationToken::new();
        for (route, valid) in [("good", true), ("wrong", false), ("long", false)] {
            let response = client
                .get(format!("http://{address}/{route}"))
                .send()
                .await
                .expect("response");
            let result = receive_body(
                response,
                5,
                Some(&digest),
                Instant::now() + Duration::from_secs(2),
                &cancel,
            )
            .await;
            assert_eq!(result.is_ok(), valid, "{route}: {result:?}");
        }
        let response = client
            .get(format!("http://{address}/stalled"))
            .send()
            .await
            .expect("stalled headers");
        let receiving = receive_body(
            response,
            5,
            Some(&digest),
            Instant::now() + Duration::from_secs(60),
            &cancel,
        );
        tokio::pin!(receiving);
        assert!(futures_util::poll!(&mut receiving).is_pending());
        cancel.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(1), receiving)
            .await
            .expect("joined cancellation")
            .is_err());
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn copies_only_accept_bounded_manifest_addressed_objects() {
        let prefix = format!("{PREFIX}{}/{}/", "a".repeat(64), "b".repeat(64));
        assert!(route_eligible(&format!("{prefix}manifest")));
        assert!(route_eligible(&format!("{prefix}0")));
        assert!(route_eligible(&format!(
            "{prefix}{}",
            manifest::MAX_OBJECTS - 1
        )));
        for object in [
            "../manifest",
            "-1",
            "8192",
            "manifest/extra",
            "index.m3u8",
            "",
        ] {
            assert!(!route_eligible(&format!("{prefix}{object}")), "{object}");
        }
        assert!(!route_eligible(&format!(
            "{PREFIX}invalid/{}/manifest",
            "b".repeat(64)
        )));
    }
}
