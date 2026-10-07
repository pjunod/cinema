//! Serve and reconcile node-local artwork.
//!
//! Item rows replicate through Raft, but artwork bytes deliberately do not.
//! Every voter therefore pulls missing files from another live voter and
//! atomically materializes them. A short-lived HMAC proves that the caller is
//! an admitted node; reusable user tokens never cross between peers.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path as FsPath;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::{ArtworkPeerAuth, MembershipManager};
use plurx_core::domain::Item;
use plurx_core::error::StoreError;
use plurx_core::store::ArtworkInventoryItem;
#[cfg(test)]
use plurx_core::store::Store;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::error::ApiError;
use super::extract::AuthUser;
use crate::state::AppState;
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
#[path = "images_worker.rs"]
mod worker;
pub(crate) use worker::run as durable_artwork_loop;

const NODE_ID_HEADER: &str = "x-plurx-node-id";
const TIMESTAMP_HEADER: &str = "x-plurx-artwork-time";
const SIGNATURE_HEADER: &str = "x-plurx-artwork-signature";
const MAX_ARTWORK_BYTES: u64 = plurx_core::metadata::MAX_ARTWORK_BYTES;
const PEER_FETCH_CONCURRENCY: usize = 8;
const LOCAL_READ_BUDGET_KIB: usize = 65_536;
const LOCAL_READ_ADMISSION_WAIT: Duration = Duration::from_millis(250);
const VERIFIED_ARTWORK_CACHE: usize = 4_096;
const VERIFIED_ARTWORK_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const DERIVATIVE_CONCURRENCY: usize = 2;
const DERIVATIVE_ADMISSION_WAIT: Duration = Duration::from_millis(500);
const DERIVATIVE_TIMEOUT: Duration = Duration::from_secs(5);
const PEER_RACE_CONCURRENCY: usize = 3;
const PEER_RACE_DEADLINE: Duration = Duration::from_millis(3_250);
const SOURCE_REPAIRS_PER_PASS: usize = 4;
const SOURCE_REPAIR_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);
const SOURCE_REPAIR_LEASE: Duration = Duration::from_secs(5 * 60);
const MATERIALIZE_INTERVAL: Duration = Duration::from_secs(60);
const SOURCE_REPAIR_GRACE: Duration = Duration::from_secs(2 * 60);
const ARTWORK_FETCH_TOTAL_TIMEOUT: Duration = Duration::from_secs(3);
const MATERIALIZE_ITEM_PAGE: i64 = 256;
const CONTENT_ORPHAN_GRACE: Duration = Duration::from_secs(24 * 60 * 60);
const ORPHAN_SCAN_LIMIT: usize = 10_000;
const ORPHAN_REMOVE_LIMIT: usize = 256;
/// The shortest verified-source digest prefix every source family can supply.
///
/// Derivative *names* carry the first 32 hex characters of the source digest,
/// but `{item}-{kind}-c{16hex}` sources — every backdrop — publish only
/// sixteen. Comparing 32 against 16 matches nothing, so the orphan sweep
/// narrows both sides to what both families have. Sixteen hex characters is
/// 64 bits: a collision retains a dead derivative, which costs disk, and can
/// never delete a live one, because a derivative's prefix is by construction
/// the digest of the source it was made from.
const DERIVATIVE_PREFIX_MATCH: usize = 16;
/// Node-local derivative store, a child of the served artwork root.
const DERIVED_DIR: &str = "derived";
/// Originals and derivatives are both immutable under their own URL: an
/// original filename is content-addressed or carries `?v={updated_at}`, and a
/// derivative key is the verified source digest.
const ARTWORK_CACHE_CONTROL: &str = "private, max-age=604800, immutable";

/// Exact opened-byte snapshot for sharing. Internal paths/identities have no
/// diagnostic or wire implementation; B receives only digest/length/MIME.
pub(super) struct SharedArtAsset {
    pub(super) bytes: axum::body::Bytes,
    pub(super) digest: [u8; 32],
    pub(super) mime: &'static str,
    path: std::path::PathBuf,
    identity: ArtworkFileIdentity,
    _local_bytes: OwnedSemaphorePermit,
    _request_owner: Arc<super::shared_artwork::ArtBodyLease>,
}
impl SharedArtAsset {
    pub(super) async fn still_current(&self) -> bool {
        open_local_artwork(self.path.clone())
            .await
            .is_some_and(|opened| opened.identity == self.identity)
    }
}
fn shared_art_mime(filename: &str, bytes: &[u8]) -> Option<&'static str> {
    let extension = FsPath::new(filename)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some("image/png"),
        "jpg" | "jpeg" if bytes.starts_with(&[0xff, 0xd8]) => Some("image/jpeg"),
        "gif" if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => Some("image/gif"),
        "webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
            Some("image/webp")
        }
        _ => None,
    }
}
async fn shared_opened_art(
    state: &AppState,
    path: std::path::PathBuf,
    name: &str,
    lease: Arc<super::shared_artwork::ArtBodyLease>,
) -> Result<Option<SharedArtAsset>, ApiError> {
    let Some(opened) = open_local_artwork(path.clone()).await else {
        return Ok(None);
    };
    let identity = opened.identity;
    let Some(local) = state.artwork_fetch.local_permit(identity.bytes + 1).await else {
        return Err(artwork_capacity_error());
    };
    let Some((bytes, digest, local, lease)) = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        // Cancellation cannot release either budget while this descriptor read
        // still owns allocation/work. The existing worker settles its CPU own.
        let request_owner = lease;
        let mut file = opened.file;
        let mut bytes = Vec::with_capacity(identity.bytes as usize + 1);
        (&mut file)
            .take(identity.bytes + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 != identity.bytes
            || artwork_file_identity(&file.metadata().ok()?) != identity
        {
            return None;
        }
        let digest = Sha256::digest(&bytes).into();
        Some((axum::body::Bytes::from(bytes), digest, local, request_owner))
    })
    .await
    .ok()
    .flatten() else {
        return Ok(None);
    };
    let Some(mime) = shared_art_mime(name, &bytes) else {
        return Ok(None);
    };
    if !artwork_digest_matches_name(name, digest) {
        return Ok(None);
    }
    Ok(Some(SharedArtAsset {
        bytes,
        digest,
        mime,
        path,
        identity,
        _local_bytes: local,
        _request_owner: lease,
    }))
}
struct SharedArtDownloadOwner {
    _lease: Arc<super::shared_artwork::ArtBodyLease>,
    _local: Option<tokio::sync::OwnedSemaphorePermit>,
    _peer: Option<tokio::sync::OwnedSemaphorePermit>,
    _workspace: tokio::sync::OwnedSemaphorePermit,
}
struct SharedArtDownload {
    bytes: bytes::Bytes,
    _owner: Arc<SharedArtDownloadOwner>,
}
impl AsRef<[u8]> for SharedArtDownload {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
/// One collector at a time, preallocated to its charged object/sentinel cap.
/// Only trusted current-cluster artwork URLs and node proofs reach this helper.
async fn shared_peer_snapshot(
    client: &reqwest::Client,
    url: reqwest::Url,
    auth: &ArtworkPeerAuth,
    name: &str,
    owner: Arc<SharedArtDownloadOwner>,
) -> Option<SharedArtDownload> {
    let response = client
        .get(url)
        .header(NODE_ID_HEADER, &auth.node_id)
        .header(TIMESTAMP_HEADER, auth.timestamp_ms)
        .header(SIGNATURE_HEADER, &auth.signature)
        .send()
        .await
        .ok()?;
    if response.status() != reqwest::StatusCode::OK
        || response
            .content_length()
            .is_some_and(|len| len == 0 || len > MAX_ARTWORK_BYTES)
        || !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| matches!(v, "image/png" | "image/jpeg" | "image/gif" | "image/webp"))
    {
        return None;
    }
    let mut body = response.bytes_stream();
    let mut bytes = Vec::with_capacity(MAX_ARTWORK_BYTES as usize + 1);
    let mut digest = Sha256::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.ok()?;
        if chunk.len() > MAX_ARTWORK_BYTES as usize - bytes.len() {
            return None;
        }
        digest.update(&chunk);
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty()
        || shared_art_mime(name, &bytes).is_none()
        || !artwork_digest_matches_name(name, digest.finalize().into())
    {
        return None;
    }
    Some(SharedArtDownload {
        bytes: bytes.into(),
        _owner: owner,
    })
}
async fn shared_materialize_original(
    state: &AppState,
    selection: &plurx_core::store::sharing_catalogue_artwork::SourceArtSnapshot,
    credential_hash: &str,
    name: &str,
    lease: Arc<super::shared_artwork::ArtBodyLease>,
) -> Result<bool, ApiError> {
    let Some(peer_permit) = state.artwork_fetch.peer_permit() else {
        return Err(artwork_capacity_error());
    };
    let Some(local_permit) = state
        .artwork_fetch
        .local_permit(MAX_ARTWORK_BYTES + 1)
        .await
    else {
        return Err(artwork_capacity_error());
    };
    // Charge a second object workspace for transport buffers independently of
    // the collector. HTTP/1 keeps the pinned Hyper adaptive receive buffer
    // bounded; no multiplexed H2 receive windows are admitted by this client.
    let workspace = super::shared_artwork::ArtBodyLease::source_collection_workspace()?;
    let client = reqwest::Client::builder()
        .http1_only()
        .connect_timeout(Duration::from_millis(750))
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| artwork_capacity_error())?;
    let peers = state
        .membership
        .reachable_peer_http_urls()
        .await
        .unwrap_or_default();
    let Ok(auth) = state.membership.artwork_peer_auth(name) else {
        return Ok(false);
    };
    let owner = Arc::new(SharedArtDownloadOwner {
        _lease: lease,
        _local: Some(local_permit),
        _peer: Some(peer_permit),
        _workspace: workspace,
    });
    tokio::time::timeout(Duration::from_secs(2),async {
        // Three roster destinations total, sequential. The ordinary three-peer
        // racer is deliberately unchanged and never consumes this reservation.
        for peer in peers.into_iter().take(3) {
            let Some(url)=peer_artwork_url(&peer,name) else {continue;};
            let Some(download)=shared_peer_snapshot(&client,url,&auth,name,owner.clone()).await else {continue;};
            let current=state.store.source_art_snapshot(credential_hash,selection.reference().grant_id,selection.reference()).await.map_err(super::error::ApiError::from)?;
            if !matches!(current,plurx_core::store::sharing_catalogue_artwork::SourceArtRead::Authorized(ref value) if selection.same_selection(value))
                || !super::shared_library::source_art_scope_current(state,selection.reference()).await {return Ok(false);}
            // Move the collector allocation and Source reservation directly into
            // the held-directory blocking writer. No second bitmap allocation.
            if plurx_core::fs_secure::atomic_write_child_owned(&state.artwork_dir,name,download).await.is_ok() {return Ok(true);}
            return Ok(false);
        }
        Ok(false)
    }).await.unwrap_or(Ok(false))
}
pub(super) async fn shared_artwork_asset(
    state: &AppState,
    selection: &plurx_core::store::sharing_catalogue_artwork::SourceArtSnapshot,
    credential_hash: &str,
    lease: Arc<super::shared_artwork::ArtBodyLease>,
) -> Result<Option<SharedArtAsset>, ApiError> {
    let name = safe_artwork_name(selection.filename())?;
    let variant = selection.reference().variant;
    let path = state.artwork_dir.join(name);
    let mut original = shared_opened_art(state, path.clone(), name, lease.clone()).await?;
    if original.is_none() {
        if !shared_materialize_original(state, selection, credential_hash, name, lease.clone())
            .await?
        {
            return Ok(None);
        }
        original = shared_opened_art(state, path, name, lease.clone()).await?;
    }
    let Some(original) = original else {
        return Ok(None);
    };
    use plurx_core::sharing_artwork::ArtVariant;
    let size = match variant {
        ArtVariant::Original => return Ok(Some(original)),
        ArtVariant::W300 => ArtworkSize::W300,
        ArtVariant::W500 => ArtworkSize::W500,
        ArtVariant::W780 => ArtworkSize::W780,
    };
    let Some(spec) = worker::spec(original.digest, size, name).await else {
        return Ok(None);
    };
    let Some(location) = worker::shared_local_variant(state, &spec).await else {
        // No render demand follows an authority refusal. The worker's own
        // canonical job/CPU/storage settlement remains independent of this
        // request's later cancellation.
        let current = tokio::time::timeout(
            Duration::from_secs(1),
            state.store.source_art_snapshot(
                credential_hash,
                selection.reference().grant_id,
                selection.reference(),
            ),
        )
        .await
        .ok()
        .and_then(Result::ok);
        if !matches!(current,Some(plurx_core::store::sharing_catalogue_artwork::SourceArtRead::Authorized(ref snapshot)) if selection.same_selection(snapshot))
            || !original.still_current().await
            || !super::shared_library::source_art_scope_current(state, selection.reference()).await
        {
            return Ok(None);
        }
        worker::request(state, spec).await;
        return Ok(None); // The canonical job owns settlement; no original fallback.
    };
    let Some(variant_name) = worker::location_filename(&location) else {
        return Ok(None);
    };
    let Some(asset) = shared_opened_art(
        state,
        state.artwork_dir.join(DERIVED_DIR).join(&variant_name),
        &variant_name,
        lease,
    )
    .await?
    else {
        return Ok(None);
    };
    if asset.bytes.len() as i64 != location.bytes
        || hex::encode(asset.digest) != location.blob_sha256
        || !image_dimensions(&variant_name, &asset.bytes)
            .is_some_and(|(width, height)| width > 0 && height > 0 && Some(width) <= size.width())
    {
        return Ok(None);
    }
    drop(original);
    Ok(Some(asset))
}
/// A fallback answers a `?size=` URL with the original bytes, which is not the
/// resource that URL names. `immutable` would freeze that substitution in every
/// client cache for a week, and a cold grid load falls back on nearly every
/// request — two ffmpeg slots behind a 500 ms admission wait — so the
/// derivative would be generated on the server and then never asked for again.
/// `max-age=0, must-revalidate` keeps the substitution to this one response:
/// the `ETag` is the original's digest, so the next request either revalidates
/// to the same original or collects the derivative that now exists.
const FALLBACK_CACHE_CONTROL: &str = "private, max-age=0, must-revalidate";

/// Shared process-wide bounds for on-demand and background peer fetches.
///
/// The keyed mutex collapses simultaneous misses for one filename; the global
/// semaphore bounds distinct misses so an authenticated card-grid burst cannot
/// allocate one 15 MiB response buffer per request without limit.
#[derive(Debug)]
pub(crate) struct ArtworkCoordinator {
    client: Result<reqwest::Client, String>,
    peer_permits: Arc<Semaphore>,
    local_bytes: Arc<Semaphore>,
    filenames: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    verified: Mutex<VerifiedArtworkCache>,
    derive_permits: Arc<Semaphore>,
    variants: Mutex<HashMap<String, worker::CachedLocation>>,
    demands: Mutex<HashMap<String, Instant>>,
    jellyfin_budget: Mutex<JellyfinArtworkBudget>,
    jellyfin_demands: Mutex<HashMap<String, JellyfinArtworkDemand>>,
    wake: tokio::sync::Notify,
    #[cfg(test)]
    hashes: AtomicU64,
    /// Committed derivative generations. A single-flight test needs to count
    /// the ffmpeg runs that actually produced a file, which no metric label
    /// distinguishes from the ones a concurrent request waited for.
    #[cfg(test)]
    derivations: AtomicU64,
}

impl ArtworkCoordinator {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            client: artwork_client().map_err(|error| error.to_string()),
            peer_permits: Arc::new(Semaphore::new(PEER_FETCH_CONCURRENCY)),
            local_bytes: Arc::new(Semaphore::new(LOCAL_READ_BUDGET_KIB)),
            filenames: Mutex::new(HashMap::new()),
            verified: Mutex::new(VerifiedArtworkCache::default()),
            derive_permits: Arc::new(Semaphore::new(DERIVATIVE_CONCURRENCY)),
            variants: Mutex::new(HashMap::new()),
            demands: Mutex::new(HashMap::new()),
            jellyfin_budget: Mutex::new(JellyfinArtworkBudget::default()),
            jellyfin_demands: Mutex::new(HashMap::new()),
            wake: tokio::sync::Notify::new(),
            #[cfg(test)]
            hashes: AtomicU64::new(0),
            #[cfg(test)]
            derivations: AtomicU64::new(0),
        })
    }

    async fn filename(&self, filename: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.filenames.lock().await;
            // Weak entries exist only to let overlapping requests find each
            // other. Prune completed keys on every miss so arbitrary valid
            // filenames cannot turn the singleflight index into an unbounded
            // process-lifetime cache.
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(filename).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(filename.to_owned(), Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
    }

    fn peer_permit(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.peer_permits).try_acquire_owned().ok()
    }

    async fn local_permit(&self, bytes: u64) -> Option<OwnedSemaphorePermit> {
        let kib = bytes.div_ceil(1024);
        let permits = u32::try_from(kib).ok()?;
        tokio::time::timeout(
            LOCAL_READ_ADMISSION_WAIT,
            Arc::clone(&self.local_bytes).acquire_many_owned(permits),
        )
        .await
        .ok()?
        .ok()
    }

    async fn verified_digest(
        &self,
        filename: &str,
        identity: ArtworkFileIdentity,
    ) -> Option<[u8; 32]> {
        let mut verified = self.verified.lock().await;
        match verified.get(filename, identity, Instant::now()) {
            VerifiedLookup::Hit(digest) => Some(digest),
            VerifiedLookup::Invalidated => {
                record_verification(ArtworkVerification::Invalidated);
                None
            }
            VerifiedLookup::Miss => None,
        }
    }

    async fn remember(&self, filename: &str, identity: ArtworkFileIdentity, digest: [u8; 32]) {
        self.verified.lock().await.insert(
            filename.to_owned(),
            VerifiedArtwork {
                identity,
                digest,
                verified_at: Instant::now(),
            },
        );
    }

    async fn forget(&self, filename: &str) {
        if self.verified.lock().await.remove(filename) {
            record_verification(ArtworkVerification::Invalidated);
        }
    }

    async fn derive_permit(&self) -> Option<OwnedSemaphorePermit> {
        // Granted, timed out, or abandoned because the request went away
        // while it queued: the wait happened, and the timer records it when
        // it drops, which is on every one of those paths.
        let _timer = crate::telemetry::AdmissionWaitTimer::start(
            crate::telemetry::AdmissionPool::ImageMaterialize,
        );
        tokio::time::timeout(
            DERIVATIVE_ADMISSION_WAIT,
            Arc::clone(&self.derive_permits).acquire_owned(),
        )
        .await
        .ok()?
        .ok()
    }

    async fn verified_snapshot(&self) -> BTreeMap<String, [u8; 32]> {
        self.verified
            .lock()
            .await
            .entries
            .iter()
            .map(|(filename, entry)| (filename.clone(), entry.digest))
            .collect()
    }

    fn client(&self) -> Option<reqwest::Client> {
        self.client.as_ref().ok().cloned()
    }
}

#[derive(Clone, Copy, Debug)]
struct VerifiedArtwork {
    identity: ArtworkFileIdentity,
    digest: [u8; 32],
    verified_at: Instant,
}

#[derive(Debug, Default)]
struct VerifiedArtworkCache {
    entries: BTreeMap<String, VerifiedArtwork>,
    order: VecDeque<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VerifiedLookup {
    Hit([u8; 32]),
    Invalidated,
    Miss,
}

impl VerifiedArtworkCache {
    fn get(
        &mut self,
        filename: &str,
        identity: ArtworkFileIdentity,
        now: Instant,
    ) -> VerifiedLookup {
        let Some(entry) = self.entries.get(filename).copied() else {
            return VerifiedLookup::Miss;
        };
        if entry.identity != identity
            || now.saturating_duration_since(entry.verified_at) >= VERIFIED_ARTWORK_TTL
        {
            self.remove(filename);
            return VerifiedLookup::Invalidated;
        }
        self.touch(filename);
        VerifiedLookup::Hit(entry.digest)
    }

