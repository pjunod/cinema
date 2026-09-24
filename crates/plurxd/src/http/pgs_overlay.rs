//! Authenticated `pgs-v1` manifest and immutable object routes.

use std::path::Path;

use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use plurx_core::domain::MediaFile;
use plurx_core::tracks::is_pgs_subtitle;

use super::error::ApiError;
use super::extract::AuthUser;
use crate::pgs_overlay::{self, OverlayError, PrepareState};
use crate::state::AppState;

async fn file_and_track(state: &AppState, id: i64, index: i64) -> Result<MediaFile, ApiError> {
    if !state.pgs_overlay_enabled().await? {
        // Counted only once the request is one the overlay would really have
        // answered. Counting here, before the track exists and before its
        // codec is checked, would let any authenticated `GET /files/999/subs/0`
        // read as "a client here wants this capability" — and the readiness
        // row says exactly that, so it has to be true. The switch is read
        // first because it is the cheaper question.
        let refusable = state
            .store
            .get_file(id)
            .await?
            .and_then(|file| {
                file.subtitle_streams
                    .get(usize::try_from(index).ok()?)
                    .map(|stream| is_pgs_subtitle(&stream.codec))
            })
            .unwrap_or(false);
        if refusable {
            OVERLAY_REFUSED_OFF.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Err(ApiError::NotFound("PGS overlay"));
    }
    if index < 0 {
        return Err(ApiError::NotFound("subtitle track"));
    }
    let file = state
        .store
        .get_file(id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let stream = file
        .subtitle_streams
        .get(index as usize)
        .ok_or(ApiError::NotFound("subtitle track"))?;
    if !is_pgs_subtitle(&stream.codec) {
        return Err(ApiError::UnsupportedMedia(
            "subtitle track is not supported by pgs-v1".into(),
        ));
    }
    Ok(file)
}

/// Overlay manifests served since this process started, and refusals because
/// the switch is off.
///
/// The Developer readiness row for this switch has to answer "does anything in
/// this fleet actually render pgs-v1", and the only honest evidence a daemon
/// holds is whether a client ever asked for a manifest. Process-local, so a
/// restart returns it to zero — the row says so rather than presenting a fresh
/// process's silence as a fleet fact.
pub(crate) static OVERLAY_MANIFESTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static OVERLAY_REFUSED_OFF: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// What this process has seen of the overlay, for the readiness route.
pub(crate) fn overlay_demand_snapshot() -> (u64, u64) {
    use std::sync::atomic::Ordering;
    (
        OVERLAY_MANIFESTS.load(Ordering::Relaxed),
        OVERLAY_REFUSED_OFF.load(Ordering::Relaxed),
    )
}

pub async fn manifest(
    _user: AuthUser,
    State(state): State<AppState>,
    AxPath((id, index)): AxPath<(i64, i64)>,
) -> Result<Response, ApiError> {
    let file = file_and_track(&state, id, index).await?;
    match pgs_overlay::prepare(
        &state.subs_dir,
        &file,
        index,
        state.subtitle_source_access(),
    )
    .await
    .map_err(map_overlay_error)?
    {
        // A 202 is not a serving. The client polls this every second while a
        // cold prepare runs, so counting here would record sixty servings and
        // zero manifests for one minute of waiting, and the readiness row
        // below claims manifests were served.
        PrepareState::Preparing => Ok(preparing_response()),
        PrepareState::Ready(path) => {
            OVERLAY_MANIFESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(generation_dir) = path.parent() {
                pgs_overlay::record_access(generation_dir).await;
            }
            let bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(error = %error, "published PGS manifest vanished; rebuilding");
                    if let Some(generation_dir) = path.parent() {
                        reprepare(&state, &file, index, generation_dir).await?;
                    }
                    return Ok(preparing_response());
                }
            };
            let etag = format!("\"{}\"", pgs_overlay::generation(&file, index));
            let mut response = (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-cache"),
            );
            response.headers_mut().insert(
                header::ETAG,
                HeaderValue::from_str(&etag)
                    .map_err(|error| ApiError::Internal(format!("PGS ETag: {error}")))?,
            );
            Ok(response)
        }
    }
}

pub async fn object(
    _user: AuthUser,
    State(state): State<AppState>,
    AxPath((id, index, generation, object)): AxPath<(i64, i64, String, String)>,
) -> Result<Response, ApiError> {
    let file = file_and_track(&state, id, index).await?;
    if generation != pgs_overlay::generation(&file, index) {
        return Err(ApiError::NotFound("PGS overlay generation"));
    }
    let hash = object
        .strip_suffix(".png")
        .ok_or(ApiError::NotFound("PGS overlay object"))?;
    let path = pgs_overlay::object_path(&state.subs_dir, &file, index, hash)
        .ok_or(ApiError::NotFound("PGS overlay object"))?;
    if tokio::fs::metadata(pgs_overlay::manifest_path(&state.subs_dir, &file, index))
        .await
        .is_err()
    {
        return Err(ApiError::NotFound("PGS overlay generation"));
    }
    if let Some(generation_dir) = path.parent().and_then(Path::parent) {
        pgs_overlay::record_access(generation_dir).await;
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(error = %error, "published PGS object vanished; rebuilding generation");
            let generation_dir = pgs_overlay::generation_dir(&state.subs_dir, &file, index);
            reprepare(&state, &file, index, &generation_dir).await?;
            return Err(ApiError::ServiceUnavailable(
                "PGS overlay generation is being rebuilt".into(),
            ));
        }
    };
    let etag = format!("\"{hash}\"");
    let mut response =
        (StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], bytes).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag)
            .map_err(|error| ApiError::Internal(format!("PGS object ETag: {error}")))?,
    );
    Ok(response)
}

