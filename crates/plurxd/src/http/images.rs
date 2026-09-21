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
    #[cfg(test)]
    hashes: AtomicU64,
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
            #[cfg(test)]
            hashes: AtomicU64::new(0),
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
    if query.size.is_some() {
        return Err(ApiError::BadRequest(
            "cluster artwork does not serve derivatives".into(),
        ));
    }
    serve_local_artwork(
        &state.artwork_fetch,
        &state.artwork_dir,
        safe_name,
        &headers,
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
        record_request(ArtworkRoute::User, ArtworkOutcome::Hit);
        record_derivative(size, DerivativeOutcome::Served);
        return Ok(admitted_artwork_response(
            &source_path,
            source,
            &[("vary", "Accept")],
        ));
    }

    let Some(derived_name) = derivative_filename(source.digest, size, safe_name) else {
        record_derivative(size, DerivativeOutcome::Refused);
        return Err(ApiError::BadRequest("unsupported image extension".into()));
    };
    let derived_dir = ensure_derived_dir(&state.artwork_dir)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "cannot create artwork derivative directory");
            ApiError::Internal("cannot prepare artwork derivatives".into())
        })?;
    match serve_local_artwork(
        &state.artwork_fetch,
        &derived_dir,
        &derived_name,
        headers,
        ArtworkRoute::User,
    )
    .await
    {
        Ok(response) => {
            record_derivative(size, DerivativeOutcome::Served);
            return Ok(with_response_header(response, header::VARY, "Accept"));
        }
        Err(error) if is_artwork_capacity_error(&error) => {
            return Ok(derivative_fallback(&source_path, source, size));
        }
        Err(_) => {}
    }

    let flight_key = format!("derived/{derived_name}");
    let _flight = state.artwork_fetch.filename(&flight_key).await;
    match serve_local_artwork(
        &state.artwork_fetch,
        &derived_dir,
        &derived_name,
        headers,
        ArtworkRoute::User,
    )
    .await
    {
        Ok(response) => {
            record_derivative(size, DerivativeOutcome::Served);
            return Ok(with_response_header(response, header::VARY, "Accept"));
        }
        Err(error) if is_artwork_capacity_error(&error) => {
            return Ok(derivative_fallback(&source_path, source, size));
        }
        Err(_) => {}
    }

    let Some(_derive_permit) = state.artwork_fetch.derive_permit().await else {
        return Ok(derivative_fallback(&source_path, source, size));
    };

    if let Err(error) = generate_derivative(
        &state.artwork_fetch,
        DerivativeJob {
            artwork_dir: &state.artwork_dir,
            source_path: &source_path,
            source_name: safe_name,
            source_digest: source.digest,
            derived_dir: &derived_dir,
            derived_name: &derived_name,
            size,
        },
    )
    .await
    {
        tracing::warn!(filename = safe_name, bucket = size.label(), %error, "artwork derivative generation failed");
        return Ok(derivative_fallback(&source_path, source, size));
    }
    let response = serve_local_artwork(
        &state.artwork_fetch,
        &derived_dir,
        &derived_name,
        headers,
        ArtworkRoute::User,
    )
    .await;
    match response {
        Ok(response) => {
            drop(source);
            record_derivative(size, DerivativeOutcome::Generated);
            Ok(with_response_header(response, header::VARY, "Accept"))
        }
        Err(_) => Ok(derivative_fallback(&source_path, source, size)),
    }
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
        "{}-{}.{}",
        &hex::encode(digest)[..32],
        size.label(),
        extension
    ))
}

async fn ensure_derived_dir(artwork_dir: &FsPath) -> Result<std::path::PathBuf, std::io::Error> {
    let path = artwork_dir.join("derived");
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
    artwork_dir: &'a FsPath,
    source_path: &'a FsPath,
    source_name: &'a str,
    source_digest: [u8; 32],
    derived_dir: &'a FsPath,
    derived_name: &'a str,
    size: ArtworkSize,
}

async fn generate_derivative(
    coordinator: &ArtworkCoordinator,
    job: DerivativeJob<'_>,
) -> Result<(), String> {
    generate_derivative_with_bin(coordinator, job, &crate::ffmpeg::ffmpeg_bin()).await
}