    fn insert(&mut self, filename: String, entry: VerifiedArtwork) {
        self.entries.insert(filename.clone(), entry);
        self.touch(&filename);
        while self.entries.len() > VERIFIED_ARTWORK_CACHE {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn remove(&mut self, filename: &str) -> bool {
        self.order.retain(|key| key != filename);
        self.entries.remove(filename).is_some()
    }

    fn touch(&mut self, filename: &str) {
        self.order.retain(|key| key != filename);
        self.order.push_back(filename.to_owned());
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ArtworkQuery {
    size: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtworkSize {
    Original,
    W300,
    W500,
    W780,
}

impl ArtworkSize {
    fn parse(value: Option<&str>) -> Result<Self, ApiError> {
        match value {
            None | Some("original") => Ok(Self::Original),
            Some("w300") => Ok(Self::W300),
            Some("w500") => Ok(Self::W500),
            Some("w780") => Ok(Self::W780),
            Some(_) => Err(ApiError::BadRequest(
                "size must be original, w300, w500, or w780".into(),
            )),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::W300 => "w300",
            Self::W500 => "w500",
            Self::W780 => "w780",
        }
    }

    fn width(self) -> Option<u32> {
        match self {
            Self::Original => None,
            Self::W300 => Some(300),
            Self::W500 => Some(500),
            Self::W780 => Some(780),
        }
    }
}

/// GET /api/v1/images/:filename
pub async fn serve(
    _user: AuthUser,
    State(state): State<AppState>,
    Path(filename): Path<String>,
    Query(query): Query<ArtworkQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    match ArtworkSize::parse(query.size.as_deref())? {
        ArtworkSize::Original => {
            serve_cluster_artwork_with_headers(&state, &filename, &headers, ArtworkRoute::User)
                .await
        }
        size => serve_derivative(&state, &filename, &headers, size).await,
    }
}

/// GET /api/v1/cluster/artwork/:filename
///
/// This route is intentionally not user-authenticated. Its credential is a
/// one-minute, filename-bound cluster proof, and verification also confirms
/// that the signing node is still a reachable non-tombstoned member.
pub async fn serve_peer(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    Query(query): Query<ArtworkQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let safe_name = safe_artwork_name(&filename)?;
    let auth = peer_auth_from_headers(&headers).ok_or(ApiError::Unauthorized)?;
    let verified = state
        .membership
        .verify_artwork_peer_auth(safe_name, &auth)
        .await
        .map_err(|error| {
            tracing::warn!(code = error.code(), "cannot verify artwork peer proof");
            ApiError::Unauthorized
        })?;
    if !verified {
        return Err(ApiError::Unauthorized);
    }
    serve_verified_peer_artwork(&state, safe_name, query.size.as_deref(), &headers).await
}

/// Everything the peer route does once its proof has been checked.
///
/// Split out so the refusal below is reachable without a live replicated
/// membership: `verify_artwork_peer_auth` needs a consistent cluster read, so
/// no unit test can get past the extractor above, and the rule that peers never
/// serve derivatives would otherwise have no test at all.
async fn serve_verified_peer_artwork(
    state: &AppState,
    safe_name: &str,
    size: Option<&str>,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    // Peers replicate originals and derive locally. Serving a bucket here would
    // let one node's resize become another node's source of record, and the
    // puller authenticates bytes against the original's name.
    if size.is_some() {
        return Err(ApiError::BadRequest(
            "cluster artwork does not serve derivatives".into(),
        ));
    }
    if let Some(key) = safe_name.strip_prefix("variant-") {
        return worker::serve_peer_variant(state, key, headers).await;
    }
    serve_local_artwork(
        &state.artwork_fetch,
        &state.artwork_dir,
        safe_name,
        headers,
        ArtworkRoute::Peer,
    )
    .await
}

/// Serve local artwork, or retrieve it from a reachable voter and materialize
/// it locally. The peer-only route reads local bytes and never recurses, so a
/// filename absent everywhere remains a bounded 404 instead of cycling.
pub(crate) async fn serve_cluster_artwork(
    state: &AppState,
    filename: &str,
) -> Result<Response, ApiError> {
    serve_cluster_artwork_with_headers(state, filename, &HeaderMap::new(), ArtworkRoute::User).await
}

pub(crate) async fn serve_plex_artwork(
    state: &AppState,
    filename: &str,
) -> Result<Response, ApiError> {
    serve_cluster_artwork_with_headers(state, filename, &HeaderMap::new(), ArtworkRoute::Plex).await
}

async fn serve_cluster_artwork_with_headers(
    state: &AppState,
    filename: &str,
    headers: &HeaderMap,
    route: ArtworkRoute,
) -> Result<Response, ApiError> {
    let safe_name = safe_artwork_name(filename)?;
    match serve_local_artwork(
        &state.artwork_fetch,
        &state.artwork_dir,
        safe_name,
        headers,
        route,
    )
    .await
    {
        Ok(response) => return Ok(response),
        Err(error) if is_artwork_capacity_error(&error) => {
            record_request(route, ArtworkOutcome::Capacity);
            return Err(error);
        }
        Err(_) => {}
    }

    let membership = state.membership.clone();
    let Some(bytes) = fetch_and_materialize(
        &state.artwork_fetch,
        &state.artwork_dir,
        safe_name,
        move |client| async move {
            let peers = match membership.reachable_peer_http_urls().await {
                Ok(peers) => peers,
                Err(error) => {
                    tracing::warn!(code = error.code(), "cannot read artwork peer roster");
                    return None;
                }
            };
            fetch_peer_artwork(&client, &membership, &peers, safe_name).await
        },
    )
    .await
    .map_err(|_| artwork_capacity_error())?
    else {
        record_request(route, ArtworkOutcome::Miss);
        return Err(ApiError::NotFound("image"));
    };
    record_request(route, ArtworkOutcome::Peer);
    Ok(admitted_artwork_response(
        &state.artwork_dir.join(safe_name),
        bytes,
        ARTWORK_CACHE_CONTROL,
        &[],
    ))
}

async fn serve_derivative(
    state: &AppState,
    filename: &str,
    headers: &HeaderMap,
    size: ArtworkSize,
) -> Result<Response, ApiError> {
    let safe_name = safe_artwork_name(filename)?;
    let source_path = state.artwork_dir.join(safe_name);

    // Warm path (plan objective 3, "no bytes read, no permit taken"). The
    // derived key is the source's *verified* digest, and M1 already holds that
    // against the source's file identity — so one `open` plus `fstat` names the
    // derivative without reading the source at all. Resolving it here is what
    // makes a `?size=` hit cheaper than the original it shrinks: reading the
    // source first meant a warm `?size=w300` on a 4 MiB backdrop did 4 MiB of
    // I/O, allocated a 4 MiB buffer and drew 4 MiB of the 64 MiB byte budget
    // to answer with ~20 KiB, or with a 304 and no body at all.
    if let Some(opened) = open_local_artwork(source_path.clone()).await {
        if let Some(digest) = state
            .artwork_fetch
            .verified_digest(safe_name, opened.identity)
            .await
        {
            if let Some(spec) = worker::spec(digest, size, safe_name).await {
                if let Some(response) = worker::serve_local_variant(state, &spec, headers).await? {
                    record_derivative(size, DerivativeOutcome::Served);
                    return Ok(with_response_header(response, header::VARY, "Accept"));
                }
            }
        }
    }

    let source =
        match read_verified_local_artwork(&state.artwork_fetch, source_path.clone(), safe_name)
            .await
        {
            LocalArtworkRead::Verified(bytes) => bytes,
            LocalArtworkRead::Corrupt(identity) => {
                quarantine_corrupt_artwork(
                    &state.artwork_fetch,
                    &state.artwork_dir,
                    safe_name,
                    identity,
                )
                .await;
                record_request(ArtworkRoute::User, ArtworkOutcome::Corrupt);
                return Err(ApiError::NotFound("image"));
            }
            LocalArtworkRead::Capacity => {
                record_request(ArtworkRoute::User, ArtworkOutcome::Capacity);
                return Err(artwork_capacity_error());
            }
            LocalArtworkRead::Missing => {
                let membership = state.membership.clone();
                match fetch_and_materialize(
                    &state.artwork_fetch,
                    &state.artwork_dir,
                    safe_name,
                    move |client| async move {
                        let peers = membership.reachable_peer_http_urls().await.ok()?;
                        fetch_peer_artwork(&client, &membership, &peers, safe_name).await
                    },
                )
                .await
                {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => {
                        record_request(ArtworkRoute::User, ArtworkOutcome::Miss);
                        return Err(ApiError::NotFound("image"));
                    }
                    Err(ArtworkCapacity) => {
                        record_request(ArtworkRoute::User, ArtworkOutcome::Capacity);
                        return Err(artwork_capacity_error());
                    }
                }
            }
        };

    if should_serve_original(size, safe_name, &source.bytes) {
        // Not a fallback: the source is already no wider than the bucket, so
        // these bytes are the permanently correct answer for this URL and keep
        // the immutable validator. Only a substitution forced by load or a
        // failed resize revalidates (see `derivative_fallback`).
        record_request(ArtworkRoute::User, ArtworkOutcome::Hit);
        record_derivative(size, DerivativeOutcome::Served);
        return Ok(admitted_artwork_response(
            &source_path,
            source,
            ARTWORK_CACHE_CONTROL,
            &[("vary", "Accept")],
        ));
    }

    let Some(spec) = worker::spec(source.digest, size, safe_name).await else {
        record_derivative(size, DerivativeOutcome::Refused);
        return Err(ApiError::BadRequest("unsupported image extension".into()));
    };
    if let Some(response) = worker::serve_local_variant(state, &spec, headers).await? {
        record_derivative(size, DerivativeOutcome::Served);
        return Ok(with_response_header(response, header::VARY, "Accept"));
    }
    worker::request(state, spec).await;
    Ok(derivative_fallback(&source_path, source, size))
}

fn derivative_fallback(
    source_path: &FsPath,
    source: AdmittedArtworkBytes,
    size: ArtworkSize,
) -> Response {
    record_request(ArtworkRoute::User, ArtworkOutcome::Hit);
    record_derivative(size, DerivativeOutcome::FallbackOriginal);
    admitted_artwork_response(
        source_path,
        source,
        FALLBACK_CACHE_CONTROL,
        &[("vary", "Accept"), ("x-plurx-artwork", "original-fallback")],
    )
}

fn with_response_header(
    mut response: Response,
    name: axum::http::HeaderName,
    value: &'static str,
) -> Response {
    response
        .headers_mut()
        .insert(name, HeaderValue::from_static(value));
    response
}

/// `{source}-{bucket}-{digest32}.{ext}`.
///
/// The digest is what makes the key correct — a source whose bytes change mints
/// a new one and nothing is ever served under a stale key. The *source
/// filename* is what makes cleanup correct: it lets the orphan sweep ask "is
/// this derivative's own source still referenced?" per derivative, instead of
/// needing every referenced source's digest resolved before it may delete
/// anything. `source` has already passed `safe_artwork_name`, so it contributes
/// no path separator.
fn derivative_filename(digest: [u8; 32], size: ArtworkSize, source: &str) -> Option<String> {
    let extension = FsPath::new(source)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    let extension = match extension.as_str() {
        "gif" => "png",
        "jpg" | "jpeg" | "png" | "webp" => extension.as_str(),
        _ => return None,
    };
    Some(format!(
        "{source}-{}-{}.{extension}",
        size.label(),
        &hex::encode(digest)[..32],
    ))
}

async fn ensure_derived_dir(artwork_dir: &FsPath) -> Result<std::path::PathBuf, std::io::Error> {
    let path = artwork_dir.join(DERIVED_DIR);
    match tokio::fs::create_dir(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = tokio::fs::symlink_metadata(&path).await?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "artwork derivative path is not a real directory",
        ));
    }
    Ok(path)
}

#[derive(Clone, Copy)]
struct DerivativeJob<'a> {
    /// Disposable ffmpeg bookkeeping. Never the served artwork root: that one
    /// is enumerated by the orphan walker, and library-created cache entries
    /// do not belong inside it.
    runtime_cache: &'a FsPath,
    source_path: &'a FsPath,
    source_name: &'a str,
    source_identity: ArtworkFileIdentity,
    derived_dir: &'a FsPath,
    derived_name: &'a str,
    size: ArtworkSize,
}

#[cfg(test)]
async fn generate_derivative(
    coordinator: &ArtworkCoordinator,
    job: DerivativeJob<'_>,
) -> Result<(), String> {
    generate_derivative_with_bin(coordinator, job, &crate::ffmpeg::ffmpeg_bin()).await
}

#[cfg(test)]
async fn generate_derivative_with_bin(
    coordinator: &ArtworkCoordinator,
    job: DerivativeJob<'_>,
    ffmpeg_bin: &str,
) -> Result<(), String> {
    generate_derivative_cancellable(coordinator, job, ffmpeg_bin, None).await
}

async fn generate_derivative_cancellable(
    coordinator: &ArtworkCoordinator,
    job: DerivativeJob<'_>,
    ffmpeg_bin: &str,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), String> {
    let DerivativeJob {
        runtime_cache,
        source_path,
        source_name,
        source_identity,
        derived_dir,
        derived_name,
        size,
    } = job;
    let extension = FsPath::new(derived_name)
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "derivative has no extension".to_owned())?;
    let codec = match extension {
        "jpg" | "jpeg" => "mjpeg",
        "png" => "png",
        "webp" => "libwebp",
        _ => return Err("unsupported derivative extension".to_owned()),
    };
    let temporary = format!(".{derived_name}.{}.tmp", uuid::Uuid::new_v4().simple());
    let temporary_path = derived_dir.join(&temporary);
    let command = derivative_command(
        ffmpeg_bin,
        source_path,
        runtime_cache,
        size,
        extension,
        codec,
    );
    let generated = crate::ffmpeg::bounded_command_output_cancellable(
        command,
        DERIVATIVE_TIMEOUT,
        MAX_ARTWORK_BYTES,
        "artwork derivative",
        cancel,
        crate::process_control::ChildWork::background("artwork derivative"),
    )
    .await?;
    if cancel.is_some_and(|token| token.is_cancelled()) {
        return Err("artwork derivative cancelled".into());
    }
    plurx_core::fs_secure::atomic_write_child(derived_dir, &temporary, &generated.stdout)
        .await
        .map_err(|e| e.to_string())?;

    // Fence the rename against the identity of the file whose bytes named this
    // derivative — not against a second read of it. The caller still holds its
    // admitted read, so a re-read would draw the byte budget twice for one
    // file, and a `Capacity` refusal from that second read is indistinguishable
    // from a real change: it discarded a correct derivative, logged a false
    // diagnosis, and left the next request to spend another five seconds of
    // ffmpeg on the same work. One `open` plus `fstat` is enough, because M1's
    // identity is exactly what any replacement moves — `install_artwork` writes
    // temp+rename so a new inode, and an in-place write moves ctime. A source
    // that has gone away counts as changed.
    let source_unchanged = match open_local_artwork(source_path.to_path_buf()).await {
        Some(opened) => opened.identity == source_identity,
        None => false,
    };
    if !source_unchanged || cancel.is_some_and(|token| token.is_cancelled()) {
        tracing::debug!(
            filename = source_name,
            "artwork source replaced during derivative generation"
        );
        remove_derivative_temporary(&temporary_path).await;
        return Err("source changed during derivative generation".to_owned());
    }
    if let Err(error) = tokio::fs::rename(&temporary_path, derived_dir.join(derived_name)).await {
        remove_derivative_temporary(&temporary_path).await;
        return Err(error.to_string());
    }
    // The key is new by construction, but a regenerated key that a sweep had
    // reclaimed must not be answered from the entry its old inode left behind.
    coordinator.forget(derived_name).await;
    #[cfg(test)]
    coordinator.derivations.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

/// The resize child, built separately so a test can read back the runtime it
/// will be given without spawning one.
fn derivative_command(
    ffmpeg_bin: &str,
    source_path: &FsPath,
    runtime_cache: &FsPath,
    size: ArtworkSize,
    extension: &str,
    codec: &str,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(ffmpeg_bin);
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"]);
    command.arg(source_path);
    command.args([
        "-vf",
        &format!(
            "scale='min({},iw)':-2:flags=lanczos",
            size.width().expect("derivative width")
        ),
        "-frames:v",
        "1",
    ]);
    if matches!(extension, "jpg" | "jpeg") {
        command.args(["-q:v", "3"]);
    }
    command.args(["-f", "image2pipe", "-vcodec", codec, "pipe:1"]);
    crate::producer_spawn::configure_ffmpeg_runtime(&mut command, runtime_cache);
    command
}

async fn remove_derivative_temporary(path: &FsPath) {
    for _ in 0..20 {
        match tokio::fs::remove_file(path).await {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    tracing::warn!(path = %path.display(), "could not remove failed artwork derivative temporary");
}

fn should_serve_original(size: ArtworkSize, filename: &str, bytes: &[u8]) -> bool {
    image_dimensions(filename, bytes)
        .zip(size.width())
        .is_some_and(|((width, _), target)| width <= target)
}

fn image_dimensions(filename: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    let extension = FsPath::new(filename)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" if bytes.get(..8) == Some(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 => Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        )),
        "gif" if bytes.len() >= 10 && matches!(bytes.get(..6), Some(b"GIF87a" | b"GIF89a")) => {
            Some((
                u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32,
                u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32,
            ))
        }
        "jpg" | "jpeg" => jpeg_dimensions(bytes),
        "webp" => webp_dimensions(bytes),
        _ => None,
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..2) != Some(&[0xff, 0xd8]) {
        return None;
    }
    let mut cursor = 2;
    while cursor + 4 <= bytes.len() {
        if bytes[cursor] != 0xff {
            cursor += 1;
            continue;
        }
        let marker = bytes[cursor + 1];
        cursor += 2;
        if matches!(marker, 0xd8 | 0xd9) || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = u16::from_be_bytes(bytes.get(cursor..cursor + 2)?.try_into().ok()?) as usize;
        if length < 2 || cursor + length > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) && length >= 7 {
            let height = u16::from_be_bytes(bytes[cursor + 3..cursor + 5].try_into().ok()?) as u32;
            let width = u16::from_be_bytes(bytes[cursor + 5..cursor + 7].try_into().ok()?) as u32;
            return Some((width, height));
        }
        cursor += length;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 || bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return None;
    }
    match bytes.get(12..16)? {
        b"VP8X" => Some((
            1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]),
            1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]),
        )),
        b"VP8 " if bytes.get(23..26) == Some(&[0x9d, 0x01, 0x2a]) => Some((
            (u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3fff) as u32,
            (u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3fff) as u32,
        )),
        b"VP8L" if bytes.get(20) == Some(&0x2f) => {
            let bits = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
            Some((1 + (bits & 0x3fff), 1 + ((bits >> 14) & 0x3fff)))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArtworkCapacity;

const ARTWORK_CAPACITY_CODE: &str = "artwork_response_capacity";

#[derive(Clone, Copy, Debug)]
enum ArtworkRoute {
    User = 0,
    Peer = 1,
    Plex = 2,
}

#[derive(Clone, Copy, Debug)]
enum ArtworkOutcome {
    Hit = 0,
    NotModified = 1,
    Peer = 2,
    Miss = 3,
    Capacity = 4,
    Corrupt = 5,
}

#[derive(Clone, Copy, Debug)]
enum ArtworkVerification {
    Cached = 0,
    Hashed = 1,
    Invalidated = 2,
}

#[derive(Clone, Copy, Debug)]
enum DerivativeOutcome {
    Served = 0,
    Generated = 1,
    FallbackOriginal = 2,
    Refused = 3,
}

static ARTWORK_REQUESTS: [AtomicU64; 18] = [const { AtomicU64::new(0) }; 18];
static ARTWORK_VERIFICATIONS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static ARTWORK_DERIVATIVES: [AtomicU64; 12] = [const { AtomicU64::new(0) }; 12];

fn record_request(route: ArtworkRoute, outcome: ArtworkOutcome) {
    ARTWORK_REQUESTS[route as usize * 6 + outcome as usize].fetch_add(1, Ordering::Relaxed);
}

fn record_verification(result: ArtworkVerification) {
    ARTWORK_VERIFICATIONS[result as usize].fetch_add(1, Ordering::Relaxed);
}

fn record_derivative(size: ArtworkSize, outcome: DerivativeOutcome) {
    let bucket = match size {
        ArtworkSize::W300 => 0,
        ArtworkSize::W500 => 1,
        ArtworkSize::W780 => 2,
        ArtworkSize::Original => return,
    };
    ARTWORK_DERIVATIVES[bucket * 4 + outcome as usize].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn prometheus() -> String {
    let mut out = String::from(
        "# HELP plurx_artwork_requests_total Artwork request outcomes by bounded route and result.\n\
         # TYPE plurx_artwork_requests_total counter\n",
    );
    for (route_index, route) in ["user", "peer", "plex"].into_iter().enumerate() {
        for (outcome_index, outcome) in
            ["hit", "not_modified", "peer", "miss", "capacity", "corrupt"]
                .into_iter()
                .enumerate()
        {
            out.push_str(&format!(
                "plurx_artwork_requests_total{{route=\"{route}\",outcome=\"{outcome}\"}} {}\n",
                ARTWORK_REQUESTS[route_index * 6 + outcome_index].load(Ordering::Relaxed)
            ));
        }
    }
    out.push_str(
        "# HELP plurx_artwork_verifications_total Artwork digest verification decisions.\n\
         # TYPE plurx_artwork_verifications_total counter\n",
    );
    for (index, result) in ["cached", "hashed", "invalidated"].into_iter().enumerate() {
        out.push_str(&format!(
            "plurx_artwork_verifications_total{{result=\"{result}\"}} {}\n",
            ARTWORK_VERIFICATIONS[index].load(Ordering::Relaxed)
        ));
    }
    out.push_str(
        "# HELP plurx_artwork_derivatives_total Artwork derivative outcomes by closed width bucket.\n\
         # TYPE plurx_artwork_derivatives_total counter\n",
    );
    for (bucket_index, bucket) in ["w300", "w500", "w780"].into_iter().enumerate() {
        for (outcome_index, outcome) in ["served", "generated", "fallback_original", "refused"]
            .into_iter()
            .enumerate()
        {
            out.push_str(&format!(
                "plurx_artwork_derivatives_total{{bucket=\"{bucket}\",outcome=\"{outcome}\"}} {}\n",
                ARTWORK_DERIVATIVES[bucket_index * 4 + outcome_index].load(Ordering::Relaxed)
            ));
        }
    }
    out
}

fn artwork_capacity_error() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        ARTWORK_CAPACITY_CODE,
        "artwork response capacity is full; retry shortly",
    )
}

fn is_artwork_capacity_error(error: &ApiError) -> bool {
    matches!(error, ApiError::Typed { code, .. } if *code == ARTWORK_CAPACITY_CODE)
}

async fn fetch_and_materialize<F, Fut>(
    coordinator: &ArtworkCoordinator,
    artwork_dir: &FsPath,
    filename: &str,
    fetch: F,
) -> Result<Option<AdmittedArtworkBytes>, ArtworkCapacity>
where
    F: FnOnce(reqwest::Client) -> Fut,
    Fut: std::future::Future<Output = Option<Vec<u8>>>,
{
    match tokio::time::timeout(
        ARTWORK_FETCH_TOTAL_TIMEOUT,
        fetch_and_materialize_inner(coordinator, artwork_dir, filename, fetch),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Ok(None),
    }
}

async fn fetch_and_materialize_inner<F, Fut>(
    coordinator: &ArtworkCoordinator,
    artwork_dir: &FsPath,
    filename: &str,
    fetch: F,
) -> Result<Option<AdmittedArtworkBytes>, ArtworkCapacity>
where
    F: FnOnce(reqwest::Client) -> Fut,
    Fut: std::future::Future<Output = Option<Vec<u8>>>,
{
    let _filename = coordinator.filename(filename).await;
    match read_verified_local_artwork(coordinator, artwork_dir.join(filename), filename).await {
        LocalArtworkRead::Verified(bytes) => {
            return Ok(Some(bytes));
        }
        LocalArtworkRead::Corrupt(identity) => {
            quarantine_corrupt_artwork(coordinator, artwork_dir, filename, identity).await;
        }
        LocalArtworkRead::Capacity => return Err(ArtworkCapacity),
        LocalArtworkRead::Missing => {}
    }
    let permit = coordinator.peer_permit().ok_or(ArtworkCapacity)?;
    let Some(client) = coordinator.client() else {
        return Ok(None);
    };
    let Some(bytes) = fetch(client).await else {
        return Ok(None);
    };
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    if let Err(error) = install_artwork(coordinator, artwork_dir, filename, &bytes).await {
        // The caller can still use the complete peer response. Log the failed
        // materialization so a read-only or full data directory is visible,
        // but do not turn working peer failover into another broken image.
        tracing::warn!(filename, %error, "cannot materialize peer artwork");
    }
    Ok(Some(AdmittedArtworkBytes {
        bytes,
        digest,
        identity: None,
        _permit: permit,
    }))
}

async fn serve_local_artwork(
    coordinator: &ArtworkCoordinator,
    artwork_dir: &FsPath,
    filename: &str,
    headers: &HeaderMap,
    route: ArtworkRoute,
) -> Result<Response, ApiError> {
    let path = artwork_dir.join(filename);
    if let Some(digest) = cached_not_modified(coordinator, &path, filename, headers).await {
        record_request(route, ArtworkOutcome::NotModified);
        record_verification(ArtworkVerification::Cached);
        return Ok(not_modified_artwork_response(digest));
    }
    match read_verified_local_artwork(coordinator, path.clone(), filename).await {
        LocalArtworkRead::Verified(bytes) => {
            record_request(route, ArtworkOutcome::Hit);
            Ok(admitted_artwork_response(
                &path,
                bytes,
                ARTWORK_CACHE_CONTROL,
                &[],
            ))
        }
        LocalArtworkRead::Corrupt(identity) => {
            coordinator.forget(filename).await;
            quarantine_corrupt_artwork(coordinator, artwork_dir, filename, identity).await;
            record_request(route, ArtworkOutcome::Corrupt);
            Err(ApiError::NotFound("image"))
        }
        LocalArtworkRead::Capacity => Err(artwork_capacity_error()),
        LocalArtworkRead::Missing => Err(ApiError::NotFound("image")),
    }
}

struct OpenedLocalArtwork {
    file: std::fs::File,
    identity: ArtworkFileIdentity,
}

enum LocalArtworkRead {
    Verified(AdmittedArtworkBytes),
    Corrupt(ArtworkFileIdentity),
    Capacity,
    Missing,
}

async fn open_local_artwork(path: std::path::PathBuf) -> Option<OpenedLocalArtwork> {
    tokio::task::spawn_blocking(move || {
        let file = plurx_core::fs_secure::open_read_nofollow_blocking(&path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ARTWORK_BYTES {
            return None;
        }
        Some(OpenedLocalArtwork {
            identity: artwork_file_identity(&metadata),
            file,
        })
    })
    .await
    .ok()
    .flatten()
}

async fn read_verified_local_artwork(
    coordinator: &ArtworkCoordinator,
    path: std::path::PathBuf,
    filename: &str,
) -> LocalArtworkRead {
    let Some(opened) = open_local_artwork(path).await else {
        return LocalArtworkRead::Missing;
    };
    let identity = opened.identity;
    let Some(permit) = coordinator.local_permit(identity.bytes).await else {
        return LocalArtworkRead::Capacity;
    };
    let cached = coordinator.verified_digest(filename, identity).await;
    let Some((bytes, digest)) = tokio::task::spawn_blocking(move || {
        use std::io::Read;

        let mut file = opened.file;
        let mut bytes = Vec::with_capacity(identity.bytes as usize);
        (&mut file)
            .take(identity.bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 != identity.bytes {
            return None;
        }
        let digest = cached.unwrap_or_else(|| Sha256::digest(&bytes).into());
        Some((bytes, digest))
    })
    .await
    .ok()
    .flatten() else {
        return LocalArtworkRead::Missing;
    };

    #[cfg(test)]
    if cached.is_none() {
        coordinator.hashes.fetch_add(1, Ordering::SeqCst);
    }
    if !artwork_digest_matches_name(filename, digest) {
        return LocalArtworkRead::Corrupt(identity);
    }
    if cached.is_some() {
        record_verification(ArtworkVerification::Cached);
    } else {
        coordinator.remember(filename, identity, digest).await;
        record_verification(ArtworkVerification::Hashed);
    }
    LocalArtworkRead::Verified(AdmittedArtworkBytes {
        bytes,
        digest,
        identity: Some(identity),
        _permit: permit,
    })
}

async fn cached_not_modified(
    coordinator: &ArtworkCoordinator,
    path: &FsPath,
    filename: &str,
    headers: &HeaderMap,
) -> Option<[u8; 32]> {
    let validators = headers.get_all(header::IF_NONE_MATCH);
    validators.iter().next()?;
    let opened = open_local_artwork(path.to_path_buf()).await?;
    let digest = coordinator
        .verified_digest(filename, opened.identity)
        .await?;
    if if_none_match_matches(headers, digest) {
        Some(digest)
    } else {
        None
    }
}

fn if_none_match_matches(headers: &HeaderMap, digest: [u8; 32]) -> bool {
    let etag = format!("\"{}\"", hex::encode(digest));
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|candidate| {
            candidate == "*"
                || candidate == etag
                || candidate.strip_prefix("W/") == Some(etag.as_str())
        })
}

async fn read_bounded_local_artwork(
    path: std::path::PathBuf,
) -> Option<(Vec<u8>, ArtworkFileIdentity)> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;

        let file = plurx_core::fs_secure::open_read_nofollow_blocking(&path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ARTWORK_BYTES {
            return None;
        }
        let expected = metadata.len();
        let identity = artwork_file_identity(&metadata);
        let mut bytes = Vec::with_capacity(expected as usize);
        file.take(expected.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 == expected).then_some((bytes, identity))
    })
    .await
    .ok()
    .flatten()
}