fn preparing_response() -> Response {
    let mut response = (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "state": "preparing",
            "retry_after_ms": 1000
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

async fn reprepare(
    state: &AppState,
    file: &MediaFile,
    index: i64,
    generation_dir: &Path,
) -> Result<(), ApiError> {
    pgs_overlay::invalidate_generation(generation_dir).await;
    pgs_overlay::prepare(&state.subs_dir, file, index, state.subtitle_source_access())
        .await
        .map_err(map_overlay_error)?;
    Ok(())
}

/// A preparation that ran and failed. Terminal for this request: the failure
/// is remembered server-side, so polling again only replays it.
pub(crate) const PREPARE_FAILED: &str = "pgs_overlay_prepare_failed";
/// Both preparation slots are busy. The one overlay refusal worth waiting out.
pub(crate) const CAPACITY: &str = "pgs_overlay_capacity";
/// How long a client should wait before asking again when capacity is full.
pub(crate) const CAPACITY_RETRY_AFTER_SECS: u64 = 5;

/// The overlay's failures, on the wire.
///
/// Before this, every `Unavailable` was a plain 503, and 503 is what both
/// native clients read as "still preparing": a remembered demux failure or
/// timeout was polled for ten minutes and showed nothing, and capacity-full
/// — the one case that really is a wait — looked exactly the same. The plan's
/// error table (docs/clients/PGS_OVERLAY_PLAN.md §10.5) says 500 for a failed
/// extraction and 503 only for capacity; this is that table, with codes.
fn map_overlay_error(error: OverlayError) -> ApiError {
    match error {
        // The legacy `{error, detail}` fields stay beside the code, so a
        // reader of either shape still gets its answer.
        OverlayError::Malformed(why) => ApiError::typed_detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            PREPARE_FAILED,
            "PGS overlay preparation failed: the PGS stream is malformed",
            serde_json::json!({ "error": "malformed PGS stream", "detail": why }),
        ),
        OverlayError::Limit(why) => ApiError::typed_detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            PREPARE_FAILED,
            "PGS overlay preparation failed: the PGS stream exceeds a safety limit",
            serde_json::json!({ "error": "PGS safety limit exceeded", "detail": why }),
        ),
        OverlayError::SourceChanged => {
            ApiError::Conflict("media source changed while its PGS overlay was preparing".into())
        }
        OverlayError::Unavailable(why) => {
            tracing::warn!(error = %why, "PGS overlay preparation failed");
            ApiError::typed(
                StatusCode::INTERNAL_SERVER_ERROR,
                PREPARE_FAILED,
                "PGS overlay preparation failed",
            )
        }
        OverlayError::Capacity => ApiError::TypedRetry {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: CAPACITY,
            message: "PGS overlay preparation capacity is full".into(),
            retry_after_seconds: CAPACITY_RETRY_AFTER_SECS,
        },
        // Remembered like any other failed preparation (the cache could not be
        // created or synced), so it is terminal in the same words. The detail
        // is logged, never returned.
        OverlayError::Internal(why) => {
            tracing::error!(error = %why, "PGS overlay preparation failed internally");
            ApiError::typed(
                StatusCode::INTERNAL_SERVER_ERROR,
                PREPARE_FAILED,
                "PGS overlay preparation failed",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remembered_preparation_failure_is_a_typed_terminal_answer_not_a_503() {
        for failure in [
            OverlayError::Unavailable("PGS demux exited with status 1".into()),
            OverlayError::Unavailable("PGS overlay preparation timed out after 600.000s".into()),
            OverlayError::Unavailable("media duration is required for a PGS manifest".into()),
            OverlayError::Internal("creating PGS staging directory: permission denied".into()),
        ] {
            match map_overlay_error(failure) {
                ApiError::Typed { status, code, .. } => {
                    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
                    assert_eq!(code, PREPARE_FAILED);
                }
                other => panic!("a failed preparation must be typed, got {other:?}"),
            }
        }
        let (wire, _) = map_overlay_error(OverlayError::Unavailable("x".into()))
            .into_response()
            .into_parts();
        assert_ne!(wire.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(wire.headers.get(header::RETRY_AFTER).is_none());
    }

    #[test]
    fn a_malformed_or_over_limit_stream_keeps_its_422_and_gains_the_code() {
        for failure in [
            OverlayError::Malformed("bad segment".into()),
            OverlayError::Limit("too many objects".into()),
        ] {
            match map_overlay_error(failure) {
                ApiError::TypedDetail {
                    status,
                    code,
                    detail,
                    ..
                } => {
                    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
                    assert_eq!(code, PREPARE_FAILED);
                    assert!(detail.contains_key("error"));
                    assert!(detail.contains_key("detail"));
                }
                other => panic!("a rejected stream must be typed, got {other:?}"),
            }
        }
    }

    #[test]
    fn capacity_full_is_the_only_retryable_503() {
        match map_overlay_error(OverlayError::Capacity) {
            ApiError::TypedRetry {
                status,
                code,
                retry_after_seconds,
                ..
            } => {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(code, CAPACITY);
                assert_eq!(retry_after_seconds, CAPACITY_RETRY_AFTER_SECS);
            }
            other => panic!("capacity must be a retryable 503, got {other:?}"),
        }
        let (wire, _) = map_overlay_error(OverlayError::Capacity)
            .into_response()
            .into_parts();
        assert_eq!(wire.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            wire.headers.get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("5"))
        );
    }
}
