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

use super::error::ApiError;
use super::extract::AdminUser;
use super::peer_transport::exact_auth_from_headers;
use crate::media_pool::{
    local_offer, local_snapshot, MediaDirectoryDiagnostics, MediaOffer, MediaOfferRequest,
    PlacementDiagnostics, OFFERS_PATH, SNAPSHOT_PATH,
};
use crate::state::AppState;

/// Authenticated peers are still fallible. Bound verified descriptor streams
/// to four concurrent responses so a buggy voter cannot accumulate open files
/// or queued response bodies.
static FRAGMENT_INDEX_READS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)));
static FRAGMENT_INDEX_PEER_READS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Semaphore>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// The permits live in the response stream, not the request handler. A slow
/// or non-reading peer therefore occupies both budgets until EOF or body drop.
struct FragmentIndexStream<S> {
    inner: S,
    _global_permit: tokio::sync::OwnedSemaphorePermit,
    _peer_permit: tokio::sync::OwnedSemaphorePermit,
}

impl<S, E> Stream for FragmentIndexStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, E>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(context)
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
                inner: tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024),
                _global_permit: read_permit,
                _peer_permit: peer_permit,
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
            let _ = state
                .store
                .forget_cluster_fragment_index_location(&cache_key, &state.node_id)
                .await;
            Err(StatusCode::NOT_FOUND)
        }
        Err(error) => {
            tracing::warn!(cache_key, %error, "refusing a corrupt fragment-index blob");
            crate::fragment_index_cluster::remove_local_blob(&root, &cache_key).await;
            let _ = state
                .store
                .forget_cluster_fragment_index_location(&cache_key, &state.node_id)
                .await;
            Err(StatusCode::NOT_FOUND)
        }
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
}