async fn valid_materialized_artwork(artwork_dir: &FsPath, filename: &str) -> bool {
    read_bounded_local_artwork(artwork_dir.join(filename))
        .await
        .is_some_and(|(bytes, _)| artwork_bytes_match_name(filename, &bytes))
}

async fn quarantine_corrupt_artwork(
    coordinator: &ArtworkCoordinator,
    artwork_dir: &FsPath,
    filename: &str,
    expected: ArtworkFileIdentity,
) {
    quarantine_corrupt_artwork_with(coordinator, artwork_dir, filename, expected, None).await;
}

type AfterCorruptQuarantineRename = Option<Box<dyn FnOnce(&str) + Send + 'static>>;

async fn quarantine_corrupt_artwork_with(
    coordinator: &ArtworkCoordinator,
    artwork_dir: &FsPath,
    filename: &str,
    expected: ArtworkFileIdentity,
    after_rename: AfterCorruptQuarantineRename,
) {
    quarantine_corrupt_artwork_digest(
        coordinator,
        artwork_dir,
        filename,
        expected,
        after_rename,
        None,
    )
    .await;
}

async fn quarantine_corrupt_artwork_digest(
    coordinator: &ArtworkCoordinator,
    artwork_dir: &FsPath,
    filename: &str,
    expected: ArtworkFileIdentity,
    after_rename: AfterCorruptQuarantineRename,
    digest: Option<String>,
) {
    coordinator.forget(filename).await;
    let quarantine = format!(".{filename}.corrupt-{}", uuid::Uuid::new_v4().simple());
    if plurx_core::fs_secure::rename_child(artwork_dir, filename, &quarantine)
        .await
        .is_err()
    {
        return;
    }
    if let Some(after_rename) = after_rename {
        after_rename(&quarantine);
    }
    let quarantined = read_bounded_local_artwork(artwork_dir.join(&quarantine)).await;
    let still_corrupt = quarantined.as_ref().is_some_and(|(bytes, identity)| {
        identity.same_inode(expected)
            && identity.bytes == expected.bytes
            && match &digest {
                Some(expected) => hex::encode(Sha256::digest(bytes)) != *expected,
                None => !artwork_bytes_match_name(filename, bytes),
            }
    });
    if still_corrupt {
        if let Err(error) = plurx_core::fs_secure::unlink_child(artwork_dir, &quarantine).await {
            tracing::warn!(filename, %error, "could not remove corrupt artwork quarantine");
        } else {
            tracing::warn!(filename, "quarantined corrupt content-addressed artwork");
        }
    } else {
        // The quarantine inode itself changed after we inspected the original.
        // An exclusive rename preserves it when a newer target won the race,
        // leaving the bounded orphan sweep—not this stale request—as the only
        // authority allowed to remove the untrusted replacement.
        let restored =
            plurx_core::fs_secure::rename_child_noreplace(artwork_dir, &quarantine, filename).await;
        tracing::warn!(
            filename,
            ?restored,
            "artwork changed during corruption quarantine; preserved newer bytes"
        );
    }
}

fn safe_artwork_name(filename: &str) -> Result<&str, ApiError> {
    // Only a bare filename is allowed — no directories, no traversal.
    FsPath::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| *name == filename && !name.is_empty())
        .ok_or_else(|| ApiError::BadRequest("invalid image name".into()))
}

struct AdmittedArtworkBytes {
    bytes: Vec<u8>,
    digest: [u8; 32],
    /// The identity of the local file these bytes were read from, when they
    /// came from one. `None` for a peer response that has just been
    /// materialized: there is no local read behind it, so nothing here may be
    /// used to fence work against the file on disk.
    identity: Option<ArtworkFileIdentity>,
    _permit: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for AdmittedArtworkBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

fn admitted_artwork_response(
    path: &FsPath,
    bytes: AdmittedArtworkBytes,
    cache_control: &'static str,
    extra_headers: &[(&'static str, &'static str)],
) -> Response {
    let digest = bytes.digest;
    artwork_response_bytes(
        path,
        bytes::Bytes::from_owner(bytes),
        digest,
        cache_control,
        extra_headers,
    )
}

#[cfg(test)]
fn artwork_response(path: &FsPath, bytes: Vec<u8>) -> Response {
    let digest = Sha256::digest(&bytes).into();
    artwork_response_bytes(path, bytes.into(), digest, ARTWORK_CACHE_CONTROL, &[])
}

fn artwork_response_bytes(
    path: &FsPath,
    bytes: bytes::Bytes,
    digest: [u8; 32],
    cache_control: &'static str,
    extra_headers: &[(&'static str, &'static str)],
) -> Response {
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let mut response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache_control.to_owned()),
        ],
        bytes,
    )
        .into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", hex::encode(digest)))
            .expect("SHA-256 ETag is a valid header"),
    );
    for &(name, value) in extra_headers {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

fn not_modified_artwork_response(digest: [u8; 32]) -> Response {
    let mut response = StatusCode::NOT_MODIFIED.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(ARTWORK_CACHE_CONTROL),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", hex::encode(digest)))
            .expect("SHA-256 ETag is a valid header"),
    );
    response
}

fn artwork_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(750))
        .timeout(Duration::from_secs(3))
        // A redirect could send a valid node proof to a host the replicated
        // roster never authorized.
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

async fn fetch_peer_artwork(
    client: &reqwest::Client,
    membership: &MembershipManager,
    peers: &[String],
    filename: &str,
) -> Option<Vec<u8>> {
    let auth = membership.artwork_peer_auth(filename).ok()?;
    fetch_peer_artwork_with_auth(client, peers, filename, &auth).await
}

async fn fetch_peer_artwork_with_auth(
    client: &reqwest::Client,
    peers: &[String],
    filename: &str,
    auth: &ArtworkPeerAuth,
) -> Option<Vec<u8>> {
    fetch_peer_artwork_checked(client, peers, filename, auth, None).await
}

