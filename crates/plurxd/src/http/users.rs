//! User management (admin only). The rules exist to make lockouts
//! impossible: the last admin can be neither deleted nor demoted, and you
//! cannot delete yourself. Password resets revoke the target's sessions.
//! A forgotten last-admin password currently has no safe console reset path:
//! the legacy direct-store command is refused because it cannot invalidate
//! daemon-local recovery proofs. Admins can still reset other users here.

use axum::extract::{Path, State};
use axum::Json;
use plurx_core::auth::{self, TokenIdlePolicy};
use plurx_core::error::StoreError;
use plurx_core::store::{keys, DeleteTokenByPrefixOutcome, Store, TokenSummary};
use serde::{Deserialize, Serialize};

use super::dto::UserDto;
use super::error::ApiError;
use super::extract::{AdminUser, AuthUser, RawToken};
use super::internal_auth_revocation::ClusterCacheRevocation;
use crate::state::AppState;

/// Unix seconds, for stamping when sign-in expiry takes effect.
pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Start the sign-in expiry clock the first time this server runs a build
/// that has the option, and return when it started.
///
/// "Sign-ins expire" defaults to on, so the moment an upgraded server first
/// starts is the moment expiry takes effect. Every token's idle window is
/// measured from no earlier than this, which is what keeps the upgrade from
/// signing out every device that was idle for a while before it. The seed is
/// written once and the first committed value wins on every voter; later
/// starts, and every other node, read it back unchanged.
pub(crate) async fn start_token_expiry_clock(
    store: &dyn Store,
    now: i64,
) -> Result<i64, StoreError> {
    let stored = store
        .get_or_init_setting(keys::AUTH_TOKEN_EXPIRY_SINCE, &now.to_string())
        .await?;
    Ok(stored.trim().parse().unwrap_or(now))
}

/// The expiry policy in force, from one settings snapshot.
pub(crate) fn token_expiry_policy(
    settings: &std::collections::BTreeMap<String, String>,
) -> Option<TokenIdlePolicy> {
    TokenIdlePolicy::from_settings(
        settings
            .get(keys::AUTH_TOKEN_EXPIRY_ENABLED)
            .map(String::as_str),
        settings.get(keys::AUTH_TOKEN_IDLE_DAYS).map(String::as_str),
        settings
            .get(keys::AUTH_TOKEN_EXPIRY_SINCE)
            .map(String::as_str),
    )
}

/// One row of a devices list: the Store's privacy-safe summary plus when the
/// device will be signed out if it stays unused. `expires_at` is absent while
/// sign-ins do not expire.
#[derive(Debug, Serialize)]
pub struct DeviceDto {
    #[serde(flatten)]
    pub token: TokenSummary,
    pub expires_at: Option<i64>,
    pub expired: bool,
}

fn device_dto(token: TokenSummary, policy: Option<TokenIdlePolicy>, now: i64) -> DeviceDto {
    let expires_at = policy.map(|policy| policy.expires_at(token.last_seen_at));
    DeviceDto {
        expired: expires_at.is_some_and(|at| now >= at),
        expires_at,
        token,
    }
}

