//! Cluster-only media snapshots/offers and admin diagnostics.
//!
//! Internal routes accept only exact, short-lived voter signatures. The
//! `/api/v1` routes are separate admin views; neither surface starts work.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::Json;
use serde::Deserialize;

use super::error::ApiError;
use super::extract::AdminUser;
use super::peer_transport::exact_auth_from_headers;
use crate::media_pool::{
    local_offer, local_snapshot, MediaDirectoryDiagnostics, MediaOffer, MediaOfferRequest,
    PlacementDiagnostics, OFFERS_PATH, SNAPSHOT_PATH,
};
use crate::state::AppState;

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
) -> Result<(), StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if state
        .membership
        .authorize_internal_peer_request(&auth, method, path, body)
        .await
        .unwrap_or(false)
    {
        Ok(())
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