async fn fetch_peer_artwork_checked(
    client: &reqwest::Client,
    peers: &[String],
    filename: &str,
    auth: &ArtworkPeerAuth,
    expected: Option<(&str, i64)>,
) -> Option<Vec<u8>> {
    // Build an owned synchronous request list before constructing any future.
    // Returning an async block from a borrowed iterator adapter makes the
    // composed handler lifetime-specific even when the block clones its input.
    let mut requests = Vec::with_capacity(peers.len());
    for peer in peers {
        match peer_artwork_url(peer, filename) {
            Some(url) => requests.push((peer.clone(), url)),
            None => tracing::warn!(peer, "ignoring invalid artwork peer URL"),
        }
    }
    let requests = requests.into_iter().map(|(peer, url)| {
        let client = client.clone();
        let auth = auth.clone();
        let filename = filename.to_owned();
        let expected = expected.map(|(digest, bytes)| (digest.to_owned(), bytes));
        async move {
            let response = match client
                .get(url)
                .header(NODE_ID_HEADER, &auth.node_id)
                .header(TIMESTAMP_HEADER, auth.timestamp_ms)
                .header(SIGNATURE_HEADER, &auth.signature)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => response,
                Ok(_) => return None,
                Err(error) => {
                    tracing::debug!(peer, %error, "artwork peer request failed");
                    return None;
                }
            };
            if response
                .content_length()
                .is_some_and(|length| length > MAX_ARTWORK_BYTES)
            {
                return None;
            }
            let is_image = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().starts_with("image/"));
            if !is_image {
                return None;
            }

            let mut body = response.bytes_stream();
            let mut bytes = Vec::new();
            let mut digest = Sha256::new();
            let mut failed = false;
            while let Some(chunk) = body.next().await {
                match chunk {
                    Ok(chunk) if bytes.len() as u64 + chunk.len() as u64 <= MAX_ARTWORK_BYTES => {
                        digest.update(&chunk);
                        bytes.extend_from_slice(&chunk);
                    }
                    _ => {
                        failed = true;
                        break;
                    }
                }
            }
            if failed || bytes.is_empty() {
                return None;
            }
            let digest: [u8; 32] = digest.finalize().into();
            let matches = match expected {
                Some((expected, count)) => {
                    bytes.len() as i64 == count && hex::encode(digest) == expected
                }
                None => artwork_digest_matches_name(&filename, digest),
            };
            matches.then_some(bytes)
        }
    });
    tokio::time::timeout(PEER_RACE_DEADLINE, async {
        let mut requests = stream::iter(requests).buffer_unordered(PEER_RACE_CONCURRENCY);
        while let Some(bytes) = requests.next().await {
            if bytes.is_some() {
                return bytes;
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

fn peer_auth_from_headers(headers: &HeaderMap) -> Option<ArtworkPeerAuth> {
    Some(ArtworkPeerAuth {
        node_id: headers.get(NODE_ID_HEADER)?.to_str().ok()?.to_owned(),
        timestamp_ms: headers.get(TIMESTAMP_HEADER)?.to_str().ok()?.parse().ok()?,
        signature: headers.get(SIGNATURE_HEADER)?.to_str().ok()?.to_owned(),
    })
}

fn peer_artwork_url(peer: &str, filename: &str) -> Option<reqwest::Url> {
    let mut url = reqwest::Url::parse(peer).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    let mut segments = url.path_segments_mut().ok()?;
    segments.pop_if_empty();
    segments.extend(["api", "v1", "cluster", "artwork", filename]);
    drop(segments);
    Some(url)
}

async fn install_artwork(
    coordinator: &ArtworkCoordinator,
    artwork_dir: &FsPath,
    filename: &str,
    bytes: &[u8],
) -> Result<(), std::io::Error> {
    if !artwork_bytes_match_name(filename, bytes) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "artwork bytes disagree with the content-addressed filename",
        ));
    }
    coordinator.forget(filename).await;
    plurx_core::fs_secure::atomic_write_child(artwork_dir, filename, bytes).await
}

#[derive(Clone, Debug)]
struct ArtworkReference {
    item_id: i64,
    filename: String,
}

trait ArtworkPaths {
    fn into_artwork_paths(self) -> (i64, Option<String>, Option<String>);
}

impl ArtworkPaths for Item {
    fn into_artwork_paths(self) -> (i64, Option<String>, Option<String>) {
        (self.id, self.poster_path, self.backdrop_path)
    }
}

impl ArtworkPaths for ArtworkInventoryItem {
    fn into_artwork_paths(self) -> (i64, Option<String>, Option<String>) {
        (self.id, self.poster_path, self.backdrop_path)
    }
}

fn artwork_references<T: ArtworkPaths>(items: Vec<T>) -> Vec<ArtworkReference> {
    let mut references = BTreeMap::<String, i64>::new();
    for item in items {
        let (item_id, poster_path, backdrop_path) = item.into_artwork_paths();
        for filename in [poster_path, backdrop_path].into_iter().flatten() {
            if safe_artwork_name(&filename).is_ok() {
                references.entry(filename).or_insert(item_id);
            }
        }
    }
    references
        .into_iter()
        .map(|(filename, item_id)| ArtworkReference { item_id, filename })
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MaterializeReport {
    pub references: usize,
    pub missing: usize,
    pub copied: usize,
    pub deferred_capacity: usize,
    pub source_repairs: usize,
    pub unresolved: usize,
    pub orphans_removed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtworkPullOutcome {
    Copied,
    Missing,
    DeferredCapacity,
}

fn artwork_pull_outcome(
    result: Result<Option<AdmittedArtworkBytes>, ArtworkCapacity>,
) -> ArtworkPullOutcome {
    match result {
        Ok(Some(_)) => ArtworkPullOutcome::Copied,
        Ok(None) => ArtworkPullOutcome::Missing,
        Err(ArtworkCapacity) => ArtworkPullOutcome::DeferredCapacity,
    }
}

fn suppress_deferred_artwork_repairs(
    deferred_items: BTreeSet<i64>,
    unresolved_by_item: &mut BTreeMap<i64, Vec<String>>,
    repair_after: &mut HashMap<i64, Instant>,
) {
    for item_id in deferred_items {
        unresolved_by_item.remove(&item_id);
        repair_after.remove(&item_id);
    }
}

fn retain_recent_artwork_repairs(repair_after: &mut HashMap<i64, Instant>, now: Instant) {
    // A due timestamp is the signal to run repair, not an expired cache entry.
    // Keep it through at least one full provider backoff window so paginated
    // inventories can revisit large libraries; genuinely deleted item ids age
    // out without resetting every still-missing item at the moment it is due.
    repair_after.retain(|_, retry| {
        retry
            .checked_add(SOURCE_REPAIR_BACKOFF)
            .is_some_and(|stale_after| stale_after > now)
    });
}

fn due_artwork_repairs(
    unresolved_by_item: BTreeMap<i64, Vec<String>>,
    repair_after: &mut HashMap<i64, Instant>,
    now: Instant,
) -> BTreeMap<i64, Vec<String>> {
    let mut repair_items = BTreeMap::new();
    for (item_id, filenames) in unresolved_by_item {
        let retry = repair_after
            .entry(item_id)
            .or_insert(now + SOURCE_REPAIR_GRACE);
        if *retry <= now {
            repair_items.insert(item_id, filenames);
        }
    }
    repair_items
}

#[cfg(test)]
fn managed_artwork_filename(filename: &str) -> bool {
    let path = FsPath::new(filename);
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    if !matches!(
        extension.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp"
    ) {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let Some((item_id, kind)) = stem.split_once('-') else {
        return false;
    };
    let Ok(item_id) = item_id.parse::<i64>() else {
        return false;
    };
    if item_id <= 0 {
        return false;
    }
    if matches!(kind, "poster" | "backdrop") {
        return true;
    }
    kind.strip_prefix("poster-").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
async fn orphan_candidate_old_enough(path: &FsPath, grace: Duration) -> bool {
    tokio::fs::metadata(path)
        .await
        .ok()
        .filter(|metadata| metadata.is_file())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= grace)
}

#[cfg(test)]
async fn remove_orphan_candidate(
    store: &dyn Store,
    filename: &str,
    path: &FsPath,
    grace: Duration,
) -> bool {
    let Ok(reservation) = plurx_core::metadata::reserve_artwork_publication(path.to_path_buf())
    else {
        return false;
    };
    // Candidate discovery happens before this reservation. Re-stat while
    // holding it: a publisher that replaced the path and released its slot
    // after discovery has reset the age, so its fresh bytes cannot be deleted
    // before the following catalogue mutation commits.
    if !orphan_candidate_old_enough(path, grace).await {
        return false;
    }
    // Recheck this exact filename consistently after taking the reservation
    // so a concurrent replicated catalogue update cannot turn a candidate
    // into live artwork between discovery and deletion.
    match store.artwork_filename_is_referenced(filename).await {
        Ok(false) => {}
        Ok(true) | Err(_) => return false,
    }
    match plurx_core::metadata::remove_artwork_reserved(reservation).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(filename, %error, "removing orphaned artwork");
            false
        }
    }
}

#[cfg(test)]
async fn sweep_orphan_artwork(store: &dyn Store, artwork_dir: &FsPath, grace: Duration) -> usize {
    let Ok(referenced) = store.items_with_artwork().await else {
        return 0;
    };
    let referenced = artwork_references(referenced)
        .into_iter()
        .map(|reference| reference.filename)
        .collect::<BTreeSet<_>>();
    let Ok(mut entries) = tokio::fs::read_dir(artwork_dir).await else {
        return 0;
    };
    let mut candidates = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let filename = entry.file_name().to_string_lossy().into_owned();
        if referenced.contains(&filename) || !managed_artwork_filename(&filename) {
            continue;
        }
        if orphan_candidate_old_enough(&entry.path(), grace).await {
            candidates.push((filename, entry.path()));
        }
    }
    if candidates.is_empty() {
        return 0;
    }

    let mut removed = 0;
    for (filename, path) in candidates {
        if remove_orphan_candidate(store, &filename, &path, grace).await {
            removed += 1;
        }
    }
    removed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtworkContentDigest<'a> {
    Prefix16(&'a str),
    Full64(&'a str),
}

fn supported_artwork_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp"
    )
}

fn managed_artwork_stem(stem: &str) -> Option<&str> {
    let (item_id, kind) = stem.split_once('-')?;
    (item_id.parse::<i64>().ok()? > 0 && matches!(kind, "poster" | "backdrop")).then_some(kind)
}

/// Classify the two immutable artwork formats Plurx publishes. Keeping this
/// parser exact is important: it decides both when served bytes need digest
/// authentication and which unreferenced files the bounded orphan walker may
/// delete.
fn content_addressed_artwork_digest(filename: &str) -> Option<ArtworkContentDigest<'_>> {
    let (stem, extension) = filename.rsplit_once('.')?;
    if !supported_artwork_extension(extension) {
        return None;
    }
    if let Some((managed, digest)) = stem.rsplit_once("-c") {
        managed_artwork_stem(managed)?;
        return (digest.len() == 16 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(ArtworkContentDigest::Prefix16(digest));
    }
    let (managed, digest) = stem.rsplit_once('-')?;
    let kind = managed_artwork_stem(managed)?;
    (kind == "poster" && digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(ArtworkContentDigest::Full64(digest))
}

fn content_addressed_artwork_name(filename: &str) -> bool {
    content_addressed_artwork_digest(filename).is_some()
}

fn corrupt_artwork_quarantine_name(filename: &str) -> bool {
    let Some((managed, suffix)) = filename
        .strip_prefix('.')
        .and_then(|name| name.rsplit_once(".corrupt-"))
    else {
        return false;
    };
    content_addressed_artwork_name(managed)
        && suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn temporary_artwork_orphan_name(filename: &str) -> bool {
    filename.starts_with('.')
        && (filename.ends_with(".part")
            || filename.ends_with(".tmp")
            || filename.contains(".tmp.")
            || corrupt_artwork_quarantine_name(filename))
}

fn artwork_bytes_match_name(filename: &str, bytes: &[u8]) -> bool {
    artwork_digest_matches_name(filename, Sha256::digest(bytes).into())
}

fn artwork_digest_matches_name(filename: &str, digest: [u8; 32]) -> bool {
    let Some(expected) = content_addressed_artwork_digest(filename) else {
        return true;
    };
    let actual = hex::encode(digest);
    match expected {
        ArtworkContentDigest::Prefix16(expected) => actual[..16].eq_ignore_ascii_case(expected),
        ArtworkContentDigest::Full64(expected) => actual.eq_ignore_ascii_case(expected),
    }
}

#[derive(Default)]
struct ArtworkOrphanWalker {
    entries: Option<tokio::fs::ReadDir>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArtworkFileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    changed_secs: i64,
    changed_nanos: i64,
}

impl ArtworkFileIdentity {
    fn same_inode(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

#[cfg(unix)]
fn artwork_file_identity(metadata: &std::fs::Metadata) -> ArtworkFileIdentity {
    use std::os::unix::fs::MetadataExt;
    ArtworkFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.len(),
        changed_secs: metadata.ctime(),
        changed_nanos: metadata.ctime_nsec(),
    }
}

#[cfg(windows)]
fn artwork_file_identity(metadata: &std::fs::Metadata) -> ArtworkFileIdentity {
    use std::os::windows::fs::MetadataExt as _;

    let changed = metadata.last_write_time();
    ArtworkFileIdentity {
        device: 0,
        inode: metadata.creation_time() ^ changed.rotate_left(17),
        bytes: metadata.file_size(),
        changed_secs: (changed / 10_000_000) as i64,
        changed_nanos: ((changed % 10_000_000) * 100) as i64,
    }
}

struct ArtworkOrphanCandidate {
    filename: String,
    identity: ArtworkFileIdentity,
    temporary: bool,
}

async fn secure_artwork_identity(path: std::path::PathBuf) -> Option<ArtworkFileIdentity> {
    tokio::task::spawn_blocking(move || {
        let file = plurx_core::fs_secure::open_read_nofollow_blocking(&path).ok()?;
        let metadata = file.metadata().ok()?;
        metadata.is_file().then(|| artwork_file_identity(&metadata))
    })
    .await
    .ok()
    .flatten()
}

async fn sweep_content_orphans(state: &AppState, walker: &mut ArtworkOrphanWalker) -> usize {
    if walker.entries.is_none() {
        walker.entries = match tokio::fs::read_dir(&state.artwork_dir).await {
            Ok(entries) => Some(entries),
            Err(error) => {
                tracing::warn!(path = %state.artwork_dir.display(), %error, "cannot scan artwork orphans");
                None
            }
        };
    }
    let Some(entries) = walker.entries.as_mut() else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut scanned = 0;
    let mut removed = 0;
    let mut candidates = Vec::with_capacity(ORPHAN_REMOVE_LIMIT);
    while scanned < ORPHAN_SCAN_LIMIT && candidates.len() < ORPHAN_REMOVE_LIMIT {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                walker.entries = None;
                break;
            }
            Err(error) => {
                tracing::warn!(path = %state.artwork_dir.display(), %error, "cannot continue artwork orphan scan");
                walker.entries = None;
                break;
            }
        };
        scanned += 1;
        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let temporary = temporary_artwork_orphan_name(&filename);
        if !temporary && !content_addressed_artwork_name(&filename) {
            continue;
        }
        let Ok(metadata) = tokio::fs::symlink_metadata(entry.path()).await else {
            continue;
        };
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_none_or(|age| age < CONTENT_ORPHAN_GRACE)
        {
            continue;
        }
        candidates.push(ArtworkOrphanCandidate {
            filename,
            identity: artwork_file_identity(&metadata),
            temporary,
        });
    }
    let reference_candidates = candidates
        .iter()
        .filter(|candidate| !candidate.temporary)
        .map(|candidate| candidate.filename.clone())
        .collect::<Vec<_>>();
    let referenced = match state
        .store
        .referenced_artwork_filenames(&reference_candidates)
        .await
    {
        Ok(referenced) => referenced.into_iter().collect::<BTreeSet<_>>(),
        Err(error) => {
            tracing::warn!(%error, "cannot prefilter artwork orphan ownership");
            return 0;
        }
    };
    for candidate in candidates {
        if referenced.contains(&candidate.filename) {
            continue;
        }
        let quarantine = format!(".orphan.{}.part", uuid::Uuid::new_v4().simple());
        if plurx_core::fs_secure::rename_child(&state.artwork_dir, &candidate.filename, &quarantine)
            .await
            .is_err()
        {
            continue;
        }
        let identity_matches = secure_artwork_identity(state.artwork_dir.join(&quarantine))
            .await
            .is_some_and(|identity| {
                identity.same_inode(candidate.identity)
                    && identity.bytes == candidate.identity.bytes
            });
        let referenced_after_quarantine = if candidate.temporary {
            false
        } else {
            match state
                .store
                .artwork_filename_is_referenced(&candidate.filename)
                .await
            {
                Ok(referenced) => referenced,
                Err(error) => {
                    tracing::warn!(filename = candidate.filename, %error, "cannot confirm quarantined artwork ownership");
                    true
                }
            }
        };
        if !identity_matches || referenced_after_quarantine {
            let _ = plurx_core::fs_secure::restore_child_noreplace(
                &state.artwork_dir,
                &quarantine,
                &candidate.filename,
            )
            .await;
        } else if plurx_core::fs_secure::unlink_child(&state.artwork_dir, &quarantine)
            .await
            .is_ok()
        {
            removed += 1;
            if let Err(error) = state
                .store
                .prune_unreferenced_book_cover_origins(&candidate.filename)
                .await
            {
                tracing::warn!(filename = candidate.filename, %error, "cannot prune orphaned Curator origin");
            }
        }
    }
    removed + sweep_derived_orphans(state).await
}

/// A derivative filename decomposed back into the facts the sweep needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DerivativeName<'a> {
    /// The source artwork filename this derivative was made from.
    source: &'a str,
    /// The first 32 hex characters of the source digest at generation time.
    prefix: &'a str,
}

fn parse_derivative_name(filename: &str) -> Option<DerivativeName<'_>> {
    let (stem, extension) = filename.rsplit_once('.')?;
    if !matches!(extension, "jpg" | "jpeg" | "png" | "webp") {
        return None;
    }
    let stem = match stem.rsplit_once("-b") {
        Some((base, blob)) if blob.len() == 32 && blob.bytes().all(|b| b.is_ascii_hexdigit()) => {
            base
        }
        _ => stem,
    };
    let stem = match stem.rsplit_once('-') {
        Some((base, pipeline))
            if pipeline.len() == 16 && pipeline.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            base
        }
        _ => stem,
    };
    let (stem, prefix) = stem.rsplit_once('-')?;
    if prefix.len() != 32 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let (source, bucket) = stem.rsplit_once('-')?;
    if !matches!(bucket, "w300" | "w500" | "w780") {
        return None;
    }
    // A derivative always names a plain child of the artwork root.
    (!source.is_empty() && !source.contains('/') && !source.contains('\\'))
        .then_some(DerivativeName { source, prefix })
}

/// The pre-review derivative layout, `{digest32}-{bucket}.{ext}`, which carried
/// no source filename. Nothing in the current code writes one, and nothing
/// would ever match one to a source again, so a node that ran an earlier build
/// of this branch would keep them forever. Recognise the shape so the sweep can
/// reclaim them once past the grace.
fn superseded_derivative_name(filename: &str) -> bool {
    let Some((stem, extension)) = filename.rsplit_once('.') else {
        return false;
    };
    if !matches!(extension, "jpg" | "jpeg" | "png" | "webp") {
        return false;
    }
    let Some((prefix, bucket)) = stem.rsplit_once('-') else {
        return false;
    };
    prefix.len() == 32
        && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(bucket, "w300" | "w500" | "w780")
}

async fn sweep_derived_orphans(state: &AppState) -> usize {
    // Exclude the rename-to-publication interval. Never wait behind active work:
    // a later bounded sweep can reclaim the same orphan.
    let Ok(_publication) = Arc::clone(&state.artwork_fetch.derive_permits)
        .try_acquire_many_owned(DERIVATIVE_CONCURRENCY as u32)
    else {
        return 0;
    };
    let Ok(items) = state.store.items_with_artwork().await else {
        return 0;
    };
    let cached = state.artwork_fetch.verified_snapshot().await;
    // Per-source retention, as the plan specifies. Each referenced source maps
    // to the digest prefix its live bytes have *if that is knowable*: from the
    // filename for the two content-addressed families, from the M1 verified
    // cache for a legacy name, and `None` for a legacy name nothing has
    // verified since boot. `None` retains that one source's derivatives and
    // says nothing about any other source — previously a single such filename
    // returned early and disabled the whole sweep for the life of the process,
    // which on a library holding one `{item}-poster.jpg` is every pass after
    // every restart.
    let mut referenced = BTreeMap::<String, Option<String>>::new();
    for reference in artwork_references(items) {
        let prefix = match content_addressed_artwork_digest(&reference.filename) {
            // Sixteen hex characters is all this family publishes, so the
            // comparison narrows to sixteen for every family. Comparing the
            // name's sixteen against a derivative's thirty-two matched nothing
            // and swept every live backdrop derivative once past the grace.
            Some(ArtworkContentDigest::Prefix16(prefix)) => Some(prefix.to_ascii_lowercase()),
            Some(ArtworkContentDigest::Full64(digest)) => {
                Some(digest[..DERIVATIVE_PREFIX_MATCH].to_ascii_lowercase())
            }
            None => cached
                .get(&reference.filename)
                .map(|digest| hex::encode(digest)[..DERIVATIVE_PREFIX_MATCH].to_owned()),
        };
        referenced.insert(reference.filename, prefix);
    }
    let derived_dir = state.artwork_dir.join(DERIVED_DIR);
    let Ok(mut entries) = tokio::fs::read_dir(&derived_dir).await else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut scanned = 0;
    let mut removed = 0;
    while scanned < ORPHAN_SCAN_LIMIT && removed < ORPHAN_REMOVE_LIMIT {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            _ => break,
        };
        scanned += 1;
        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let staging = worker::staging_owner(&filename);
        let published_key = worker::published_key(&filename);
        if staging.is_none()
            && published_key.is_none()
            && !derived_entry_is_reclaimable(&filename, &referenced)
        {
            continue;
        }
        let Ok(metadata) = tokio::fs::symlink_metadata(entry.path()).await else {
            continue;
        };
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_none_or(|age| age < CONTENT_ORPHAN_GRACE)
        {
            continue;
        }
        if let Some(key) = published_key {
            match state.store.artwork_locations(key, worker::now_ms()).await {
                Ok(locations)
                    if locations.iter().any(|location| {
                        location.node_id == state.node_id
                            && worker::location_filename(location).as_deref() == Some(&filename)
                    }) =>
                {
                    continue
                }
                Err(_) => continue,
                _ => {}
            }
        }
        if let Some((job_id, fence)) = staging {
            match state.store.background_job(&job_id).await {
                Ok(Some(job))
                    if job.fence == fence
                        && matches!(
                            job.state,
                            plurx_core::store::background_jobs::JobState::Running
                                | plurx_core::store::background_jobs::JobState::Cancelling
                        ) =>
                {
                    continue
                }
                Err(_) => continue,
                _ => {}
            }
        }
        if tokio::fs::remove_file(entry.path()).await.is_ok() {
            state.artwork_fetch.forget(&filename).await;
            removed += 1;
        }
    }
    removed
}

/// Whether one entry of `derived/` may be reclaimed once it is past the grace.
///
/// `referenced` maps every source filename the catalogue still points at to the
/// first [`DERIVATIVE_PREFIX_MATCH`] hex characters of its live digest, or to
/// `None` when that digest is not yet known on this node.
fn derived_entry_is_reclaimable(
    filename: &str,
    referenced: &BTreeMap<String, Option<String>>,
) -> bool {
    // An interrupted generation — SIGKILL, panic, deploy restart — leaves its
    // `.{derived_name}.{uuid}.tmp` behind. `sweep_content_orphans` walks only
    // the artwork root and never descends here, so nothing else would ever
    // remove one, and each costs up to `MAX_ARTWORK_BYTES`.
    if temporary_artwork_orphan_name(filename) {
        return true;
    }
    if superseded_derivative_name(filename) {
        return true;
    }
    let Some(derivative) = parse_derivative_name(filename) else {
        // Not something this node writes. Leave it alone rather than delete a
        // file whose purpose is unknown.
        return false;
    };
    match referenced.get(derivative.source) {
        // The source itself is gone from the catalogue, so every derivative of
        // it is dead regardless of any digest.
        None => true,
        // The source is referenced but nothing has verified its bytes since
        // boot. Keeping a dead derivative costs disk; deleting a live one costs
        // a re-derive on a grid that is asking for it now.
        Some(None) => false,
        // The source is referenced and its live digest is known: this
        // derivative is dead exactly when it was made from other bytes.
        Some(Some(live)) => {
            !derivative.prefix[..DERIVATIVE_PREFIX_MATCH].eq_ignore_ascii_case(live)
        }
    }
}

/// How much of a reconciliation pass this node may run.
///
/// Two different questions were collapsed into one before the learner protocol
/// existed, because every member was a voter and the answers could not differ.
/// They differ now: materializing this node's own artwork is node-local work
/// that every member has to do — a learner serves images like any other node —
/// while source/provider repair is cluster-wide singleton work that exactly one
/// voter owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtworkReconciliation {
    /// Converge this node's own artwork directory: scan the references, pull
    /// missing files from peers, sweep orphans. Nothing here speaks for the
    /// cluster.
    LocalOnly,
    /// The above, plus the source/provider repair that takes the replicated
    /// artwork repair fence on the cluster's behalf.
    Full,
}

