//! Cluster-only media snapshots/offers and admin diagnostics.
//!
//! Internal routes accept only exact, short-lived voter signatures. The
//! `/api/v1` routes are separate admin views; neither surface starts work.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::Stream;
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use super::error::ApiError;
use super::extract::AdminUser;
use super::peer_transport::exact_auth_from_headers;
use crate::media_pool::{
    local_offer, local_snapshot, MediaDirectoryDiagnostics, MediaOffer, MediaOfferRequest,
    PlacementDiagnostics, OFFERS_PATH, SNAPSHOT_PATH,
};
use crate::media_sessions::MEDIA_BODY_READ_BUFFER;
use crate::state::AppState;

/// Authenticated peers are still fallible. Bound verified descriptor streams
/// to four concurrent responses so a buggy voter cannot accumulate open files
/// or queued response bodies.
static FRAGMENT_INDEX_READS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)));
static FRAGMENT_INDEX_PEER_READS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Semaphore>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Subtitle-source responses have their own bounded read budget. A permit is
/// held until the peer drains or drops the response, including after the
/// handler has returned.
static SUBTITLE_SOURCE_READS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)));
static SUBTITLE_SOURCE_PEER_READS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Semaphore>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// The permits live in the response stream, not the request handler. A slow
/// or non-reading peer therefore occupies both budgets until EOF or body drop.
struct FragmentIndexStream<S> {
    inner: S,
    _global_permit: tokio::sync::OwnedSemaphorePermit,
    _peer_permit: tokio::sync::OwnedSemaphorePermit,
    subtitle_bytes: bool,
}

impl<S, E> Stream for FragmentIndexStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, E>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(context) {
            Poll::Ready(Some(Ok(bytes))) => {
                if self.subtitle_bytes {
                    crate::telemetry::record_subtitle_source(
                        crate::telemetry::SubtitleSourceMetric::HydrationServedBytes(
                            bytes.len() as u64
                        ),
                    );
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            other => other,
        }
    }
}

pub(crate) async fn snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<
    (
        [(HeaderName, &'static str); 1],
        Json<crate::media_pool::MediaNodeSnapshot>,
    ),
    StatusCode,
> {
    authorize(&state, &headers, "GET", SNAPSHOT_PATH, &[]).await?;
    Ok((
        private_no_store_headers(),
        Json(local_snapshot(&state).await),
    ))
}

fn private_no_store_headers() -> [(HeaderName, &'static str); 1] {
    [(header::CACHE_CONTROL, "private, no-store")]
}

pub(crate) async fn fragment_index(
    State(state): State<AppState>,
    AxumPath(cache_key): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let path = format!(
        "{}{}",
        crate::fragment_index_cluster::PEER_PATH_PREFIX,
        cache_key
    );
    let peer_id = authorize(&state, &headers, "GET", &path, &[]).await?;
    let read_permit = std::sync::Arc::clone(&FRAGMENT_INDEX_READS)
        .try_acquire_owned()
        .map_err(|_| {
            tracing::warn!(cache_key, "throttling concurrent peer fragment-index reads");
            StatusCode::TOO_MANY_REQUESTS
        })?;
    let peer_reads = FRAGMENT_INDEX_PEER_READS
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .entry(peer_id.clone())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone();
    let peer_permit = peer_reads.try_acquire_owned().map_err(|_| {
        tracing::warn!(
            cache_key,
            peer_id,
            "throttling one peer's fragment-index reads"
        );
        StatusCode::TOO_MANY_REQUESTS
    })?;
    let artifact = state
        .store
        .cluster_fragment_index_artifact(&cache_key)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let root = crate::fragment_index_cluster::cache_root(&state.runtime_cache_dir);
    match crate::fragment_index_cluster::open_verified_local_blob(&root, &artifact).await {
        Ok(Some(file)) => {
            let stream = FragmentIndexStream {
                inner: tokio_util::io::ReaderStream::with_capacity(file, MEDIA_BODY_READ_BUFFER),
                _global_permit: read_permit,
                _peer_permit: peer_permit,
                subtitle_bytes: false,
            };
            let mut response = Body::from_stream(stream).into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                "private, no-store".parse().expect("static cache control"),
            );
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                "application/octet-stream"
                    .parse()
                    .expect("static content type"),
            );
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                artifact
                    .bytes
                    .to_string()
                    .parse()
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            );
            Ok(response)
        }
        Ok(None) => {
            // Best-effort: the endpoint already refuses the missing object;
            // catalogue reconciliation can remove the stale location later.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetMissingInternalIndex,
                crate::store_result::Discard::BestEffort,
                state
                    .store
                    .forget_cluster_fragment_index_location(&cache_key, &state.node_id)
                    .await,
            );
            Err(StatusCode::NOT_FOUND)
        }
        Err(error) => {
            tracing::warn!(cache_key, %error, "refusing a corrupt fragment-index blob");
            crate::fragment_index_cluster::remove_local_blob(&root, &cache_key).await;
            // Best-effort: the corrupt object was removed and cannot be
            // served; catalogue reconciliation retries stale-row cleanup.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetCorruptInternalIndex,
                crate::store_result::Discard::BestEffort,
                state
                    .store
                    .forget_cluster_fragment_index_location(&cache_key, &state.node_id)
                    .await,
            );
            Err(StatusCode::NOT_FOUND)
        }
    }
}