/// GET /api/v1/me/devices
pub async fn list_my_devices(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<DeviceDto>>, ApiError> {
    list_devices_for_user(&state, user.id).await
}

/// DELETE /api/v1/me/devices/{prefix}
pub async fn revoke_my_device(
    AuthUser(user): AuthUser,
    RawToken(token): RawToken,
    State(state): State<AppState>,
    Path(prefix): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    revoke_device_for_user(&state, user.id, &token, &prefix).await
}

/// GET /api/v1/users/{id}/devices (admin)
pub async fn list_user_devices(
    AdminUser(_admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<DeviceDto>>, ApiError> {
    state
        .store
        .get_user(id)
        .await?
        .ok_or(ApiError::NotFound("user"))?;
    list_devices_for_user(&state, id).await
}

/// DELETE /api/v1/users/{id}/devices/{prefix} (admin)
pub async fn revoke_user_device(
    AdminUser(_admin): AdminUser,
    RawToken(token): RawToken,
    State(state): State<AppState>,
    Path((id, prefix)): Path<(i64, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .store
        .get_user(id)
        .await?
        .ok_or(ApiError::NotFound("user"))?;
    revoke_device_for_user(&state, id, &token, &prefix).await
}

async fn list_devices_for_user(
    state: &AppState,
    user_id: i64,
) -> Result<Json<Vec<DeviceDto>>, ApiError> {
    let tokens = state.store.list_tokens_for_user(user_id).await?;
    let policy = token_expiry_policy(&state.store.settings_snapshot().await?);
    let now = unix_now();
    Ok(Json(
        tokens
            .into_iter()
            .map(|token| device_dto(token, policy, now))
            .collect(),
    ))
}

async fn revoke_device_for_user(
    state: &AppState,
    user_id: i64,
    current_token: &str,
    prefix: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let prefix = normalize_token_prefix(prefix)?;
    let matches = state
        .store
        .list_tokens_for_user(user_id)
        .await?
        .into_iter()
        .filter(|token| token.token_hash_prefix == prefix)
        .count();
    match matches {
        0 => return Err(ApiError::NotFound("device token")),
        1 if prefix == token_prefix(current_token) => {
            return Err(ApiError::BadRequest(
                "the current device must sign out through /api/v1/auth/logout".into(),
            ));
        }
        1 => {}
        _ => {
            return Err(ApiError::Conflict(
                "device token prefix is ambiguous; refresh the device list".into(),
            ));
        }
    }

    let proof_revocation = ClusterCacheRevocation::begin_user(state, user_id).await?;
    let outcome = state
        .store
        .delete_token_by_prefix_for_user(user_id, &prefix, proof_revocation.mutation_claim())
        .await?;
    proof_revocation.finish(state).await?;
    match outcome {
        DeleteTokenByPrefixOutcome::Deleted => Ok(Json(serde_json::json!({ "ok": true }))),
        DeleteTokenByPrefixOutcome::NotFound => Err(ApiError::NotFound("device token")),
        DeleteTokenByPrefixOutcome::Ambiguous => Err(ApiError::Conflict(
            "device token prefix is ambiguous; refresh the device list".into(),
        )),
        DeleteTokenByPrefixOutcome::ClaimLost => Err(ApiError::ServiceUnavailable(
            "device revocation lost its cache-revocation exclusion; retry the request".into(),
        )),
    }
}

fn token_prefix(token: &str) -> String {
    auth::hash_token(token).chars().take(8).collect()
}

fn normalize_token_prefix(prefix: &str) -> Result<String, ApiError> {
    if prefix.len() != 8 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ApiError::BadRequest(
            "device token prefix must be exactly 8 hexadecimal characters".into(),
        ));
    }
    Ok(prefix.to_ascii_lowercase())
}

/// GET /api/v1/users (admin)
pub async fn list(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<UserDto>>, ApiError> {
    let users = state.store.list_users().await?;
    Ok(Json(users.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize)]
pub struct CreateUser {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub is_admin: bool,
}

/// POST /api/v1/users (admin)
pub async fn create(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<CreateUser>,
) -> Result<Json<UserDto>, ApiError> {
    let username = req.username.trim();
    if username.is_empty() {
        return Err(ApiError::BadRequest("username required".into()));
    }
    super::auth::validate_new_password(&req.password)?;
    if state.store.get_user_by_username(username).await?.is_some() {
        return Err(ApiError::Conflict(format!(
            "a user named `{username}` already exists"
        )));
    }
    let hash = super::auth::hash_password_bounded(&state.password_capacity, req.password).await?;
    let user = state
        .store
        .create_user(username, &hash, req.is_admin)
        .await?;
    Ok(Json(user.into()))
}

#[derive(Deserialize)]
pub struct UpdateUser {
    /// New password (resets the user's sessions). Absent = unchanged.
    pub password: Option<String>,
    /// Grant or revoke admin. Absent = unchanged.
    pub is_admin: Option<bool>,
}

/// PUT /api/v1/users/:id (admin)
pub async fn update(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUser>,
) -> Result<Json<UserDto>, ApiError> {
    state
        .store
        .get_user(id)
        .await?
        .ok_or(ApiError::NotFound("user"))?;

    let password_hash = match req.password.as_deref() {
        Some(password) => {
            super::auth::validate_new_password(password)?;
            Some(
                super::auth::hash_password_bounded(&state.password_capacity, password.to_owned())
                    .await?,
            )
        }
        None => None,
    };
    let needs_revocation = req.is_admin == Some(false) || password_hash.is_some();
    let mut proof_revocation = if needs_revocation {
        Some(ClusterCacheRevocation::begin_user(&state, id).await?)
    } else {
        None
    };
    let combined_promotion = req.is_admin == Some(true) && password_hash.is_some();

    if let (Some(true), Some(hash)) = (req.is_admin, password_hash.as_deref()) {
        let changed = state
            .store
            .promote_user_and_reset_password(
                id,
                hash,
                proof_revocation
                    .as_ref()
                    .and_then(ClusterCacheRevocation::mutation_claim),
            )
            .await?;
        if !changed {
            return Err(ApiError::ServiceUnavailable(
                "combined promotion lost its cache-revocation exclusion; retry the request".into(),
            ));
        }
    }

    if let Some(is_admin) = req.is_admin.filter(|_| !combined_promotion) {
        if is_admin {
            state.store.set_admin(id, true).await?;
        } else if !state
            .store
            .demote_user_preserving_admin(
                id,
                proof_revocation
                    .as_ref()
                    .and_then(ClusterCacheRevocation::mutation_claim),
            )
            .await?
        {
            proof_revocation
                .take()
                .expect("demotion owns a cache revocation fence")
                .finish(&state)
                .await?;
            return Err(ApiError::Conflict(
                "cannot remove admin from the last admin account".into(),
            ));
        }
    }
    if let Some(hash) = password_hash.filter(|_| !combined_promotion) {
        let changed = state
            .store
            .reset_password_and_revoke_tokens(
                id,
                &hash,
                proof_revocation
                    .as_ref()
                    .and_then(ClusterCacheRevocation::mutation_claim),
            )
            .await?;
        if !changed {
            return Err(ApiError::ServiceUnavailable(
                "password reset lost its cache-revocation exclusion; retry the request".into(),
            ));
        }
    }
    if let Some(proof_revocation) = proof_revocation {
        proof_revocation.finish(&state).await?;
    }

    let user = state
        .store
        .get_user(id)
        .await?
        .ok_or(ApiError::NotFound("user"))?;
    Ok(Json(user.into()))
}

/// DELETE /api/v1/users/:id (admin)
pub async fn delete(
    _admin: AdminUser,
    AuthUser(caller): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let target = state
        .store
        .get_user(id)
        .await?
        .ok_or(ApiError::NotFound("user"))?;
    if target.id == caller.id {
        return Err(ApiError::BadRequest(
            "you cannot delete the account you are signed in with".into(),
        ));
    }
    // Tokens and watch state go with the user (ON DELETE CASCADE).
    let proof_revocation = ClusterCacheRevocation::begin_user(&state, id).await?;
    if !state
        .store
        .delete_user_preserving_admin(id, proof_revocation.mutation_claim())
        .await?
    {
        proof_revocation.finish(&state).await?;
        if state.store.get_user(id).await?.is_none() {
            return Err(ApiError::NotFound("user"));
        }
        return Err(ApiError::Conflict("cannot delete the last admin".into()));
    }
    proof_revocation.finish(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