/// Reconcile every filename named by replicated item rows onto this node.
/// Peer copies are preferred; source/provider repair is a bounded fallback for
/// the case where the last node holding a file disappeared before convergence,
/// and it only runs under [`ArtworkReconciliation::Full`].
async fn materialize_once(
    state: &AppState,
    scope: ArtworkReconciliation,
    repair_after: &mut HashMap<i64, Instant>,
    item_cursor: &mut i64,
    orphan_walker: &mut ArtworkOrphanWalker,
) -> Result<MaterializeReport, StoreError> {
    let orphans_removed = sweep_content_orphans(state, orphan_walker).await;
    if !state.membership.is_replicated() {
        return Ok(MaterializeReport {
            orphans_removed,
            ..Default::default()
        });
    }
    let items = state
        .store
        .items_with_artwork_page(*item_cursor, MATERIALIZE_ITEM_PAGE)
        .await?;
    if items.is_empty() {
        *item_cursor = 0;
    } else {
        *item_cursor = items.last().map_or(0, |item| item.id);
    }
    let references = artwork_references(items);
    let mut report = MaterializeReport {
        references: references.len(),
        orphans_removed,
        ..Default::default()
    };
    let mut missing = Vec::new();
    for reference in references {
        let path = state.artwork_dir.join(&reference.filename);
        // Steady-state reconciliation is an inventory operation, not a
        // cryptographic scrub. Reading and hashing every referenced image on
        // every voter each minute can consume gigabytes; request serving and
        // peer installation authenticate content-addressed bytes when they are
        // actually used or introduced.
        let present = tokio::task::spawn_blocking(move || {
            let file = plurx_core::fs_secure::open_read_nofollow_blocking(&path).ok()?;
            let metadata = file.metadata().ok()?;
            (metadata.is_file() && metadata.len() > 0 && metadata.len() <= MAX_ARTWORK_BYTES)
                .then_some(())
        })
        .await
        .ok()
        .flatten()
        .is_some();
        if !present {
            missing.push(reference);
        }
    }
    report.missing = missing.len();
    let now = Instant::now();
    retain_recent_artwork_repairs(repair_after, now);
    if missing.is_empty() {
        return Ok(report);
    }

    let peers = match state.membership.reachable_peer_http_urls().await {
        Ok(peers) => peers,
        Err(error) => {
            tracing::warn!(code = error.code(), "cannot read artwork peer roster");
            Vec::new()
        }
    };
    let peers = Arc::new(peers);
    let mut pulls = stream::iter(missing.into_iter().map(|reference| {
        let membership = state.membership.clone();
        let peers = Arc::clone(&peers);
        let artwork_dir = state.artwork_dir.clone();
        let coordinator = Arc::clone(&state.artwork_fetch);
        async move {
            let filename = reference.filename.clone();
            let fetch_filename = filename.clone();
            let outcome = artwork_pull_outcome(
                fetch_and_materialize(
                    &coordinator,
                    &artwork_dir,
                    &filename,
                    move |client| async move {
                        fetch_peer_artwork(&client, &membership, &peers, &fetch_filename).await
                    },
                )
                .await,
            );
            (reference, outcome)
        }
    }))
    .buffer_unordered(PEER_FETCH_CONCURRENCY);

    let now = Instant::now();
    let mut unresolved_by_item = BTreeMap::<i64, Vec<String>>::new();
    let mut deferred_items = BTreeSet::new();
    let mut observed_items = BTreeSet::new();
    while let Some((reference, outcome)) = pulls.next().await {
        observed_items.insert(reference.item_id);
        match outcome {
            ArtworkPullOutcome::Copied => report.copied += 1,
            ArtworkPullOutcome::Missing => {
                report.unresolved += 1;
                unresolved_by_item
                    .entry(reference.item_id)
                    .or_default()
                    .push(reference.filename);
            }
            ArtworkPullOutcome::DeferredCapacity => {
                report.deferred_capacity += 1;
                deferred_items.insert(reference.item_id);
            }
        }
    }
    // Capacity says nothing about peer availability. If any filename for an
    // item was deferred, do not turn the partial observation into a provider
    // repair/backoff decision; the next reconciliation pass retries it.
    suppress_deferred_artwork_repairs(deferred_items, &mut unresolved_by_item, repair_after);
    for item_id in observed_items {
        if !unresolved_by_item.contains_key(&item_id) {
            repair_after.remove(&item_id);
        }
    }

    let repair_items = due_artwork_repairs(unresolved_by_item, repair_after, now);
    if scope == ArtworkReconciliation::LocalOnly {
        // Everything below claims the replicated artwork repair fence and
        // talks to metadata providers on the cluster's behalf. A node with no
        // vote must not do that — `claim_artwork_source_repair` refuses it
        // anyway, so running the loop would only produce log noise and burn
        // the per-item backoff a voter is relying on.
        return Ok(report);
    }

    for (item_id, filenames) in repair_items.into_iter().take(SOURCE_REPAIRS_PER_PASS) {
        let local_deadline = tokio::time::Instant::now() + SOURCE_REPAIR_LEASE;
        match tokio::time::timeout_at(
            local_deadline,
            state
                .jobs
                .materialize_local_item_artwork(item_id, &filenames),
        )
        .await
        {
            Ok(Ok(Some(true))) => {
                report.source_repairs += 1;
                repair_after.insert(item_id, now + SOURCE_REPAIR_BACKOFF);
                continue;
            }
            Ok(Ok(Some(false))) => {
                repair_after.insert(item_id, now + SOURCE_REPAIR_LEASE);
                continue;
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                tracing::warn!(item_id, %error, "local artwork materialization failed");
                repair_after.insert(item_id, now + SOURCE_REPAIR_LEASE);
                continue;
            }
            Err(_) => {
                tracing::warn!(item_id, "local artwork materialization exceeded its lease");
                repair_after.insert(item_id, now + SOURCE_REPAIR_LEASE);
                continue;
            }
        }
        match state
            .membership
            .claim_artwork_source_repair(item_id, SOURCE_REPAIR_LEASE)
            .await
        {
            Ok(Some(claim)) => {
                // The successor for a newer Raft term waits this same lease
                // from its first observation of our fence. The timeout bounds
                // new local work; every replicated mutation also compares the
                // owner/term/generation row atomically because dropping a
                // submitted Raft future cannot prove that its command will
                // never commit.
                let deadline = tokio::time::Instant::from_std(claim.deadline());
                let repair_fence = claim.fence().clone();
                if deadline <= tokio::time::Instant::now() {
                    tracing::warn!(
                        item_id,
                        "artwork source repair fence expired before work began"
                    );
                    if let Err(error) = state
                        .membership
                        .retire_artwork_source_repair(&repair_fence)
                        .await
                    {
                        tracing::warn!(
                            item_id,
                            code = error.code(),
                            "cannot retire expired artwork source repair generation"
                        );
                    }
                    repair_after.insert(item_id, now + SOURCE_REPAIR_LEASE);
                    continue;
                }
                let outcome = tokio::time::timeout_at(
                    deadline,
                    state
                        .jobs
                        .refresh_item_artwork_fenced(item_id, &repair_fence),
                )
                .await;
                match state
                    .membership
                    .retire_artwork_source_repair(&repair_fence)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!(
                        item_id,
                        "artwork source repair generation was already retired"
                    ),
                    Err(error) => tracing::warn!(
                        item_id,
                        code = error.code(),
                        "cannot retire artwork source repair generation"
                    ),
                }
                let materialized =
                    futures_util::future::join_all(filenames.into_iter().map(|filename| {
                        let artwork_dir = state.artwork_dir.clone();
                        async move { valid_materialized_artwork(&artwork_dir, &filename).await }
                    }))
                    .await
                    .into_iter()
                    .all(|present| present);
                if matches!(outcome, Ok(Ok(_))) && materialized {
                    report.source_repairs += 1;
                    repair_after.insert(item_id, now + SOURCE_REPAIR_BACKOFF);
                } else {
                    match outcome {
                        Ok(Err(error)) => {
                            tracing::warn!(item_id, %error, "artwork source repair failed");
                        }
                        Err(_) => {
                            tracing::warn!(item_id, "artwork source repair exceeded its lease");
                        }
                        Ok(Ok(_)) => {
                            tracing::warn!(
                                item_id,
                                "artwork source repair produced no requested file"
                            );
                        }
                    }
                    repair_after.insert(item_id, now + SOURCE_REPAIR_LEASE);
                }
            }
            Ok(None) => {
                repair_after.insert(item_id, now + MATERIALIZE_INTERVAL);
            }
            Err(error) => {
                repair_after.insert(item_id, now + MATERIALIZE_INTERVAL);
                tracing::warn!(item_id, code = error.code(), "artwork repair lease failed");
            }
        }
    }
    Ok(report)
}

/// What committed membership says about this node, as three answers rather
/// than one collapsed boolean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArtworkMembershipView {
    /// A committed voter whose durable row is neither tombstoned nor fenced by
    /// an in-flight removal.
    active_voter: bool,
    /// A committed voter, whatever the durable row currently says.
    committed_voter: bool,
    /// Present in committed membership at all — as a voter or as a learner.
    committed_member: bool,
}

/// What one reconciliation tick does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtworkTick {
    Reconcile(ArtworkReconciliation),
    /// A membership change is fencing this node's durable row. Wait for it to
    /// commit or roll back rather than acting on either answer.
    Wait,
    /// This node is not in committed membership at all, so nothing here has an
    /// artwork directory worth converging.
    Stop,
}

/// Decide a tick from what membership says.
///
/// Split out so the distinction this makes is testable without a cluster. The
/// distinction is the point: "carries no vote" and "is no longer a member" are
/// different states, and only the second one ends the loop. A learner is
/// permanently in the first, and a learner promoted to voter has to start doing
/// provider work on its next tick without a process restart.
fn artwork_tick(view: ArtworkMembershipView) -> ArtworkTick {
    if view.active_voter {
        ArtworkTick::Reconcile(ArtworkReconciliation::Full)
    } else if view.committed_voter {
        ArtworkTick::Wait
    } else if view.committed_member {
        ArtworkTick::Reconcile(ArtworkReconciliation::LocalOnly)
    } else {
        ArtworkTick::Stop
    }
}

/// Read the three answers, asking only as many questions as the tick needs.
async fn observe_artwork_membership(
    membership: &plurx_core::cluster::membership::MembershipManager,
) -> Result<ArtworkMembershipView, plurx_core::cluster::membership::MembershipError> {
    if membership.local_node_is_active_voter().await? {
        return Ok(ArtworkMembershipView {
            active_voter: true,
            committed_voter: true,
            committed_member: true,
        });
    }
    if membership.local_node_is_committed_voter().await? {
        return Ok(ArtworkMembershipView {
            active_voter: false,
            committed_voter: true,
            committed_member: true,
        });
    }
    Ok(ArtworkMembershipView {
        active_voter: false,
        committed_voter: false,
        committed_member: membership.local_node_is_committed_member().await?,
    })
}