async fn generate_derivative_with_bin(
    coordinator: &ArtworkCoordinator,
    job: DerivativeJob<'_>,
    ffmpeg_bin: &str,
) -> Result<(), String> {
    let DerivativeJob {
        artwork_dir,
        source_path,
        source_name,
        source_digest,
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
    crate::transcode::configure_ffmpeg_runtime(&mut command, artwork_dir);
    let child = crate::ffmpeg::BoundedDiagnosticChild::spawn_piped_output(&mut command)
        .map_err(|error| error.to_string())?;
    let generated = tokio::time::timeout(
        DERIVATIVE_TIMEOUT,
        child.output_to_bounded_file(&temporary_path, MAX_ARTWORK_BYTES),
    )
    .await;
    let result = match generated {
        Ok(Ok((status, _))) if status.success() => Ok(()),
        Ok(Ok((status, diagnostics))) => Err(format!("ffmpeg exited {status}: {diagnostics}")),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!(
            "ffmpeg timed out after {} seconds",
            DERIVATIVE_TIMEOUT.as_secs()
        )),
    };
    if let Err(error) = result {
        remove_derivative_temporary(&temporary_path).await;
        return Err(error);
    }

    let source_still_matches = match read_verified_local_artwork(
        coordinator,
        source_path.to_path_buf(),
        source_name,
    )
    .await
    {
        LocalArtworkRead::Verified(bytes) => bytes.digest == source_digest,
        _ => false,
    };
    if !source_still_matches {
        remove_derivative_temporary(&temporary_path).await;
        return Err("source changed during derivative generation".to_owned());
    }
    if let Err(error) = tokio::fs::rename(&temporary_path, derived_dir.join(derived_name)).await {
        remove_derivative_temporary(&temporary_path).await;
        return Err(error.to_string());
    }
    coordinator.forget(derived_name).await;
    Ok(())
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
            Ok(admitted_artwork_response(&path, bytes, &[]))
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
            && !artwork_bytes_match_name(filename, bytes)
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
    extra_headers: &[(&'static str, &'static str)],
) -> Response {
    let digest = bytes.digest;
    artwork_response_bytes(path, bytes::Bytes::from_owner(bytes), digest, extra_headers)
}

#[cfg(test)]
fn artwork_response(path: &FsPath, bytes: Vec<u8>) -> Response {
    let digest = Sha256::digest(&bytes).into();
    artwork_response_bytes(path, bytes.into(), digest, &[])
}

fn artwork_response_bytes(
    path: &FsPath,
    bytes: bytes::Bytes,
    digest: [u8; 32],
    extra_headers: &[(&'static str, &'static str)],
) -> Response {
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let mut response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (
                header::CACHE_CONTROL,
                "private, max-age=604800, immutable".to_owned(),
            ),
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
        HeaderValue::from_static("private, max-age=604800, immutable"),
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
            let mut failed = false;
            while let Some(chunk) = body.next().await {
                match chunk {
                    Ok(chunk) if bytes.len() as u64 + chunk.len() as u64 <= MAX_ARTWORK_BYTES => {
                        bytes.extend_from_slice(&chunk);
                    }
                    _ => {
                        failed = true;
                        break;
                    }
                }
            }
            if !failed && !bytes.is_empty() && artwork_bytes_match_name(&filename, &bytes) {
                return Some(bytes);
            }
            None
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

fn derivative_source_prefix(filename: &str) -> Option<&str> {
    let (stem, extension) = filename.rsplit_once('.')?;
    if !matches!(extension, "jpg" | "jpeg" | "png" | "webp") {
        return None;
    }
    let (prefix, bucket) = stem.rsplit_once('-')?;
    (prefix.len() == 32
        && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(bucket, "w300" | "w500" | "w780"))
    .then_some(prefix)
}

async fn sweep_derived_orphans(state: &AppState) -> usize {
    let Ok(items) = state.store.items_with_artwork().await else {
        return 0;
    };
    let cached = state.artwork_fetch.verified_snapshot().await;
    let mut live = BTreeSet::new();
    let mut has_unverified_legacy = false;
    for reference in artwork_references(items) {
        match content_addressed_artwork_digest(&reference.filename) {
            Some(ArtworkContentDigest::Prefix16(prefix)) => {
                live.insert(prefix.to_ascii_lowercase());
            }
            Some(ArtworkContentDigest::Full64(digest)) => {
                live.insert(digest[..32].to_ascii_lowercase());
            }
            None => match cached.get(&reference.filename) {
                Some(digest) => {
                    live.insert(hex::encode(digest)[..32].to_owned());
                }
                None => has_unverified_legacy = true,
            },
        }
    }
    // A legacy filename carries no source digest. Until every referenced
    // legacy source has entered the verified cache there is no safe way to
    // distinguish its derivative from an orphan, so retain the uncertain
    // files. Re-deriving is cheap; deleting a live grid cache blindly is not.
    if has_unverified_legacy {
        return 0;
    }
    let derived_dir = state.artwork_dir.join("derived");
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
        let Some(prefix) = derivative_source_prefix(&filename) else {
            continue;
        };
        if live.contains(&prefix.to_ascii_lowercase()) {
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
        if tokio::fs::remove_file(entry.path()).await.is_ok() {
            state.artwork_fetch.forget(&filename).await;
            removed += 1;
        }
    }
    removed
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
        let first: [u8; 32] = Sha256::digest(b"first legacy bytes").into();
        let second: [u8; 32] = Sha256::digest(b"second legacy bytes").into();
        let first_key =
            derivative_filename(first, ArtworkSize::W500, "84-poster.jpg").expect("first key");
        let second_key =
            derivative_filename(second, ArtworkSize::W500, "84-poster.jpg").expect("second key");
        assert_ne!(first_key, second_key);
        assert!(first_key.ends_with("-w500.jpg"));
        assert_eq!(
            derivative_source_prefix(&first_key),
            Some(&hex::encode(first)[..32])
        );
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
        generate_derivative_with_bin(
            &coordinator,
            DerivativeJob {
                artwork_dir: directory.path(),
                source_path: &source_path,
                source_name,
                source_digest: digest,
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
        generate_derivative(
            &coordinator,
            DerivativeJob {
                artwork_dir: directory.path(),
                source_path: &source_path,
                source_name,
                source_digest: digest,
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
