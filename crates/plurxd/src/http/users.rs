//! User management (admin only). The rules exist to make lockouts
//! impossible: the last admin can be neither deleted nor demoted, and you
//! cannot delete yourself. Password resets revoke the target's sessions.
//! A forgotten last-admin password currently has no safe console reset path:
//! the legacy direct-store command is refused because it cannot invalidate
//! daemon-local recovery proofs. Admins can still reset other users here.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use super::dto::UserAccessDto;
use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use super::internal_auth_revocation::ClusterCacheRevocation;
use crate::state::AppState;

/// GET /api/v1/users (admin)
pub async fn list(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<UserAccessDto>>, ApiError> {
    let users = state.store.list_users().await?;
    let mut result = Vec::with_capacity(users.len());
    for user in users {
        result.push(managed_user(&state, user).await?);
    }
    Ok(Json(result))
}

async fn managed_user(
    state: &AppState,
    user: plurx_core::domain::User,
) -> Result<UserAccessDto, ApiError> {
    let optical_play = state.store.optical_play_grant(user.id).await? == Some(true);
    Ok(UserAccessDto {
        user: user.into(),
        optical_play,
    })
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
) -> Result<Json<UserAccessDto>, ApiError> {
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
    let hash = super::auth::hash_password_bounded(req.password).await?;
    let user = state
        .store
        .create_user(username, &hash, req.is_admin)
        .await?;
    Ok(Json(managed_user(&state, user).await?))
}

#[derive(Deserialize)]
pub struct UpdateUser {
    /// New password (resets the user's sessions). Absent = unchanged.
    pub password: Option<String>,
    /// Grant or revoke admin. Absent = unchanged.
    pub is_admin: Option<bool>,
    /// Grant or revoke access to physical optical titles. Absent = unchanged.
    pub optical_play: Option<bool>,
}

/// PUT /api/v1/users/:id (admin)
pub async fn update(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUser>,
) -> Result<Json<UserAccessDto>, ApiError> {
    state
        .store
        .get_user(id)
        .await?
        .ok_or(ApiError::NotFound("user"))?;

    let password_hash = match req.password.as_deref() {
        Some(password) => {
            super::auth::validate_new_password(password)?;
            Some(super::auth::hash_password_bounded(password.to_owned()).await?)
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
    if let Some(granted) = req.optical_play {
        state.store.set_optical_play_grant(id, granted).await?;
    }
    if let Some(proof_revocation) = proof_revocation {
        proof_revocation.finish(&state).await?;
    }

    let user = state
        .store
        .get_user(id)
        .await?
        .ok_or(ApiError::NotFound("user"))?;
    Ok(Json(managed_user(&state, user).await?))
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