pub(crate) async fn materialize_loop(state: AppState) {
    let mut repair_after = HashMap::new();
    let mut item_cursor = 0;
    let mut orphan_walker = ArtworkOrphanWalker::default();
    tokio::time::sleep(Duration::from_secs(2)).await;
    loop {
        let scope = if state.membership.is_replicated() {
            let tick = match observe_artwork_membership(&state.membership).await {
                Ok(view) => artwork_tick(view),
                Err(error) => {
                    tracing::warn!(
                        code = error.code(),
                        "cannot verify artwork reconciliation authority"
                    );
                    tokio::time::sleep(MATERIALIZE_INTERVAL).await;
                    continue;
                }
            };
            match tick {
                ArtworkTick::Reconcile(scope) => scope,
                ArtworkTick::Wait => {
                    tokio::time::sleep(MATERIALIZE_INTERVAL).await;
                    continue;
                }
                ArtworkTick::Stop => {
                    tracing::info!(
                        "stopping artwork reconciliation: this node is no longer in committed \
                         cluster membership"
                    );
                    break;
                }
            }
        } else {
            ArtworkReconciliation::Full
        };
        match materialize_once(
            &state,
            scope,
            &mut repair_after,
            &mut item_cursor,
            &mut orphan_walker,
        )
        .await
        {
            Ok(report) if report.missing > 0 || report.orphans_removed > 0 => tracing::info!(
                references = report.references,
                missing = report.missing,
                copied = report.copied,
                deferred_capacity = report.deferred_capacity,
                source_repairs = report.source_repairs,
                unresolved = report.unresolved,
                orphans_removed = report.orphans_removed,
                "reconciled node-local artwork"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "artwork reconciliation failed"),
        }
        tokio::time::sleep(MATERIALIZE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Bytes;
    use axum::extract::Path;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use plurx_core::store::SqliteStore;

    use super::*;
    #[tokio::test]
    async fn sharing_art_opened_digest_refuses_symlinks_and_rehashes_inode_changes() {
        let _serial = super::super::shared_artwork::ART_FIXTURES.lock().await;
        let (_, state) = super::super::tests::test_app_with_state();
        let directory = crate::test_tempdir().expect("owned images");
        let path = directory.path().join("private.png");
        let bytes = b"\x89PNG\r\n\x1a\nfirst";
        std::fs::write(&path, bytes).expect("first");
        let first = shared_opened_art(
            &state,
            path.clone(),
            "private.png",
            super::super::shared_artwork::ArtBodyLease::source(false).expect("lease"),
        )
        .await
        .expect("read")
        .expect("asset");
        assert_eq!(first.digest, <[u8; 32]>::from(Sha256::digest(bytes)));
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nother").expect("same inode same size rewrite");
        assert!(!first.still_current().await);
        drop(first);
        let second = shared_opened_art(
            &state,
            path.clone(),
            "private.png",
            super::super::shared_artwork::ArtBodyLease::source(false).expect("lease"),
        )
        .await
        .expect("read")
        .expect("asset");
        assert_ne!(second.digest, <[u8; 32]>::from(Sha256::digest(bytes)));
        drop(second);
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, bytes).expect("replace");
        std::fs::rename(replacement, &path).expect("rename");
        assert_eq!(
            shared_opened_art(
                &state,
                path.clone(),
                "private.png",
                super::super::shared_artwork::ArtBodyLease::source(false).expect("lease")
            )
            .await
            .expect("read")
            .expect("asset")
            .digest,
            <[u8; 32]>::from(Sha256::digest(bytes))
        );
        #[cfg(unix)]
        {
            std::fs::remove_file(&path).expect("remove");
            std::os::unix::fs::symlink(directory.path().join("outside"), &path).expect("symlink");
            assert!(shared_opened_art(
                &state,
                path.clone(),
                "private.png",
                super::super::shared_artwork::ArtBodyLease::source(false).expect("lease")
            )
            .await
            .expect("closed")
            .is_none());
        }
        let huge = std::fs::File::create(directory.path().join("huge.png")).expect("huge");
        huge.set_len(MAX_ARTWORK_BYTES + 1).expect("size");
        assert!(shared_opened_art(
            &state,
            directory.path().join("huge.png"),
            "huge.png",
            super::super::shared_artwork::ArtBodyLease::source(false).expect("lease")
        )
        .await
        .expect("closed")
        .is_none());
    }

    #[test]
    fn artwork_inventory_deduplicates_bare_names_and_rejects_paths() {
        let references = artwork_references(vec![
            ArtworkInventoryItem {
                id: 10,
                poster_path: Some("shared.jpg".to_owned()),
                backdrop_path: Some("nested/rejected.jpg".to_owned()),
            },
            ArtworkInventoryItem {
                id: 20,
                poster_path: Some("shared.jpg".to_owned()),
                backdrop_path: Some("hero.jpg".to_owned()),
            },
        ]);
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].filename, "hero.jpg");
        assert_eq!(references[0].item_id, 20);
        assert_eq!(references[1].filename, "shared.jpg");
        assert_eq!(references[1].item_id, 10);
    }

    #[test]
    fn orphan_sweep_only_matches_exact_plurx_artwork_names() {
        let digest = "a".repeat(64);
        assert!(managed_artwork_filename("84-poster.jpg"));
        assert!(managed_artwork_filename("84-backdrop.png"));
        assert!(managed_artwork_filename(&format!(
            "84-poster-{digest}.webp"
        )));
        assert!(!managed_artwork_filename("2024-family.jpg"));
        assert!(!managed_artwork_filename(".84-poster.jpg"));
        assert!(!managed_artwork_filename("84-poster-short.jpg"));
        assert!(!managed_artwork_filename("0-poster.jpg"));
        assert!(!managed_artwork_filename("-84-poster.jpg"));
    }

    #[test]
    fn bounded_orphan_classifier_authenticates_both_generation_formats() {
        let bytes = b"immutable artwork generation";
        let digest = hex::encode(Sha256::digest(bytes));
        let scoped = format!("84-poster-c{}.jpg", &digest[..16]);
        let curator = format!("84-poster-{digest}.webp");

        assert!(content_addressed_artwork_name(&scoped));
        assert!(content_addressed_artwork_name(&curator));
        assert!(artwork_bytes_match_name(&scoped, bytes));
        assert!(artwork_bytes_match_name(&curator, bytes));
        assert!(!artwork_bytes_match_name(&scoped, b"corrupt"));
        assert!(!artwork_bytes_match_name(&curator, b"corrupt"));

        assert!(!content_addressed_artwork_name(&format!(
            "84-backdrop-{digest}.jpg"
        )));
        assert!(!content_addressed_artwork_name(&format!(
            "family-poster-{digest}.jpg"
        )));
        assert!(!content_addressed_artwork_name(
            "84-poster-c0123456789abcdef.txt"
        ));
    }

    #[tokio::test]
    async fn a_verified_hit_is_not_rehashed_and_a_validator_skips_the_read() {
        let coordinator = ArtworkCoordinator::new();
        let directory = crate::test_tempdir().expect("artwork");
        let filename = "84-poster.jpg";
        tokio::fs::write(directory.path().join(filename), b"legacy artwork")
            .await
            .expect("artwork");

        let first = serve_local_artwork(
            &coordinator,
            directory.path(),
            filename,
            &HeaderMap::new(),
            ArtworkRoute::User,
        )
        .await
        .expect("first response");
        let etag = first.headers()[header::ETAG].clone();
        assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 1);
        drop(first);

        let second = serve_local_artwork(
            &coordinator,
            directory.path(),
            filename,
            &HeaderMap::new(),
            ArtworkRoute::User,
        )
        .await
        .expect("cached response");
        assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 1);
        drop(second);

        let mut conditional = HeaderMap::new();
        conditional.insert(header::IF_NONE_MATCH, etag.clone());
        let response = serve_local_artwork(
            &coordinator,
            directory.path(),
            filename,
            &conditional,
            ArtworkRoute::User,
        )
        .await
        .expect("conditional response");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::ETAG], etag);
        assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 1);
        assert_eq!(
            coordinator.local_bytes.available_permits(),
            LOCAL_READ_BUDGET_KIB
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_in_place_rewrite_with_a_new_ctime_is_reverified_and_quarantined() {
        let coordinator = ArtworkCoordinator::new();
        let directory = crate::test_tempdir().expect("artwork");
        let original = b"authenticated artwork";
        let digest = hex::encode(Sha256::digest(original));
        let filename = format!("84-poster-{digest}.webp");
        let path = directory.path().join(&filename);
        tokio::fs::write(&path, original).await.expect("original");
        let first = serve_local_artwork(
            &coordinator,
            directory.path(),
            &filename,
            &HeaderMap::new(),
            ArtworkRoute::User,
        )
        .await
        .expect("verified original");
        drop(first);
        tokio::time::sleep(Duration::from_millis(2)).await;
        tokio::fs::write(&path, b"corrupted artwork!!!!")
            .await
            .expect("in-place replacement");

        let error = serve_local_artwork(
            &coordinator,
            directory.path(),
            &filename,
            &HeaderMap::new(),
            ArtworkRoute::User,
        )
        .await
        .expect_err("corrupt rewrite is refused");
        assert!(matches!(error, ApiError::NotFound("image")));
        assert!(!path.exists());
        assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_replaced_inode_is_reverified_and_install_evicts_the_entry() {
        let coordinator = ArtworkCoordinator::new();
        let directory = crate::test_tempdir().expect("artwork");
        let filename = "84-poster.jpg";
        tokio::fs::write(directory.path().join(filename), b"old legacy artwork")
            .await
            .expect("old artwork");
        let first = serve_local_artwork(
            &coordinator,
            directory.path(),
            filename,
            &HeaderMap::new(),
            ArtworkRoute::User,
        )
        .await
        .expect("old response");
        let old_etag = first.headers()[header::ETAG].clone();
        drop(first);
        assert!(coordinator
            .verified
            .lock()
            .await
            .entries
            .contains_key(filename));

        install_artwork(
            &coordinator,
            directory.path(),
            filename,
            b"new legacy artwork",
        )
        .await
        .expect("new artwork");
        assert!(!coordinator
            .verified
            .lock()
            .await
            .entries
            .contains_key(filename));
        let second = serve_local_artwork(
            &coordinator,
            directory.path(),
            filename,
            &HeaderMap::new(),
            ArtworkRoute::User,
        )
        .await
        .expect("new response");
        assert_ne!(second.headers()[header::ETAG], old_etag);
        assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn orphan_classifier_only_accepts_exact_corrupt_quarantine_shape() {
        let digest = "a".repeat(64);
        let managed = format!("84-poster-{digest}.webp");
        let quarantine = format!(".{managed}.corrupt-{}", "b".repeat(32));
        assert!(corrupt_artwork_quarantine_name(&quarantine));
        assert!(temporary_artwork_orphan_name(&quarantine));
        assert!(!temporary_artwork_orphan_name(&format!(
            ".README.txt.corrupt-{}",
            "b".repeat(32)
        )));
        assert!(!temporary_artwork_orphan_name(&format!(
            ".{managed}.corrupt-too-short"
        )));
    }

    #[test]
    fn w300_never_upscales_a_small_source() {
        let mut png = vec![0_u8; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&200_u32.to_be_bytes());
        png[20..24].copy_from_slice(&120_u32.to_be_bytes());
        assert!(should_serve_original(ArtworkSize::W300, "poster.png", &png));
        assert!(!should_serve_original(
            ArtworkSize::W300,
            "poster.jpg",
            &png
        ));
    }

    #[test]
    fn derivative_key_is_the_verified_source_digest() {
        // Both sides of every assertion here are written out, not derived from
        // the same expression as the value under test: an assertion that
        // recomputes `[..32]` from `hex::encode(first)` holds whatever prefix
        // width the code actually uses, and so could not have caught the
        // sweep's 16-against-32 comparison.
        let first: [u8; 32] = Sha256::digest(b"first legacy bytes").into();
        let second: [u8; 32] = Sha256::digest(b"second legacy bytes").into();
        let first_key =
            derivative_filename(first, ArtworkSize::W500, "84-poster.jpg").expect("first key");
        let second_key =
            derivative_filename(second, ArtworkSize::W500, "84-poster.jpg").expect("second key");
        assert_eq!(
            first_key, "84-poster.jpg-w500-107457d3b119e162368e875adc2c8cf3.jpg",
            "the key names its source and the first 32 hex characters of the source digest"
        );
        assert_eq!(
            second_key,
            "84-poster.jpg-w500-9343c668c053f08a3e30c21aa418332f.jpg"
        );
        assert_ne!(first_key, second_key, "changed source bytes mint a new key");

        let parsed = parse_derivative_name(&first_key).expect("parse");
        assert_eq!(parsed.source, "84-poster.jpg");
        assert_eq!(parsed.prefix, "107457d3b119e162368e875adc2c8cf3");
        // The comparison width the sweep uses has to be one a `-c{16hex}`
        // source can supply.
        assert_eq!(DERIVATIVE_PREFIX_MATCH, 16);
        assert_eq!(
            &parsed.prefix[..DERIVATIVE_PREFIX_MATCH],
            "107457d3b119e162"
        );

        // A `{item}-{kind}-c{16hex}` source publishes exactly the sixteen
        // characters the sweep compares, and a derivative of it round-trips.
        let backdrop = format!("84-backdrop-c{}.jpg", "107457d3b119e162");
        let backdrop_key =
            derivative_filename(first, ArtworkSize::W780, &backdrop).expect("backdrop key");
        let parsed = parse_derivative_name(&backdrop_key).expect("parse backdrop");
        assert_eq!(parsed.source, backdrop);
        assert_eq!(
            &parsed.prefix[..DERIVATIVE_PREFIX_MATCH],
            "107457d3b119e162"
        );

        assert_eq!(parse_derivative_name("84-poster.jpg"), None);
        assert_eq!(
            parse_derivative_name(&format!(
                "84-poster.jpg-w999-{}.jpg",
                "107457d3b119e162368e875adc2c8cf3"
            )),
            None
        );
        assert!(superseded_derivative_name(
            "107457d3b119e162368e875adc2c8cf3-w300.jpg"
        ));
        assert!(!superseded_derivative_name(&first_key));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_ffmpeg_leaves_no_derivative() {
        let coordinator = ArtworkCoordinator::new();
        let directory = crate::test_tempdir().expect("artwork");
        let source_name = "84-poster.png";
        let source_path = directory.path().join(source_name);
        let source = b"not decoded because the child fails first";
        tokio::fs::write(&source_path, source)
            .await
            .expect("source");
        let derived_dir = ensure_derived_dir(directory.path())
            .await
            .expect("derived directory");
        let digest: [u8; 32] = Sha256::digest(source).into();
        let derived_name =
            derivative_filename(digest, ArtworkSize::W300, source_name).expect("derivative name");
        let source_identity = open_local_artwork(source_path.clone())
            .await
            .expect("source identity")
            .identity;
        generate_derivative_with_bin(
            &coordinator,
            DerivativeJob {
                runtime_cache: directory.path(),
                source_path: &source_path,
                source_name,
                source_identity,
                derived_dir: &derived_dir,
                derived_name: &derived_name,
                size: ArtworkSize::W300,
            },
            "/bin/false",
        )
        .await
        .expect_err("failed child");
        assert!(!derived_dir.join(derived_name).exists());
        assert!(
            std::fs::read_dir(derived_dir)
                .expect("derived entries")
                .next()
                .is_none(),
            "failed generation must remove its temporary output"
        );
    }

    #[tokio::test]
    async fn ffmpeg_derivative_is_bounded_and_preserves_the_original() {
        let coordinator = ArtworkCoordinator::new();
        let directory = crate::test_tempdir().expect("artwork");
        let source_name = "84-poster.png";
        let source_path = directory.path().join(source_name);
        let status = std::process::Command::new(crate::ffmpeg::ffmpeg_bin())
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=640x360",
                "-frames:v",
                "1",
                "-y",
            ])
            .arg(&source_path)
            .status()
            .expect("fixture ffmpeg");
        assert!(status.success());
        let original = tokio::fs::read(&source_path).await.expect("source bytes");
        let source =
            match read_verified_local_artwork(&coordinator, source_path.clone(), source_name).await
            {
                LocalArtworkRead::Verified(source) => source,
                _ => panic!("verified fixture"),
            };
        let digest = source.digest;
        drop(source);
        let derived_dir = ensure_derived_dir(directory.path())
            .await
            .expect("derived directory");
        let derived_name =
            derivative_filename(digest, ArtworkSize::W300, source_name).expect("derivative name");
        let source_identity = open_local_artwork(source_path.clone())
            .await
            .expect("source identity")
            .identity;
        generate_derivative(
            &coordinator,
            DerivativeJob {
                runtime_cache: directory.path(),
                source_path: &source_path,
                source_name,
                source_identity,
                derived_dir: &derived_dir,
                derived_name: &derived_name,
                size: ArtworkSize::W300,
            },
        )
        .await
        .expect("generate derivative");
        let derived = tokio::fs::read(derived_dir.join(&derived_name))
            .await
            .expect("derived bytes");
        assert_eq!(image_dimensions(&derived_name, &derived), Some((300, 168)));
        assert!(derived.len() as u64 <= MAX_ARTWORK_BYTES);
        assert_eq!(
            tokio::fs::read(source_path).await.expect("source survives"),
            original
        );
    }

    #[tokio::test]
    async fn failed_corruption_restore_leaves_a_sweep_eligible_quarantine() {
        let coordinator = ArtworkCoordinator::new();
        let directory = crate::test_tempdir().expect("artwork");
        let digest = hex::encode(Sha256::digest(b"authenticated artwork"));
        let filename = format!("84-poster-{digest}.webp");
        let path = directory.path().join(&filename);
        std::fs::write(&path, b"corrupt original").expect("corrupt original");
        let expected = artwork_file_identity(&std::fs::metadata(&path).expect("metadata"));
        let swap_directory = directory.path().to_path_buf();
        let swap_filename = filename.clone();

        quarantine_corrupt_artwork_with(
            &coordinator,
            directory.path(),
            &filename,
            expected,
            Some(Box::new(move |quarantine| {
                std::fs::remove_file(swap_directory.join(quarantine))
                    .expect("replace quarantine inode");
                std::fs::write(swap_directory.join(quarantine), b"changed quarantine")
                    .expect("changed quarantine");
                std::fs::write(swap_directory.join(&swap_filename), b"newer target")
                    .expect("competing target");
            })),
        )
        .await;

        assert_eq!(std::fs::read(&path).expect("new target"), b"newer target");
        let quarantines = std::fs::read_dir(directory.path())
            .expect("artwork entries")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|entry| entry != &filename)
            .collect::<Vec<_>>();
        assert_eq!(quarantines.len(), 1);
        assert!(
            temporary_artwork_orphan_name(&quarantines[0]),
            "a failed no-replace restore must remain eligible for bounded cleanup"
        );
    }

    #[tokio::test]
    async fn versioned_repair_name_invalidates_old_bytes_on_every_voter() {
        let old = "84-poster-aaaaaaaaaaaaaaaa.jpg";
        let new = "84-poster-bbbbbbbbbbbbbbbb.jpg";
        let voters = [
            crate::test_tempdir().expect("voter one"),
            crate::test_tempdir().expect("voter two"),
        ];
        for voter in &voters {
            tokio::fs::write(voter.path().join(old), b"old edition")
                .await
                .expect("old cover");
        }
        let references = artwork_references(vec![ArtworkInventoryItem {
            id: 84,
            poster_path: Some(new.to_owned()),
            backdrop_path: None,
        }]);
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].filename, new);
        for voter in &voters {
            assert!(voter.path().join(old).is_file(), "stale bytes still exist");
            assert!(
                tokio::fs::metadata(voter.path().join(&references[0].filename))
                    .await
                    .is_err(),
                "the replicated content-versioned name must be missing on every stale voter"
            );
        }
    }

    #[tokio::test]
    async fn orphan_sweep_honors_publication_reservations_and_removes_old_generations() {
        let store = SqliteStore::open_in_memory().expect("store");
        let directory = crate::test_tempdir().expect("artwork");
        let orphan = directory
            .path()
            .join(format!("84-poster-{}.jpg", "a".repeat(64)));
        tokio::fs::write(&orphan, b"superseded")
            .await
            .expect("orphan");
        tokio::fs::write(directory.path().join("README.txt"), b"not managed")
            .await
            .expect("sentinel");
        let reservation =
            plurx_core::metadata::reserve_artwork_publication(orphan.clone()).expect("reserve");
        assert_eq!(
            sweep_orphan_artwork(&store, directory.path(), Duration::ZERO).await,
            0,
            "an active publisher owns the path"
        );
        assert!(orphan.is_file());
        drop(reservation);

        assert_eq!(
            sweep_orphan_artwork(&store, directory.path(), Duration::ZERO).await,
            1
        );
        assert!(!orphan.exists());
        assert!(directory.path().join("README.txt").is_file());
    }

    #[tokio::test]
    async fn orphan_candidate_rechecks_age_after_a_competing_publication() {
        let store = SqliteStore::open_in_memory().expect("store");
        let directory = crate::test_tempdir().expect("artwork");
        let filename = format!("84-poster-{}.jpg", "b".repeat(64));
        let path = directory.path().join(&filename);
        let grace = Duration::from_secs(60);
        tokio::fs::write(&path, b"old generation")
            .await
            .expect("old candidate");
        let old_modified = std::time::SystemTime::now()
            .checked_sub(Duration::from_secs(120))
            .expect("old modified time");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open old candidate")
            .set_times(std::fs::FileTimes::new().set_modified(old_modified))
            .expect("age old candidate");
        assert!(orphan_candidate_old_enough(&path, grace).await);

        plurx_core::metadata::write_artwork_atomically(&path, b"fresh generation")
            .await
            .expect("competing publication");
        assert!(
            !remove_orphan_candidate(&store, &filename, &path, grace).await,
            "the discovery-time age must not authorize deletion after replacement"
        );
        assert_eq!(
            tokio::fs::read(&path).await.expect("fresh file survives"),
            b"fresh generation"
        );
    }

    #[tokio::test]
    async fn simultaneous_same_filename_misses_fetch_and_install_once() {
        let coordinator = ArtworkCoordinator::new();
        let directory = Arc::new(crate::test_tempdir().expect("artwork directory"));
        let hits = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..PEER_FETCH_CONCURRENCY {
            let coordinator = Arc::clone(&coordinator);
            let directory = Arc::clone(&directory);
            let hits = Arc::clone(&hits);
            tasks.push(tokio::spawn(async move {
                fetch_and_materialize(
                    &coordinator,
                    directory.path(),
                    "singleflight.jpg",
                    move |_| async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Some(b"one peer response".to_vec())
                    },
                )
                .await
                .expect("artwork admission")
                .expect("materialized bytes")
            }));
        }
        for task in tasks {
            let bytes = task.await.expect("request task");
            assert_eq!(bytes.as_ref(), b"one peer response");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            tokio::fs::read(directory.path().join("singleflight.jpg"))
                .await
                .expect("installed artwork"),
            b"one peer response"
        );
    }

    #[tokio::test]
    async fn distinct_filename_fetches_obey_the_global_buffer_bound() {
        let coordinator = ArtworkCoordinator::new();
        let directory = Arc::new(crate::test_tempdir().expect("artwork directory"));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for ordinal in 0..24 {
            let coordinator = Arc::clone(&coordinator);
            let directory = Arc::clone(&directory);
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            tasks.push(tokio::spawn(async move {
                let filename = format!("bounded-{ordinal}.jpg");
                fetch_and_materialize(&coordinator, directory.path(), &filename, move |_| {
                    let active = Arc::clone(&active);
                    let maximum = Arc::clone(&maximum);
                    async move {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Some(vec![ordinal as u8 + 1])
                    }
                })
                .await
                .map(|bytes| bytes.expect("bounded fetch"))
            }));
        }
        let mut admitted = 0usize;
        let mut rejected = 0usize;
        for task in tasks {
            match task.await.expect("request task") {
                Ok(_) => admitted += 1,
                Err(ArtworkCapacity) => rejected += 1,
            }
        }
        assert_eq!(maximum.load(Ordering::SeqCst), PEER_FETCH_CONCURRENCY);
        assert_eq!(admitted, PEER_FETCH_CONCURRENCY);
        assert_eq!(rejected, 24 - PEER_FETCH_CONCURRENCY);
    }

    #[tokio::test]
    async fn a_derive_permit_wait_is_timed_into_the_image_materialize_pool() {
        use crate::telemetry::{admission_waits_for_test, AdmissionPool};
        let coordinator = ArtworkCoordinator::new();
        let before = admission_waits_for_test(AdmissionPool::ImageMaterialize);
        let _permit = coordinator
            .derive_permit()
            .await
            .expect("an idle pool admits");
        let after = admission_waits_for_test(AdmissionPool::ImageMaterialize);
        assert!(after > before, "{before} -> {after}");
    }

    /// A derive that queues behind a full pool and is abandoned there (its
    /// request dropped) is still one observation.
    #[tokio::test]
    async fn an_abandoned_derive_permit_wait_is_still_timed() {
        use crate::telemetry::{admission_waits_on_this_thread, AdmissionPool};
        use std::future::Future;
        let coordinator = ArtworkCoordinator::new();
        let held = Arc::clone(&coordinator.derive_permits)
            .acquire_many_owned(
                u32::try_from(coordinator.derive_permits.available_permits())
                    .expect("permit count"),
            )
            .await
            .expect("take the whole pool");
        let before = admission_waits_on_this_thread(AdmissionPool::ImageMaterialize);
        {
            let mut queued = Box::pin(coordinator.derive_permit());
            let parked = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(queued.as_mut().poll(cx).is_pending())
            })
            .await;
            assert!(parked, "the derive queued behind the full pool");
        }
        assert_eq!(
            admission_waits_on_this_thread(AdmissionPool::ImageMaterialize) - before,
            1,
            "the abandoned wait is recorded once"
        );
        drop(held);
    }

    #[tokio::test]
    async fn local_reads_are_bounded_by_bytes_not_count() {
        let coordinator = ArtworkCoordinator::new();
        let mut small = Vec::new();
        for _ in 0..12 {
            small.push(
                coordinator
                    .local_permit(100 * 1024)
                    .await
                    .expect("twelve small reads fit the byte budget"),
            );
        }
        drop(small);

        let mut large = Vec::new();
        for _ in 0..4 {
            large.push(
                coordinator
                    .local_permit(MAX_ARTWORK_BYTES)
                    .await
                    .expect("four maximum-size reads fit"),
            );
        }
        assert!(
            coordinator.local_permit(MAX_ARTWORK_BYTES).await.is_none(),
            "the fifth maximum-size read must time out at the byte budget"
        );
        drop(large.pop());
        assert!(coordinator.local_permit(MAX_ARTWORK_BYTES).await.is_some());
    }

    #[tokio::test]
    async fn saturated_response_capacity_defers_reconciliation_repairs() {
        let coordinator = ArtworkCoordinator::new();
        let directory = crate::test_tempdir().expect("artwork directory");
        let mut held = Vec::new();
        for _ in 0..PEER_FETCH_CONCURRENCY {
            held.push(coordinator.peer_permit().expect("hold peer permit"));
        }
        tokio::fs::write(directory.path().join("local.jpg"), b"local")
            .await
            .expect("local artwork");
        let local = serve_local_artwork(
            &coordinator,
            directory.path(),
            "local.jpg",
            &HeaderMap::new(),
            ArtworkRoute::User,
        )
        .await
        .expect("peer saturation must not refuse local bytes");
        drop(local);
        let outcome = artwork_pull_outcome(
            fetch_and_materialize(&coordinator, directory.path(), "capacity.jpg", |_| async {
                Some(b"peer bytes".to_vec())
            })
            .await,
        );
        assert_eq!(outcome, ArtworkPullOutcome::DeferredCapacity);

        let item_id = 42;
        let mut unresolved = BTreeMap::from([(item_id, vec!["capacity.jpg".to_owned()])]);
        let mut repair_after = HashMap::from([(item_id, Instant::now())]);
        suppress_deferred_artwork_repairs(
            BTreeSet::from([item_id]),
            &mut unresolved,
            &mut repair_after,
        );
        assert!(unresolved.is_empty());
        assert!(repair_after.is_empty());
        drop(held);
    }

    #[test]
    fn missing_artwork_becomes_repairable_after_the_grace_pass() {
        let item_id = 77;
        let missing = || BTreeMap::from([(item_id, vec!["77-poster.jpg".to_owned()])]);
        let first_pass = Instant::now();
        let mut repair_after = HashMap::new();
        assert!(due_artwork_repairs(missing(), &mut repair_after, first_pass).is_empty());
        assert_eq!(
            repair_after.get(&item_id).copied(),
            Some(first_pass + SOURCE_REPAIR_GRACE)
        );

        let after_grace = first_pass + SOURCE_REPAIR_GRACE + Duration::from_millis(1);
        retain_recent_artwork_repairs(&mut repair_after, after_grace);
        let due = due_artwork_repairs(missing(), &mut repair_after, after_grace);
        assert_eq!(due.get(&item_id), Some(&vec!["77-poster.jpg".to_owned()]));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn provider_repair_validation_rejects_symlinks_and_oversized_outputs() {
        let directory = crate::test_tempdir().expect("artwork directory");
        let outside = tempfile::NamedTempFile::new().expect("outside artwork");
        std::fs::write(outside.path(), b"outside bytes").expect("outside bytes");
        std::os::unix::fs::symlink(outside.path(), directory.path().join("symlink.jpg"))
            .expect("symlink output");
        assert!(!valid_materialized_artwork(directory.path(), "symlink.jpg").await);

        let oversized = std::fs::File::create(directory.path().join("oversized.jpg"))
            .expect("oversized output");
        oversized
            .set_len(MAX_ARTWORK_BYTES + 1)
            .expect("extend sparse artwork");
        assert!(!valid_materialized_artwork(directory.path(), "oversized.jpg").await);
    }

    #[test]
    fn versioned_artwork_is_private_and_immutable() {
        let response = artwork_response(
            FsPath::new("287-poster.jpg"),
            b"\xff\xd8\xff artwork".to_vec(),
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/jpeg");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, max-age=604800, immutable"
        );
    }

    #[test]
    fn peer_url_keeps_a_reverse_proxy_prefix_and_escapes_the_filename() {
        let url =
            peer_artwork_url("https://plurx-b.test/cinema/", "poster one.jpg").expect("peer URL");
        assert_eq!(
            url.as_str(),
            "https://plurx-b.test/cinema/api/v1/cluster/artwork/poster%20one.jpg"
        );
        assert!(peer_artwork_url("file:///tmp/plurx", "poster.jpg").is_none());
    }

    #[test]
    fn peer_proof_headers_are_complete_and_typed() {
        let mut headers = HeaderMap::new();
        headers.insert(NODE_ID_HEADER, "node-b".parse().expect("node"));
        headers.insert(TIMESTAMP_HEADER, "12345".parse().expect("time"));
        headers.insert(SIGNATURE_HEADER, "aabb".parse().expect("signature"));
        assert_eq!(
            peer_auth_from_headers(&headers),
            Some(ArtworkPeerAuth {
                node_id: "node-b".to_owned(),
                timestamp_ms: 12_345,
                signature: "aabb".to_owned(),
            })
        );
        headers.remove(SIGNATURE_HEADER);
        assert!(peer_auth_from_headers(&headers).is_none());
    }

    #[tokio::test]
    async fn sharing_art_single_peer_collector_keeps_owned_capacity_and_refuses_invalid_bytes() {
        let _fixture = super::super::shared_artwork::ART_FIXTURES.lock().await;
        async fn peer(Path(name): Path<String>, headers: HeaderMap) -> Response {
            if headers
                .get(SIGNATURE_HEADER)
                .and_then(|value| value.to_str().ok())
                != Some("proof")
            {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            if name == "redirect.jpg" {
                return (StatusCode::FOUND, [(header::LOCATION, "/original.jpg")]).into_response();
            }
            let bytes = if name == "invalid.jpg" {
                Bytes::from_static(b"not an image")
            } else {
                Bytes::from_static(b"\xff\xd8\xff bounded peer")
            };
            ([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/art/{name}", get(peer)))
                .await
                .expect("server");
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("client");
        let lease = super::super::shared_artwork::ArtBodyLease::source(false).expect("lease");
        let owner = Arc::new(SharedArtDownloadOwner {
            _lease: lease,
            _local: None,
            _peer: None,
            _workspace: super::super::shared_artwork::ArtBodyLease::source_collection_workspace()
                .expect("workspace"),
        });
        let auth = ArtworkPeerAuth {
            node_id: "peer".to_owned(),
            timestamp_ms: 123,
            signature: "proof".to_owned(),
        };
        for name in ["invalid.jpg", "redirect.jpg"] {
            assert!(shared_peer_snapshot(
                &client,
                reqwest::Url::parse(&format!("http://{address}/art/{name}")).expect("url"),
                &auth,
                name,
                owner.clone()
            )
            .await
            .is_none());
        }
        let download = shared_peer_snapshot(
            &client,
            reqwest::Url::parse(&format!("http://{address}/art/original.jpg")).expect("url"),
            &auth,
            "original.jpg",
            owner.clone(),
        )
        .await
        .expect("validated collector");
        assert_eq!(download.as_ref(), b"\xff\xd8\xff bounded peer");
        drop(owner);
        assert_eq!(super::super::shared_artwork::source_capacity().0, 3);
        let directory = crate::test_tempdir().expect("directory");
        plurx_core::fs_secure::atomic_write_child_owned(directory.path(), "original.jpg", download)
            .await
            .expect("owned publication");
        assert_eq!(
            tokio::fs::read(directory.path().join("original.jpg"))
                .await
                .expect("bytes"),
            b"\xff\xd8\xff bounded peer"
        );
        assert_eq!(super::super::shared_artwork::source_capacity().0, 4);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn a_peer_miss_sends_node_proof_and_materializes_complete_bytes() {
        let coordinator = ArtworkCoordinator::new();
        async fn peer(
            Path(filename): Path<String>,
            headers: HeaderMap,
        ) -> (StatusCode, HeaderMap, Bytes) {
            let authorized = headers
                .get(NODE_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                == Some("node-b")
                && headers
                    .get(TIMESTAMP_HEADER)
                    .and_then(|value| value.to_str().ok())
                    == Some("12345")
                && headers
                    .get(SIGNATURE_HEADER)
                    .and_then(|value| value.to_str().ok())
                    == Some("aabb");
            if !authorized || filename != "poster one.jpg" {
                return (StatusCode::UNAUTHORIZED, HeaderMap::new(), Bytes::new());
            }
            let mut response_headers = HeaderMap::new();
            response_headers.insert(header::CONTENT_TYPE, "image/jpeg".parse().expect("mime"));
            (
                StatusCode::OK,
                response_headers,
                Bytes::from_static(b"\xff\xd8\xff peer artwork"),
            )
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/cinema/api/v1/cluster/artwork/{filename}", get(peer)),
            )
            .await
            .expect("peer server");
        });

        let client = artwork_client().expect("client");
        let bytes = fetch_peer_artwork_with_auth(
            &client,
            &["not a URL".to_owned(), format!("http://{address}/cinema")],
            "poster one.jpg",
            &ArtworkPeerAuth {
                node_id: "node-b".to_owned(),
                timestamp_ms: 12_345,
                signature: "aabb".to_owned(),
            },
        )
        .await
        .expect("peer bytes");
        assert_eq!(bytes, b"\xff\xd8\xff peer artwork");

        let directory = crate::test_tempdir().expect("artwork directory");
        install_artwork(&coordinator, directory.path(), "poster one.jpg", &bytes)
            .await
            .expect("materialize");
        assert_eq!(
            tokio::fs::read(directory.path().join("poster one.jpg"))
                .await
                .expect("cached artwork"),
            bytes
        );
        server.abort();
    }

    // ---- M2 derivative route ------------------------------------------------
    // `serve_derivative` needs a real `AppState`: the store behind the orphan
    // sweep, the artwork root, and the dedicated runtime cache the child runs
    // against.
    pub(super) fn derivative_state() -> AppState {
        let store = SqliteStore::open_in_memory().expect("store");
        let base = crate::test_temp_path(format!("plurx-derived-{}", uuid::Uuid::new_v4()));
        let dirs = crate::state::Dirs {
            artwork: base.join("artwork"),
            transcode: base.join("transcode"),
            cache: base.join("cache"),
            subs: base.join("subs"),
            runtime_cache: base.join("runtime"),
            renditions: base.join("renditions"),
        };
        std::fs::create_dir_all(&dirs.artwork).expect("artwork directory");
        AppState::new(
            "test".into(),
            Arc::new(store),
            dirs,
            "test-node".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        )
    }

    /// One real image, written by the shipped ffmpeg, as the plan's §5.2
    /// "generated fixtures only" requires.
    pub(super) fn write_test_image(path: &FsPath, source: &str, extra: &[&str]) {
        let mut command = std::process::Command::new(crate::ffmpeg::ffmpeg_bin());
        command.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            source,
        ]);
        command.args(extra);
        command.args(["-y"]).arg(path);
        let status = command.status().expect("fixture ffmpeg");
        assert!(status.success(), "fixture ffmpeg failed for {source}");
    }

    async fn response_body(response: Response) -> Vec<u8> {
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec()
    }

    fn header_value(response: &Response, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn a_failed_ffmpeg_serves_the_original() {
        // Bytes no decoder will accept, so the real child exits non-zero and
        // the handler takes its fallback path.
        let state = derivative_state();
        let source_name = "84-poster.jpg";
        let original = b"\xff\xd8\xff not actually a JPEG".to_vec();
        tokio::fs::write(state.artwork_dir.join(source_name), &original)
            .await
            .expect("source");

        let response = serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
            .await
            .expect("a failed resize must not break the grid");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_value(&response, "x-plurx-artwork").as_deref(),
            Some("original-fallback")
        );
        // The heart of it: these are the *original* bytes under a `?size=`
        // URL. Caching them as `immutable` for a week would make every cold
        // grid load poison its own cache with the full-size image and never
        // ask for the derivative that generation will eventually produce.
        assert_eq!(
            header_value(&response, "cache-control").as_deref(),
            Some("private, max-age=0, must-revalidate"),
            "a substituted original must revalidate, not be frozen for a week"
        );
        assert_eq!(response_body(response).await, original);
        assert!(!state
            .artwork_dir
            .join(DERIVED_DIR)
            .join("84-poster.jpg-w300")
            .exists());
    }

    #[tokio::test]
    async fn derivatives_are_single_flight() {
        let state = derivative_state();
        let source_name = "84-backdrop.png";
        let source_path = state.artwork_dir.join(source_name);
        write_test_image(&source_path, "testsrc2=size=1280x720", &["-frames:v", "1"]);

        let headers = HeaderMap::new();
        let requests =
            (0..10).map(|_| serve_derivative(&state, source_name, &headers, ArtworkSize::W300));
        let responses = futures_util::future::join_all(requests).await;
        for response in responses {
            let response = response.expect("derivative");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                header_value(&response, "x-plurx-artwork"),
                Some("original-fallback".to_owned()),
                "cold requests return originals while one durable intent prepares the variant"
            );
        }
        assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 0);
        assert!(worker::run_one(&state).await.expect("worker pass"));
        assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 1);
        assert!(!worker::run_one(&state)
            .await
            .expect("no duplicate execution"));
        let ready = serve_derivative(&state, source_name, &headers, ArtworkSize::W300)
            .await
            .expect("ready variant");
        assert_eq!(header_value(&ready, "x-plurx-artwork"), None);
    }

    #[tokio::test]
    async fn a_warm_derivative_hit_does_not_read_the_source() {
        let state = derivative_state();
        let source_name = "84-backdrop.png";
        let source_path = state.artwork_dir.join(source_name);
        write_test_image(&source_path, "testsrc2=size=1920x1080", &["-frames:v", "1"]);
        let source_kib = tokio::fs::metadata(&source_path)
            .await
            .expect("source metadata")
            .len()
            .div_ceil(1024);

        // A cold request records demand; the worker publishes verified bytes.
        drop(
            serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
                .await
                .expect("cold request"),
        );
        assert!(worker::run_one(&state).await.expect("worker pass"));
        let first = serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
            .await
            .expect("first request generates");
        assert_eq!(first.status(), StatusCode::OK);
        let etag = header_value(&first, "etag").expect("derivative ETag");
        let derived_len = response_body(first).await.len() as u64;
        assert!(
            derived_len.div_ceil(1024) < source_kib,
            "the fixture must shrink, or this test proves nothing"
        );

        // Leave exactly one KiB less budget than the source needs. Anything
        // that reads the original now is refused; the derivative still fits.
        let held = state
            .artwork_fetch
            .local_permit((LOCAL_READ_BUDGET_KIB as u64 - (source_kib - 1)) * 1024)
            .await
            .expect("squeeze the byte budget");
        assert!(
            state
                .artwork_fetch
                .local_permit(source_kib * 1024)
                .await
                .is_none(),
            "the remaining budget must be too small for the original"
        );

        let warm = serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
            .await
            .expect("a warm derivative must not need the original's byte budget");
        assert_eq!(warm.status(), StatusCode::OK);
        assert_eq!(response_body(warm).await.len() as u64, derived_len);

        // Plan objective 3 on the route M2 exists for: a validator match costs
        // no read and no permit.
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag.parse().expect("validator"));
        let not_modified = serve_derivative(&state, source_name, &headers, ArtworkSize::W300)
            .await
            .expect("conditional derivative request");
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            header_value(&not_modified, "etag").as_deref(),
            Some(etag.as_str())
        );
        drop(held);
    }

    #[tokio::test]
    async fn peer_route_refuses_size() {
        let state = derivative_state();
        let source_name = "84-poster.jpg";
        let original = b"\xff\xd8\xff peer original".to_vec();
        tokio::fs::write(state.artwork_dir.join(source_name), &original)
            .await
            .expect("source");

        let refused =
            serve_verified_peer_artwork(&state, source_name, Some("w300"), &HeaderMap::new())
                .await
                .expect_err("peers replicate originals and derive locally");
        assert!(
            matches!(refused, ApiError::BadRequest(_)),
            "a bucket on the cluster route is a 400, not a silent original"
        );

        let served = serve_verified_peer_artwork(&state, source_name, None, &HeaderMap::new())
            .await
            .expect("the peer route still serves originals");
        assert_eq!(served.status(), StatusCode::OK);
        assert_eq!(response_body(served).await, original);
        assert!(
            !state.artwork_dir.join(DERIVED_DIR).exists(),
            "the peer route must not create a derivative store"
        );
    }

    #[tokio::test]
    async fn an_animated_gif_yields_one_png_frame() {
        let state = derivative_state();
        let source_name = "84-poster.gif";
        let source_path = state.artwork_dir.join(source_name);
        write_test_image(
            &source_path,
            "testsrc2=size=640x360:rate=10",
            &["-frames:v", "20"],
        );
        let source_identity = open_local_artwork(source_path.clone())
            .await
            .expect("source identity")
            .identity;
        let digest: [u8; 32] =
            Sha256::digest(tokio::fs::read(&source_path).await.expect("gif")).into();
        let derived_name =
            derivative_filename(digest, ArtworkSize::W300, source_name).expect("derived name");
        assert!(
            derived_name.ends_with(".png"),
            "an animated source must not stay a GIF: {derived_name}"
        );
        let derived_dir = ensure_derived_dir(&state.artwork_dir)
            .await
            .expect("derived directory");
        generate_derivative(
            &state.artwork_fetch,
            DerivativeJob {
                runtime_cache: &state.runtime_cache_dir,
                source_path: &source_path,
                source_name,
                source_identity,
                derived_dir: &derived_dir,
                derived_name: &derived_name,
                size: ArtworkSize::W300,
            },
        )
        .await
        .expect("generate from an animated GIF");
        let derived = tokio::fs::read(derived_dir.join(&derived_name))
            .await
            .expect("derived bytes");
        assert_eq!(&derived[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(image_dimensions(&derived_name, &derived), Some((300, 168)));
    }

    #[tokio::test]
    async fn an_alpha_png_stays_png() {
        let state = derivative_state();
        let source_name = "84-poster.png";
        let source_path = state.artwork_dir.join(source_name);
        write_test_image(
            &source_path,
            "color=c=red@0.5:s=640x360",
            &["-frames:v", "1", "-pix_fmt", "rgba"],
        );
        let source = tokio::fs::read(&source_path).await.expect("source");
        // IHDR colour type 6 is truecolour with alpha; the fixture must have
        // one or the assertion below proves nothing.
        assert_eq!(source[25], 6, "the fixture source must carry alpha");
        let source_identity = open_local_artwork(source_path.clone())
            .await
            .expect("source identity")
            .identity;
        let digest: [u8; 32] = Sha256::digest(&source).into();
        let derived_name =
            derivative_filename(digest, ArtworkSize::W500, source_name).expect("derived name");
        assert!(derived_name.ends_with(".png"));
        let derived_dir = ensure_derived_dir(&state.artwork_dir)
            .await
            .expect("derived directory");
        generate_derivative(
            &state.artwork_fetch,
            DerivativeJob {
                runtime_cache: &state.runtime_cache_dir,
                source_path: &source_path,
                source_name,
                source_identity,
                derived_dir: &derived_dir,
                derived_name: &derived_name,
                size: ArtworkSize::W500,
            },
        )
        .await
        .expect("generate from an alpha PNG");
        let derived = tokio::fs::read(derived_dir.join(&derived_name))
            .await
            .expect("derived bytes");
        assert_eq!(&derived[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            derived[25], 6,
            "an alpha source must keep its alpha channel, or the grid gains black corners"
        );
        assert_eq!(image_dimensions(&derived_name, &derived), Some((500, 282)));
    }

    #[tokio::test]
    async fn a_source_replaced_during_generation_is_not_committed() {
        let state = derivative_state();
        let source_name = "84-poster.png";
        let source_path = state.artwork_dir.join(source_name);
        write_test_image(&source_path, "color=c=red:s=640x360", &["-frames:v", "1"]);
        let stale_identity = open_local_artwork(source_path.clone())
            .await
            .expect("identity")
            .identity;
        let derived_dir = ensure_derived_dir(&state.artwork_dir)
            .await
            .expect("derived directory");
        let derived_name =
            derivative_filename([7_u8; 32], ArtworkSize::W300, source_name).expect("derived name");
        // Replace the source with a *decodable* image through the same
        // temp+rename install path a repair uses. Undecodable bytes would stop
        // the child on their own and prove nothing about the fence; these let
        // ffmpeg succeed, so only the identity check can refuse the rename.
        let replacement = state.artwork_dir.join("replacement.png");
        write_test_image(
            &replacement,
            "color=c=yellow:s=800x450",
            &["-frames:v", "1"],
        );
        let replacement_bytes = tokio::fs::read(&replacement).await.expect("replacement");
        plurx_core::metadata::write_artwork_atomically(&source_path, &replacement_bytes)
            .await
            .expect("replace source");
        generate_derivative(
            &state.artwork_fetch,
            DerivativeJob {
                runtime_cache: &state.runtime_cache_dir,
                source_path: &source_path,
                source_name,
                source_identity: stale_identity,
                derived_dir: &derived_dir,
                derived_name: &derived_name,
                size: ArtworkSize::W300,
            },
        )
        .await
        .expect_err("a replaced source must not commit a derivative under the old key");
        assert!(!derived_dir.join(&derived_name).exists());
    }

    #[tokio::test]
    async fn an_exhausted_byte_budget_does_not_look_like_a_changed_source() {
        let state = derivative_state();
        let source_name = "84-poster.png";
        let source_path = state.artwork_dir.join(source_name);
        write_test_image(&source_path, "color=c=blue:s=640x360", &["-frames:v", "1"]);
        let source_identity = open_local_artwork(source_path.clone())
            .await
            .expect("identity")
            .identity;
        let digest: [u8; 32] =
            Sha256::digest(tokio::fs::read(&source_path).await.expect("src")).into();
        let derived_dir = ensure_derived_dir(&state.artwork_dir)
            .await
            .expect("derived directory");
        let derived_name =
            derivative_filename(digest, ArtworkSize::W300, source_name).expect("derived name");

        // Every local byte permit is spoken for, exactly as sixteen concurrent
        // backdrop requests would leave it. The post-generation fence must not
        // read the source of truth a second time here: the caller already holds
        // the admitted read that produced this key, and a refusal from a second
        // read is not evidence that anything changed.
        let held = state
            .artwork_fetch
            .local_permit(LOCAL_READ_BUDGET_KIB as u64 * 1024)
            .await
            .expect("hold the whole byte budget");
        generate_derivative(
            &state.artwork_fetch,
            DerivativeJob {
                runtime_cache: &state.runtime_cache_dir,
                source_path: &source_path,
                source_name,
                source_identity,
                derived_dir: &derived_dir,
                derived_name: &derived_name,
                size: ArtworkSize::W300,
            },
        )
        .await
        .expect("a saturated byte budget must not discard a correct derivative");
        assert!(
            derived_dir.join(&derived_name).is_file(),
            "the generation must commit and make forward progress"
        );
        drop(held);
    }

    #[test]
    fn the_derivative_child_caches_in_the_runtime_cache_not_the_artwork_root() {
        // `XDG_CACHE_HOME` decides where libraries loaded by ffmpeg write their
        // bookkeeping. Pointed at the served artwork root it puts
        // library-created files inside the directory `sweep_content_orphans`
        // enumerates every pass; `state.rs` keeps `runtime_cache` separate
        // precisely so scratch cleanup cannot reach another managed root.
        let artwork = FsPath::new("/var/lib/plurx/artwork");
        let runtime_cache = FsPath::new("/var/lib/plurx/runtime");
        let command = derivative_command(
            "ffmpeg",
            &artwork.join("84-poster.jpg"),
            runtime_cache,
            ArtworkSize::W300,
            "jpg",
            "mjpeg",
        );
        let cache_home = command
            .as_std()
            .get_envs()
            .find(|(name, _)| *name == std::ffi::OsStr::new("XDG_CACHE_HOME"))
            .and_then(|(_, value)| value)
            .map(std::path::PathBuf::from)
            .expect("the child is given a cache home");
        assert_eq!(cache_home, runtime_cache);
        assert_ne!(cache_home, artwork);
    }

    #[tokio::test]
    async fn the_derivative_child_leaves_nothing_in_the_served_artwork_root() {
        // `XDG_CACHE_HOME` must be the dedicated runtime cache, never the
        // directory `sweep_content_orphans` enumerates every pass.
        let state = derivative_state();
        let source_name = "84-poster.png";
        let source_path = state.artwork_dir.join(source_name);
        write_test_image(&source_path, "color=c=green:s=640x360", &["-frames:v", "1"]);
        let response = serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
            .await
            .expect("derivative");
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        assert!(worker::run_one(&state).await.expect("worker pass"));
        let entries = std::fs::read_dir(&state.artwork_dir)
            .expect("artwork entries")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            entries,
            BTreeSet::from([source_name.to_owned(), DERIVED_DIR.to_owned()]),
            "nothing but the source and the derivative store belongs in the artwork root"
        );
    }

    pub(super) async fn age_past_the_orphan_grace(path: &FsPath) {
        let old = std::time::SystemTime::now()
            .checked_sub(CONTENT_ORPHAN_GRACE + Duration::from_secs(60))
            .expect("aged timestamp");
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open candidate")
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .expect("age candidate");
    }

    #[tokio::test]
    async fn orphaned_derivatives_are_swept_after_grace() {
        let state = derivative_state();
        let library = state
            .store
            .create_library(&plurx_core::domain::NewLibrary {
                name: "Films".into(),
                kind: plurx_core::domain::LibraryKind::Movies,
                paths: vec!["/films".into()],
                anime: false,
            })
            .await
            .expect("library");
        let mut item_ids = Vec::new();
        for title in ["Live backdrop", "Unverified legacy", "Verified legacy"] {
            item_ids.push(
                state
                    .store
                    .insert_item(&plurx_core::domain::NewItem {
                        library_id: library.id,
                        kind: plurx_core::domain::ItemKind::Movie,
                        parent_id: None,
                        title: title.into(),
                        year: Some(2024),
                        season_number: None,
                        episode_number: None,
                    })
                    .await
                    .expect("item"),
            );
        }

        // A content-addressed backdrop: the family that publishes only sixteen
        // hex characters, and the one every TV hero uses.
        let backdrop_bytes = b"live backdrop bytes";
        let backdrop_digest: [u8; 32] = Sha256::digest(backdrop_bytes).into();
        let backdrop = format!(
            "{}-backdrop-c{}.jpg",
            item_ids[0],
            &hex::encode(backdrop_digest)[..16]
        );
        // A legacy source nothing has verified since boot.
        let unverified = format!("{}-poster.jpg", item_ids[1]);
        // A legacy source whose bytes have been verified, and whose derivative
        // was made from a *previous* generation of those bytes.
        let verified = format!("{}-poster.jpg", item_ids[2]);
        let verified_bytes = b"current legacy poster bytes";
        let verified_digest: [u8; 32] = Sha256::digest(verified_bytes).into();

        for (item_id, poster, backdrop_path) in [
            (item_ids[0], None, Some(backdrop.clone())),
            (item_ids[1], Some(unverified.clone()), None),
            (item_ids[2], Some(verified.clone()), None),
        ] {
            state
                .store
                .apply_metadata(
                    item_id,
                    &plurx_core::domain::MetadataPatch {
                        poster_path: poster,
                        backdrop_path,
                        ..Default::default()
                    },
                )
                .await
                .expect("patch");
        }

        tokio::fs::write(state.artwork_dir.join(&verified), verified_bytes)
            .await
            .expect("verified source");
        let identity = open_local_artwork(state.artwork_dir.join(&verified))
            .await
            .expect("identity")
            .identity;
        state
            .artwork_fetch
            .remember(&verified, identity, verified_digest)
            .await;

        let derived_dir = ensure_derived_dir(&state.artwork_dir)
            .await
            .expect("derived directory");
        let live_backdrop = derivative_filename(backdrop_digest, ArtworkSize::W780, &backdrop)
            .expect("backdrop derivative");
        let live_unverified = derivative_filename([1_u8; 32], ArtworkSize::W300, &unverified)
            .expect("unverified derivative");
        let live_verified = derivative_filename(verified_digest, ArtworkSize::W500, &verified)
            .expect("verified derivative");
        let stale_verified = derivative_filename([2_u8; 32], ArtworkSize::W500, &verified)
            .expect("stale derivative");
        let unreferenced = derivative_filename([3_u8; 32], ArtworkSize::W300, "999-poster.jpg")
            .expect("unreferenced derivative");
        let stranded_temporary = format!(".{live_backdrop}.{}.tmp", uuid::Uuid::new_v4().simple());
        let superseded = format!("{}-w300.jpg", "4".repeat(32));

        let kept = [&live_backdrop, &live_unverified, &live_verified];
        let removed = [
            &stale_verified,
            &unreferenced,
            &stranded_temporary,
            &superseded,
        ];
        for name in kept.iter().chain(removed.iter()) {
            let path = derived_dir.join(name.as_str());
            tokio::fs::write(&path, b"derived bytes")
                .await
                .expect("derivative");
            age_past_the_orphan_grace(&path).await;
        }

        assert_eq!(
            sweep_derived_orphans(&state).await,
            removed.len(),
            "exactly the dead derivatives are reclaimed"
        );
        for name in kept {
            assert!(
                derived_dir.join(name.as_str()).is_file(),
                "a live derivative was swept: {name}"
            );
        }
        for name in removed {
            assert!(
                !derived_dir.join(name.as_str()).exists(),
                "a dead entry survived: {name}"
            );
        }

        // Nothing is removed before the grace, and a fresh derivative of a
        // source with no verified digest is not an orphan either.
        let fresh = derivative_filename([5_u8; 32], ArtworkSize::W300, &unverified)
            .expect("fresh derivative");
        tokio::fs::write(derived_dir.join(&fresh), b"fresh")
            .await
            .expect("fresh derivative");
        assert_eq!(sweep_derived_orphans(&state).await, 0);
        assert!(derived_dir.join(&fresh).is_file());
    }
}

#[cfg(test)]
mod reconciliation_scope_tests {
    use super::*;

    fn view(
        active_voter: bool,
        committed_voter: bool,
        committed_member: bool,
    ) -> ArtworkMembershipView {
        ArtworkMembershipView {
            active_voter,
            committed_voter,
            committed_member,
        }
    }

    /// A learner reconciles its own artwork forever, and is never mistaken for
    /// a removed node.
    ///
    /// This is the regression: the tick used to read "not a voter" as "removed
    /// voter" and `break` out of the loop on the learner's first pass, roughly
    /// two seconds after boot. The learner then never materialized an image
    /// again — not after a promotion either, because the loop was gone — and
    /// the log said it had been removed, which was false.
    #[test]
    fn a_member_without_a_vote_reconciles_locally_instead_of_stopping() {
        assert_eq!(
            artwork_tick(view(false, false, true)),
            ArtworkTick::Reconcile(ArtworkReconciliation::LocalOnly),
            "a learner is a member; it converges its own artwork directory"
        );
    }

    /// Promotion needs no restart: the same view a moment later is a full pass.
    #[test]
    fn a_promoted_learner_starts_provider_work_on_its_next_tick() {
        assert_eq!(
            artwork_tick(view(true, true, true)),
            ArtworkTick::Reconcile(ArtworkReconciliation::Full)
        );
    }

    /// Only genuine absence from committed membership ends the loop.
    #[test]
    fn stopping_is_reserved_for_a_node_that_is_no_longer_a_member() {
        assert_eq!(artwork_tick(view(false, false, false)), ArtworkTick::Stop);
    }

    /// A committed voter whose durable row is fenced by an in-flight removal
    /// waits, rather than doing provider work in the transition window or
    /// deciding it has been removed.
    #[test]
    fn a_fenced_voter_waits_for_the_membership_change_to_settle() {
        assert_eq!(artwork_tick(view(false, true, true)), ArtworkTick::Wait);
    }
}

/// Admission state is bounded and never evicts active addresses to admit new ones.
#[derive(Debug, Default)]
struct JellyfinArtworkBudget {
    addresses: HashMap<std::net::IpAddr, JellyfinArtworkWindow>,
}
#[derive(Debug)]
struct JellyfinArtworkWindow {
    started: Instant,
    last: Instant,
    count: u8,
}
/// One budget per IPv4 address and per IPv6 /64: a single host owns a whole
/// /64, so keying full IPv6 addresses would let one host fill the table.
fn artwork_budget_key(address: std::net::IpAddr) -> std::net::IpAddr {
    match address {
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => std::net::IpAddr::V4(v4),
            None => std::net::IpAddr::V6(std::net::Ipv6Addr::from(
                u128::from(v6) & !((1u128 << 64) - 1),
            )),
        },
        v4 => v4,
    }
}
impl JellyfinArtworkBudget {
    fn admit(&mut self, address: std::net::IpAddr, now: Instant) -> bool {
        let address = artwork_budget_key(address);
        self.addresses
            .retain(|_, window| now.duration_since(window.last) < Duration::from_secs(60));
        if !self.addresses.contains_key(&address) && self.addresses.len() >= 4096 {
            return false;
        }
        let window = self
            .addresses
            .entry(address)
            .or_insert(JellyfinArtworkWindow {
                started: now,
                last: now,
                count: 0,
            });
        if now.duration_since(window.started) >= Duration::from_secs(60) {
            window.started = now;
            window.count = 0;
        }
        window.last = now;
        if window.count >= 20 {
            return false;
        }
        window.count += 1;
        true
    }
}
#[derive(Debug, Clone)]
struct JellyfinArtworkDemand {
    mapped: plurx_core::store::JellyfinCatalogArtwork,
    backdrop: bool,
    size: ArtworkSize,
    deadline: Instant,
    retry_at: Instant,
}
pub(super) async fn serve_jellyfin_artwork(
    state: &AppState,
    mapped: plurx_core::store::JellyfinCatalogArtwork,
    backdrop: bool,
    width: u32,
    headers: &HeaderMap,
    address: std::net::IpAddr,
) -> Result<Response, ApiError> {
    let size = match width {
        0..=300 => ArtworkSize::W300,
        301..=500 => ArtworkSize::W500,
        _ => ArtworkSize::W780,
    };
    let safe_name = safe_artwork_name(&mapped.filename)?;
    // Open/fstat plus an existing verified digest; no original read, peer
    // fetch, decoder or resize is allowed in this compatibility request.
    if let Some(opened) = open_local_artwork(state.artwork_dir.join(safe_name)).await {
        if let Some(digest) = state
            .artwork_fetch
            .verified_digest(safe_name, opened.identity)
            .await
        {
            if let Some(spec) = worker::spec(digest, size, safe_name).await {
                if let Some(mut response) =
                    worker::serve_local_variant(state, &spec, headers).await?
                {
                    // The URL identifies the current mapped item's artwork,
                    // which can change. Always revalidate instead of freezing it.
                    response.headers_mut().insert(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("private, max-age=0, must-revalidate"),
                    );
                    return Ok(response);
                }
            }
        }
    }
    // Warm hits do no work; only a miss, which enqueues materialization,
    // spends this address's budget.
    admit_jellyfin_artwork(state, address).await?;
    let key = format!(
        "{}:{}:{}:{}:{}",
        mapped.wire_id,
        backdrop,
        size.label(),
        mapped.filename,
        mapped.generation
    );
    let now = Instant::now();
    let mut pending = state.artwork_fetch.jellyfin_demands.lock().await;
    pending.retain(|_, d| d.deadline > now);
    if !pending.contains_key(&key) {
        if pending.len() >= 256 {
            return Err(artwork_capacity_error());
        }
        pending.insert(
            key,
            JellyfinArtworkDemand {
                mapped,
                backdrop,
                size,
                deadline: now + Duration::from_secs(60),
                retry_at: now,
            },
        );
        state.artwork_fetch.wake.notify_one();
    }
    drop(pending);
    // A miss is retryable and never an inline original/thumbnail fallback.
    let mut response = ApiError::NotFound("image derivative").into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
    Ok(response)
}
async fn admit_jellyfin_artwork(
    state: &AppState,
    address: std::net::IpAddr,
) -> Result<(), ApiError> {
    if !state
        .artwork_fetch
        .jellyfin_budget
        .lock()
        .await
        .admit(address, Instant::now())
    {
        return Err(ApiError::typed(
            StatusCode::TOO_MANY_REQUESTS,
            "artwork_admission_limit",
            "artwork admission limit reached",
        ));
    }
    Ok(())
}
/// Runs only in the existing artwork owner; a miss never creates another task.
async fn jellyfin_artwork_pass(state: &AppState) -> bool {
    use plurx_core::store::background_jobs::JobKind;
    if !state
        .jobs
        .execution_authority()
        .may_execute_job(JobKind::ArtworkDerivative)
        .await
    {
        return false;
    }
    let now = Instant::now();
    let intents = {
        let mut pending = state.artwork_fetch.jellyfin_demands.lock().await;
        pending.retain(|_, d| d.deadline > now);
        pending
            .iter_mut()
            .filter(|(_, d)| d.retry_at <= now)
            .take(4)
            .map(|(key, d)| {
                d.retry_at = now + Duration::from_secs(5);
                (key.clone(), d.clone())
            })
            .collect::<Vec<_>>()
    };
    let progressed = !intents.is_empty();
    for (key, intent) in intents {
        match state
            .store
            .jellyfin_catalog_artwork(intent.mapped.wire_id.clone(), intent.backdrop)
            .await
        {
            Ok(Some(current)) if current == intent.mapped => {}
            Ok(_) => {
                state
                    .artwork_fetch
                    .jellyfin_demands
                    .lock()
                    .await
                    .remove(&key);
                continue;
            }
            Err(_) => continue,
        }
        let name = &intent.mapped.filename;
        let Ok(safe_name) = safe_artwork_name(name) else {
            state
                .artwork_fetch
                .jellyfin_demands
                .lock()
                .await
                .remove(&key);
            continue;
        };
        let source = match read_verified_local_artwork(
            &state.artwork_fetch,
            state.artwork_dir.join(safe_name),
            safe_name,
        )
        .await
        {
            LocalArtworkRead::Verified(bytes) => Some(bytes),
            LocalArtworkRead::Missing => {
                let membership = state.membership.clone();
                fetch_and_materialize(
                    &state.artwork_fetch,
                    &state.artwork_dir,
                    safe_name,
                    move |client| async move {
                        let peers = membership.reachable_peer_http_urls().await.ok()?;
                        fetch_peer_artwork(&client, &membership, &peers, safe_name).await
                    },
                )
                .await
                .ok()
                .flatten()
            }
            LocalArtworkRead::Corrupt(identity) => {
                quarantine_corrupt_artwork(
                    &state.artwork_fetch,
                    &state.artwork_dir,
                    safe_name,
                    identity,
                )
                .await;
                None
            }
            LocalArtworkRead::Capacity => None,
        };
        let Some(source) = source else {
            continue;
        };
        // Source verification/fetch awaited. Fence the exact incarnation,
        // artwork name and switch generation again before durable admission.
        if !matches!(state.store.jellyfin_catalog_artwork(intent.mapped.wire_id.clone(),intent.backdrop).await,Ok(Some(current)) if current==intent.mapped)
        {
            state
                .artwork_fetch
                .jellyfin_demands
                .lock()
                .await
                .remove(&key);
            continue;
        }
        if let Some(spec) = worker::spec(source.digest, intent.size, safe_name).await {
            worker::request(state, spec).await;
        }
        state
            .artwork_fetch
            .jellyfin_demands
            .lock()
            .await
            .remove(&key);
    }
    progressed
}

