//! Dedicated support transport: never forwards Cinema request credentials.
use crate::{http::extract::AuthUser, state::AppState};
use axum::{
    body::Bytes,
    extract::State,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use futures_util::StreamExt;
use plurx_core::{
    config::SupportConfig,
    diagnostics::{canonical_uuid, strict_json},
    domain::User,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

pub(crate) struct Publisher {
    config: SupportConfig,
    client: Option<reqwest::Client>,
    permits: tokio::sync::Semaphore,
}
impl Publisher {
    pub(crate) fn new(config: SupportConfig) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .ok();
        Self {
            config,
            client,
            permits: tokio::sync::Semaphore::new(4),
        }
    }
    pub(crate) fn capabilities(&self) -> Value {
        json!({"configured": self.config.configured(), "origin": self.config.origin, "enrollment_revocation_delay_ms":86400000})
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Value,
    ) -> Result<Value, StatusCode> {
        if !self.config.configured() {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        let _permit = self
            .permits
            .try_acquire()
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
        let meta = tokio::fs::symlink_metadata(&self.config.publisher_secret_file)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if !meta.is_file() || meta.len() != 64 {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
        let secret = tokio::fs::read_to_string(&self.config.publisher_secret_file)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if !secret
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        let response = self
            .client
            .as_ref()
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
            .request(
                method,
                format!(
                    "{}/support/v1/{path}",
                    self.config.origin.trim_end_matches('/')
                ),
            )
            .bearer_auth(secret)
            .header("X-Publisher-Id", &self.config.publisher_id)
            .json(&body)
            .send()
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let status = response.status();
        if !status.is_success() {
            return Err(if status.as_u16() == 429 {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            });
        }
        let mut bytes = Vec::new();
        let stream = response.bytes_stream();
        futures_util::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            if bytes.len() + chunk.len() > 16384 {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            bytes.extend_from_slice(&chunk);
        }
        strict_json(&bytes, 16384).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    installation_id: String,
}
fn principal(user: &User, installation: &str, publisher: &str) -> Result<String, StatusCode> {
    if !canonical_uuid(installation) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(hex::encode(Sha256::digest(
        format!(
            "cinema.diagnostic-installation.v1/{publisher}/{}/{installation}",
            user.id
        )
        .as_bytes(),
    )))
}
fn private(value: impl IntoResponse) -> Response {
    let mut r = value.into_response();
    r.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    r
}
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        // Preserve the outer router's literal Plex `:` routes when merged.
        .without_v07_checks()
        .route("/api/v1/diagnostics/upload-grants", post(grant))
        .route(
            "/api/v1/diagnostics/enrollments",
            post(enroll).delete(revoke),
        )
}
pub(crate) fn eligible(method: &Method, path: &str) -> bool {
    method == Method::POST && path == "/api/v1/diagnostics/upload-grants"
        || (method == Method::POST || method == Method::DELETE)
            && path == "/api/v1/diagnostics/enrollments"
}
async fn grant(
    AuthUser(_user): AuthUser,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let value = strict_json(&body, 16384).map_err(|_| StatusCode::BAD_REQUEST)?;
    let o = value.as_object().ok_or(StatusCode::BAD_REQUEST)?;
    if o.len() != 4
        || !o.contains_key("report_id")
        || !o.contains_key("sha256")
        || !o.contains_key("length")
        || !o.contains_key("privacy")
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let id = value["report_id"].as_str().ok_or(StatusCode::BAD_REQUEST)?;
    let hash = value["sha256"].as_str().ok_or(StatusCode::BAD_REQUEST)?;
    if !canonical_uuid(id)
        || hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !value["length"]
            .as_u64()
            .is_some_and(|n| n > 0 && n <= 16 * 1024 * 1024)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let _: plurx_core::diagnostics::Privacy =
        serde_json::from_value(value["privacy"].clone()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let recorder = crate::telemetry::diagnostics(Arc::clone(&state.store));
    let publisher = recorder
        .publisher()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let result = publisher
        .request(reqwest::Method::POST, "grants", value)
        .await?;
    Ok(private(Json(
        json!({"origin":publisher.config.origin,"grant":result}),
    )))
}
async fn installation(
    state: &AppState,
    user: &User,
    body: Bytes,
    method: reqwest::Method,
) -> Result<Response, StatusCode> {
    let input: Installation =
        serde_json::from_value(strict_json(&body, 16384).map_err(|_| StatusCode::BAD_REQUEST)?)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    let recorder = crate::telemetry::diagnostics(Arc::clone(&state.store));
    let publisher = recorder
        .publisher()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let bound = principal(user, &input.installation_id, &publisher.config.publisher_id)?;
    let result = publisher
        .request(method, "enrollments", json!({"principal":bound}))
        .await?;
    Ok(private(Json(
        json!({"origin":publisher.config.origin,"authorization":result}),
    )))
}
async fn enroll(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    installation(&state, &user, body, reqwest::Method::POST).await
}
async fn revoke(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    installation(&state, &user, body, reqwest::Method::DELETE).await
}