const SUBTITLE_SOURCE_PATH_PREFIX: &str = "/internal/media/subtitle-source/";

/// Serve only a representation named by this node's manifest. The source
/// video is never opened on this route; a missing or corrupt stored object
/// is a 404 so the requester can try another published holder.
pub(crate) async fn subtitle_source(
    State(state): State<AppState>,
    AxumPath((file_id, ordinal, format)): AxumPath<(i64, i64, String)>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    if file_id <= 0 || ordinal < 0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let format = parse_subtitle_format(&format).ok_or(StatusCode::NOT_FOUND)?;
    let path = format!(
        "{SUBTITLE_SOURCE_PATH_PREFIX}{file_id}/{ordinal}/{}",
        format_name(format)
    );
    let peer_id = authorize(&state, &headers, "GET", &path, &[]).await?;
    serve_subtitle_source(&state, file_id, ordinal, format, peer_id).await
}

async fn serve_subtitle_source(
    state: &AppState,
    file_id: i64,
    ordinal: i64,
    format: crate::subtitle_source::RepresentationFormat,
    peer_id: String,
) -> Result<Response, StatusCode> {
    if !subtitle_cluster_source_enabled(state.store.as_ref()).await {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let read_permit = std::sync::Arc::clone(&SUBTITLE_SOURCE_READS)
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let peer_reads = SUBTITLE_SOURCE_PEER_READS
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .entry(peer_id)
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone();
    let peer_permit = peer_reads
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let root = crate::subtitle_source::store_root(&state.runtime_cache_dir);
    let (file, bytes) =
        crate::subtitle_source::open_verified_for_peer(&root, file_id, ordinal, format)
            .await
            .ok_or(StatusCode::NOT_FOUND)?;
    let stream = FragmentIndexStream {
        inner: tokio_util::io::ReaderStream::with_capacity(
            tokio::fs::File::from_std(file).take(bytes),
            MEDIA_BODY_READ_BUFFER,
        ),
        _global_permit: read_permit,
        _peer_permit: peer_permit,
        subtitle_bytes: true,
    };
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "private, no-store".parse().expect("static cache control"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/octet-stream"
            .parse()
            .expect("static content type"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        bytes
            .to_string()
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    Ok(response)
}

async fn subtitle_cluster_source_enabled(store: &dyn plurx_core::store::Store) -> bool {
    store
        .get_setting(plurx_core::store::keys::SUBTITLE_CLUSTER_SOURCES)
        .await
        .ok()
        .is_some_and(|value| plurx_core::store::stored_switch(value.as_deref(), false))
}

fn parse_subtitle_format(value: &str) -> Option<crate::subtitle_source::RepresentationFormat> {
    use crate::subtitle_source::RepresentationFormat;
    match value {
        "sup" => Some(RepresentationFormat::Sup),
        "webvtt" => Some(RepresentationFormat::Webvtt),
        "matroska" => Some(RepresentationFormat::Matroska),
        _ => None,
    }
}

fn format_name(format: crate::subtitle_source::RepresentationFormat) -> &'static str {
    use crate::subtitle_source::RepresentationFormat;
    match format {
        RepresentationFormat::Sup => "sup",
        RepresentationFormat::Webvtt => "webvtt",
        RepresentationFormat::Matroska => "matroska",
    }
}

pub(crate) async fn offers(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<MediaOffer>, StatusCode> {
    authorize(&state, &headers, "POST", OFFERS_PATH, &body).await?;
    let request = serde_json::from_slice::<MediaOfferRequest>(&body)
        .ok()
        .filter(MediaOfferRequest::is_valid)
        .ok_or(StatusCode::BAD_REQUEST)?;
    Ok(Json(local_offer(&state, &request).await))
}

pub(crate) async fn shared_cache_canary(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<crate::shared_cache::CanaryResponse>, StatusCode> {
    authorize(
        &state,
        &headers,
        "POST",
        crate::shared_cache::CANARY_PATH,
        &body,
    )
    .await?;
    let request = serde_json::from_slice::<crate::shared_cache::CanaryRequest>(&body)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    state
        .shared_cache
        .answer_canary(&request)
        .await
        .map(Json)
        .map_err(|_| StatusCode::CONFLICT)
}

/// At most two playback ranges execute on a node. HTTP cancellation or the
/// worker deadline drops the job-owned FFmpeg and its private temporary file.
pub(crate) async fn subtitle_range(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let peer = authorize(
        &state,
        &headers,
        "POST",
        crate::subtitle_ranges::PATH,
        &body,
    )
    .await?;
    if !state.transcode.pretranscode_worker_idle() {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    static WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    static PEERS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::LazyLock::new(Default::default);
    let _permit = WORKERS
        .try_acquire()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    struct PeerClaim(String);
    impl Drop for PeerClaim {
        fn drop(&mut self) {
            PEERS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.0);
        }
    }
    if !PEERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(peer.clone())
    {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let _peer_claim = PeerClaim(peer);
    let request = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let answer = tokio::time::timeout(
        crate::subtitle_ranges::WORK_BUDGET,
        crate::subtitle_ranges::execute(&state, request),
    )
    .await
    .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
    .map_err(|error| {
        tracing::debug!(%error, "subtitle peer range refused");
        StatusCode::CONFLICT
    })?;
    let body = serde_json::to_vec(&answer).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let auth = exact_auth_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let payload = super::peer_transport::signed_response_payload(StatusCode::OK.as_u16(), &body);
    let signature = state
        .membership
        .sign_internal_peer_response(
            &auth.node_id,
            &auth.nonce,
            crate::subtitle_ranges::PATH,
            &payload,
        )
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(super::peer_transport::RESPONSE_SIGNATURE_HEADER, signature)
        .body(Body::from(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<String, StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let node_id = auth.node_id.clone();
    if state
        .membership
        .authorize_internal_peer_request(&auth, method, path, body)
        .await
        .unwrap_or(false)
    {
        Ok(node_id)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

pub(crate) async fn directory(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Json<MediaDirectoryDiagnostics> {
    Json(state.media_pool.diagnostics(&state).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticOfferRequest {
    file_id: i64,
    target_height: i64,
    #[serde(default)]
    start_millis: i64,
    #[serde(default)]
    audio_index: Option<i64>,
    #[serde(default)]
    subtitle_index: Option<i64>,
    #[serde(default)]
    hdr10: bool,
}

pub(crate) async fn diagnostic_offers(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(input): Json<DiagnosticOfferRequest>,
) -> Result<Json<PlacementDiagnostics>, ApiError> {
    if input.file_id <= 0
        || !(crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT)
            .contains(&input.target_height)
        || input.start_millis < 0
        || input.audio_index.is_some_and(|index| index < 0)
        || input.subtitle_index.is_some_and(|index| index < 0)
    {
        return Err(ApiError::BadRequest(
            "invalid media-offer diagnostic request".to_owned(),
        ));
    }
    let file = state
        .store
        .get_file(input.file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let request = MediaOfferRequest::new(
        &file,
        input.target_height,
        input.start_millis,
        input.audio_index,
        input.subtitle_index,
        input.hdr10,
    )
    .map_err(|reason| ApiError::BadRequest(reason.to_owned()))?;
    Ok(Json(state.media_pool.offers(&state, request).await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle_source::{self as source, RepresentationFormat};

    fn peer_fixture(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let base = crate::test_tempdir().expect("store fixture");
        let root = base.path().join(source::STORE_DIR);
        let track = source::testing::kept(&source::file_dir(&root, 7), 2, bytes);
        let artifact = source::file_dir(&root, 7).join(track.file.as_deref().expect("stored name"));
        let stamp = source::SourceStamp {
            size: 1,
            mtime: 2,
            dev: None,
            ino: None,
        };
        source::testing::write_manifest(&root, 7, stamp, vec![track]);
        (base, root, artifact)
    }

    #[test]
    fn successful_snapshots_are_private_and_never_cacheable() {
        assert_eq!(
            private_no_store_headers(),
            [(header::CACHE_CONTROL, "private, no-store")]
        );
    }

    #[tokio::test]
    async fn fragment_index_stream_holds_budgets_until_body_drop() {
        let global = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let peer = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let stream = FragmentIndexStream {
            inner: futures_util::stream::pending::<Result<Bytes, std::io::Error>>(),
            _global_permit: std::sync::Arc::clone(&global)
                .try_acquire_owned()
                .expect("global permit"),
            _peer_permit: std::sync::Arc::clone(&peer)
                .try_acquire_owned()
                .expect("peer permit"),
            subtitle_bytes: false,
        };
        let body = Body::from_stream(stream);
        assert!(std::sync::Arc::clone(&global).try_acquire_owned().is_err());
        assert!(std::sync::Arc::clone(&peer).try_acquire_owned().is_err());
        drop(body);
        assert!(std::sync::Arc::clone(&global).try_acquire_owned().is_ok());
        assert!(std::sync::Arc::clone(&peer).try_acquire_owned().is_ok());
    }

    #[tokio::test]
    async fn household_bearers_never_authorize_internal_media_routes() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            "Bearer household-token"
                .parse()
                .expect("static authorization header"),
        );
        assert_eq!(
            snapshot(State(state.clone()), headers.clone())
                .await
                .expect_err("household bearer must not authorize a media snapshot"),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            offers(State(state), headers, Bytes::from_static(b"{}"))
                .await
                .expect_err("household bearer must not authorize a media offer"),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn subtitle_range_rejects_unsigned_work_before_reading_file() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let error = subtitle_range(State(state), HeaderMap::new(), Bytes::from_static(b"{}"))
            .await
            .expect_err("unsigned range work");
        assert_eq!(error, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn handler_refuses_with_switch_off() {
        let (_, state) = crate::http::tests::test_app_with_state();
        assert_eq!(
            serve_subtitle_source(&state, 7, 2, RepresentationFormat::Sup, "peer".into())
                .await
                .expect_err("off switch refuses the response"),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn subtitle_source_body_cap_enforced() {
        let (_base, root, artifact) = peer_fixture(b"subtitle");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(artifact)
            .expect("stored artifact");
        file.set_len(source::MAX_TRACK_BYTES + 1)
            .expect("sparse over-cap artifact");
        assert!(
            source::open_verified_for_peer(&root, 7, 2, RepresentationFormat::Sup)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn missing_or_corrupt_artifact_is_not_served() {
        let (_base, root, artifact) = peer_fixture(b"subtitle");
        std::fs::write(&artifact, b"changed!").expect("corrupt artifact");
        assert!(
            source::open_verified_for_peer(&root, 7, 2, RepresentationFormat::Sup)
                .await
                .is_none()
        );
        std::fs::remove_file(artifact).expect("remove artifact");
        assert!(
            source::open_verified_for_peer(&root, 7, 2, RepresentationFormat::Sup)
                .await
                .is_none()
        );
    }
}