#[cfg(test)]
mod jellyfin_artwork_tests {
    use super::*;
    fn artwork_test_address(n: u32) -> std::net::IpAddr {
        std::net::IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + n))
    }
    #[test]
    fn jellyfin_artwork_budget_keys_ipv6_per_64_and_ipv4_per_address() {
        let mut budget = JellyfinArtworkBudget::default();
        let now = Instant::now();
        let host: std::net::IpAddr = "2001:db8:1:2::1".parse().expect("v6");
        let same_64: std::net::IpAddr = "2001:db8:1:2:ffff::9".parse().expect("v6");
        let other_64: std::net::IpAddr = "2001:db8:1:3::1".parse().expect("v6");
        for _ in 0..20 {
            assert!(budget.admit(host, now));
        }
        assert!(
            !budget.admit(same_64, now),
            "one /64 shares one miss budget"
        );
        assert!(budget.admit(other_64, now));
        for v4 in ["192.0.2.1", "192.0.2.2"] {
            assert!(budget.admit(v4.parse().expect("v4"), now));
        }
        let mapped: std::net::IpAddr = "::ffff:192.0.2.1".parse().expect("mapped");
        assert_eq!(
            artwork_budget_key(mapped),
            "192.0.2.1".parse::<std::net::IpAddr>().expect("v4")
        );
    }
    #[tokio::test]
    async fn jellyfin_artwork_cold_demands_deduplicate_without_hashing_or_decoding_originals() {
        let state = super::tests::derivative_state();
        let name = "1-poster-c123456789abcdef0.png";
        tokio::fs::create_dir_all(&state.artwork_dir)
            .await
            .expect("fixture directory");
        tokio::fs::write(
            state.artwork_dir.join(name),
            b"synthetic bytes that must not be decoded inline",
        )
        .await
        .expect("fixture source");
        let mapped = plurx_core::store::JellyfinCatalogArtwork {
            wire_id: uuid::Uuid::new_v4().simple().to_string(),
            filename: name.into(),
            generation: uuid::Uuid::new_v4().simple().to_string(),
        };
        for _ in 0..4 {
            let response = serve_jellyfin_artwork(
                &state,
                mapped.clone(),
                false,
                500,
                &HeaderMap::new(),
                artwork_test_address(0),
            )
            .await
            .expect("cold request");
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), 0);
        assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 0);
        assert_eq!(state.artwork_fetch.jellyfin_demands.lock().await.len(), 1);
        // Admission is refused at the fixed queue bound; existing work remains.
        for n in 1..256u32 {
            let mut another = mapped.clone();
            another.wire_id = uuid::Uuid::new_v4().simple().to_string();
            serve_jellyfin_artwork(
                &state,
                another,
                false,
                500,
                &HeaderMap::new(),
                artwork_test_address(n),
            )
            .await
            .expect("bounded demand");
        }
        let mut overflow = mapped;
        overflow.wire_id = uuid::Uuid::new_v4().simple().to_string();
        assert!(serve_jellyfin_artwork(
            &state,
            overflow,
            false,
            500,
            &HeaderMap::new(),
            artwork_test_address(0)
        )
        .await
        .is_err());
        assert_eq!(state.artwork_fetch.jellyfin_demands.lock().await.len(), 256);
        assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn jellyfin_artwork_owner_discards_queued_demand_after_switch_generation_changes() {
        use plurx_core::domain::{ItemKind, LibraryKind, MetadataPatch, NewItem, NewLibrary};
        use plurx_core::store::JellyfinEntityKind;
        let state = super::tests::derivative_state();
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "Mapped artwork".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/synthetic".into()],
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
                title: "Mapped".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        state
            .store
            .apply_metadata(
                item,
                &MetadataPatch {
                    poster_path: Some("1-poster-c123456789abcdef0.png".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("poster mapping");
        let wire = state
            .store
            .jellyfin_entity_ids(JellyfinEntityKind::Item, &[item])
            .await
            .expect("wire")[0]
            .wire_id
            .clone();
        state
            .store
            .set_jellyfin_compatibility(true)
            .await
            .expect("enable");
        let old = state
            .store
            .jellyfin_catalog_artwork(wire.clone(), false)
            .await
            .expect("mapping")
            .expect("enabled mapping");
        serve_jellyfin_artwork(
            &state,
            old.clone(),
            false,
            500,
            &HeaderMap::new(),
            artwork_test_address(0),
        )
        .await
        .expect("queued miss");
        assert_eq!(state.artwork_fetch.jellyfin_demands.lock().await.len(), 1);
        state
            .store
            .set_jellyfin_compatibility(false)
            .await
            .expect("disable");
        state
            .store
            .set_jellyfin_compatibility(true)
            .await
            .expect("reenable");
        let current = state
            .store
            .jellyfin_catalog_artwork(wire, false)
            .await
            .expect("mapping")
            .expect("new mapping");
        assert_ne!(old.generation, current.generation);
        assert!(jellyfin_artwork_pass(&state).await);
        assert!(state.artwork_fetch.jellyfin_demands.lock().await.is_empty());
        assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), 0);
        assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn jellyfin_artwork_native_owner_publishes_warm_private_revalidated_derivatives() {
        use plurx_core::domain::{ItemKind, LibraryKind, MetadataPatch, NewItem, NewLibrary};
        use plurx_core::store::JellyfinEntityKind;
        let state = super::tests::derivative_state();
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "Mapped artwork".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/synthetic".into()],
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
                title: "Mapped".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        state
            .store
            .apply_metadata(
                item,
                &MetadataPatch {
                    poster_path: Some("1-poster.png".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("poster mapping");
        let wire = state
            .store
            .jellyfin_entity_ids(JellyfinEntityKind::Item, &[item])
            .await
            .expect("wire")[0]
            .wire_id
            .clone();
        state
            .store
            .set_jellyfin_compatibility(true)
            .await
            .expect("enable");
        let old = state
            .store
            .jellyfin_catalog_artwork(wire.clone(), false)
            .await
            .expect("mapping")
            .expect("enabled mapping");
        super::tests::write_test_image(
            &state.artwork_dir.join(&old.filename),
            "testsrc2=size=960x540",
            &["-frames:v", "1"],
        );
        serve_jellyfin_artwork(
            &state,
            old.clone(),
            false,
            500,
            &HeaderMap::new(),
            artwork_test_address(0),
        )
        .await
        .expect("cold miss");
        assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), 0);
        assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 0);
        assert!(jellyfin_artwork_pass(&state).await);
        assert!(state.artwork_fetch.jellyfin_demands.lock().await.is_empty());
        assert!(worker::run_one(&state)
            .await
            .expect("native derivative owner"));
        let derivations = state.artwork_fetch.derivations.load(Ordering::SeqCst);
        let warm = serve_jellyfin_artwork(
            &state,
            old.clone(),
            false,
            500,
            &HeaderMap::new(),
            artwork_test_address(0),
        )
        .await
        .expect("warm derivative");
        assert_eq!(warm.status(), StatusCode::OK);
        assert_eq!(
            warm.headers()[header::CACHE_CONTROL],
            "private, max-age=0, must-revalidate"
        );
        let etag = warm.headers()[header::ETAG].clone();
        assert!(!axum::body::to_bytes(warm.into_body(), 8 * 1024 * 1024)
            .await
            .expect("warm bytes")
            .is_empty());
        // Native serving verifies the published derivative on its first read.
        // Subsequent validator hits must not hash either source or derivative.
        let hashes = state.artwork_fetch.hashes.load(Ordering::SeqCst);
        let mut conditional = HeaderMap::new();
        conditional.insert(header::IF_NONE_MATCH, etag);
        let unchanged = serve_jellyfin_artwork(
            &state,
            old,
            false,
            500,
            &conditional,
            artwork_test_address(0),
        )
        .await
        .expect("conditional derivative");
        assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            unchanged.headers()[header::CACHE_CONTROL],
            "private, max-age=0, must-revalidate"
        );
        assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), hashes);
        assert_eq!(
            state.artwork_fetch.derivations.load(Ordering::SeqCst),
            derivations
        );
        assert!(state.artwork_fetch.jellyfin_demands.lock().await.is_empty());
    }
    #[test]
    fn jellyfin_artwork_budget_bounds_addresses_without_evicting_active_windows() {
        let mut budget = JellyfinArtworkBudget::default();
        let now = Instant::now();
        let address = std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
        for _ in 0..20 {
            assert!(budget.admit(address, now));
        }
        assert!(!budget.admit(address, now));
        for n in 1..4096 {
            assert!(budget.admit(std::net::IpAddr::V4(std::net::Ipv4Addr::from(n)), now));
        }
        assert_eq!(budget.addresses.len(), 4096);
        assert!(!budget.admit(std::net::IpAddr::V4(std::net::Ipv4Addr::from(5000)), now));
        assert!(!budget.admit(address, now + Duration::from_secs(59)));
        assert!(budget.admit(address, now + Duration::from_secs(60)));
        assert!(budget.admit(
            std::net::IpAddr::V4(std::net::Ipv4Addr::from(5000)),
            now + Duration::from_secs(60)
        ));
        assert_eq!(budget.addresses.len(), 2);
    }
}
