//! One-file capability URLs for external book readers.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::Json;
use plurx_core::auth;
use plurx_core::domain::ItemKind;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::AuthUser;
use crate::state::AppState;

const DEFAULT_TTL_SECS: i64 = 900;
const MAX_TTL_SECS: i64 = 3_600;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Deserialize)]
pub struct MintRequest {
    purpose: String,
    ttl_secs: Option<i64>,
}

#[derive(Serialize)]
pub struct MintResponse {
    url: String,
    expires_at: i64,
    grant_id: String,
}

pub async fn mint(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    AxPath(file_id): AxPath<i64>,
    Json(request): Json<MintRequest>,
) -> Result<(StatusCode, Json<MintResponse>), ApiError> {
    if request.purpose != "open_in" {
        return Err(ApiError::BadRequest(
            "unsupported file grant purpose".into(),
        ));
    }
    let ttl = request.ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
    if !(1..=MAX_TTL_SECS).contains(&ttl) {
        return Err(ApiError::BadRequest(format!(
            "ttl_secs must be between 1 and {MAX_TTL_SECS}"
        )));
    }
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let item = state
        .store
        .get_item(file.item_id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    if item.kind != ItemKind::Book {
        return Err(ApiError::NotFound("book content"));
    }
    let token = auth::generate_token().map_err(|error| ApiError::Internal(error.to_string()))?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_unix();
    let expires_at = now + ttl;
    state
        .store
        .create_file_grant(
            &id,
            &auth::hash_token(&token),
            file_id,
            user.id,
            now,
            expires_at,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(MintResponse {
            url: format!("/api/v1/grants/{token}/content"),
            expires_at,
            grant_id: id,
        }),
    ))
}

pub async fn content(
    State(state): State<AppState>,
    AxPath(token): AxPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ApiError::NotFound("grant"));
    }
    let grant = state
        .store
        .file_grant_by_hash(&auth::hash_token(&token))
        .await?
        .ok_or(ApiError::NotFound("grant"))?;
    if grant.revoked_at.is_some() || grant.expires_at <= now_unix() {
        return Err(ApiError::typed(
            StatusCode::GONE,
            "grant_gone",
            "The file grant has expired or was revoked.",
        ));
    }
    super::stream::serve_book_content(&state, grant.file_id, &method, &headers).await
}

pub async fn revoke(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<StatusCode, ApiError> {
    if state
        .store
        .revoke_file_grant(&id, user.id, now_unix())
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("grant"))
    }
}

pub async fn prune_loop(state: AppState, shutdown: tokio_util::sync::CancellationToken) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = interval.tick() => {
                let before = now_unix().saturating_sub(24 * 60 * 60);
                if let Err(error) = state.store.prune_file_grants(before).await {
                    tracing::warn!(%error, "file grant pruning failed");
                }
            }
        }
    }
}
