//! One deadline and one exact native request identity for transient start retries.
use super::*;
use plurx_core::store::JellyfinPlay;

fn retry_delay(
    error: &ApiError,
    encoded: bool,
    backoff: std::time::Duration,
) -> Option<std::time::Duration> {
    let retryable = matches!(
        error.code(),
        Some("startup_timeout" | "media_owner_transition" | "transcode_capacity_pending")
    ) || encoded && error.code() == Some("vod_engine_unattested");
    if !retryable {
        return None;
    }
    Some(match error {
        ApiError::TypedRetry {
            retry_after_seconds,
            ..
        } => std::time::Duration::from_secs(*retry_after_seconds).max(backoff),
        _ => backoff,
    })
}
fn public_failure(error: ApiError) -> ApiError {
    let status = match &error {
        ApiError::Typed { status, .. }
        | ApiError::TypedRetry { status, .. }
        | ApiError::TypedDetail { status, .. } => *status,
        ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
        ApiError::Forbidden => StatusCode::FORBIDDEN,
        ApiError::NotFound(_) => StatusCode::NOT_FOUND,
        ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
        ApiError::Conflict(_) => StatusCode::CONFLICT,
        ApiError::UnsupportedMedia(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        ApiError::Internal(_) => StatusCode::BAD_GATEWAY,
    };
    ApiError::typed(
        status,
        error.code().unwrap_or("native_playback_failed"),
        "native playback could not start",
    )
}
pub(super) async fn create(
    state: &AppState,
    play: &JellyfinPlay,
    vod: &Value,
) -> Result<super::super::hls::StartResponse, ApiError> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let exhausted = || {
        ApiError::typed(
            StatusCode::GATEWAY_TIMEOUT,
            "compatibility_start_timeout",
            "native playback did not start within its deadline",
        )
    };
    tokio::time::timeout_at(deadline, async {
        let user = state
            .store
            .get_user(play.negotiation.scope.user_id)
            .await?
            .ok_or(ApiError::Unauthorized)?;
        let encoded = vod["body"]["copy"] != true;
        let mut backoff = std::time::Duration::from_millis(100);
        let mut attempts = 0_u32;
        loop {
            let remaining = deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .as_millis();
            if remaining == 0 {
                return Err(exhausted());
            }
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-plurx-startup-remaining-ms",
                remaining
                    .to_string()
                    .parse()
                    .map_err(|_| ApiError::Internal("invalid startup allowance".into()))?,
            );
            let body = serde_json::from_value(vod["body"].clone()).map_err(|_| {
                ApiError::Conflict("native play selection changed; renegotiate".into())
            })?;
            let policy = super::super::hls::CompatibilityVodPolicy {
                bitrate_limit_bps: serde_json::from_value(vod["bitrate"].clone())?,
                expected_fingerprint: play.negotiation.native_request_fingerprint.clone(),
            };
            attempts += 1;
            match Box::pin(super::super::hls::create_for_compatibility(
                user.clone(),
                state.clone(),
                play.negotiation.file_id,
                headers,
                None,
                body,
                policy,
            ))
            .await
            {
                Ok(response) => return Ok(response),
                Err(error) => {
                    let Some(delay) = retry_delay(&error, encoded, backoff) else {
                        return Err(public_failure(error));
                    };
                    tracing::info!(target: "plurxd::jellyfin", attempts, code = error.code(), remaining_ms = remaining, "compatibility start retry remains within its original deadline");
                    let now = tokio::time::Instant::now();
                    let wake = now + delay.min(deadline.saturating_duration_since(now));
                    tokio::time::sleep_until(wake).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(1));
                }
            }
        }
    })
    .await
    .map_err(|_| exhausted())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_retries_only_measured_transients_and_keeps_unknown_or_index_errors_terminal() {
        let delay = std::time::Duration::from_millis(100);
        for code in [
            "startup_timeout",
            "media_owner_transition",
            "transcode_capacity_pending",
        ] {
            let error = ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                code,
                "private native detail",
            );
            assert_eq!(retry_delay(&error, false, delay), Some(delay));
        }
        let engine = ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "vod_engine_unattested",
            "private engine detail",
        );
        assert!(retry_delay(&engine, false, delay).is_none());
        assert_eq!(retry_delay(&engine, true, delay), Some(delay));
        for code in [
            "vod_index_pending",
            "media_session_starting",
            "vod_source_rescan_required",
            "unknown_failure",
        ] {
            let error = ApiError::typed(StatusCode::SERVICE_UNAVAILABLE, code, "private detail");
            assert!(retry_delay(&error, true, delay).is_none());
        }
        let hint = ApiError::TypedRetry {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "transcode_capacity_pending",
            message: "private detail".into(),
            retry_after_seconds: 3,
        };
        assert_eq!(
            retry_delay(&hint, true, delay),
            Some(std::time::Duration::from_secs(3))
        );
        let public = public_failure(hint).into_response();
        assert_eq!(public.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
