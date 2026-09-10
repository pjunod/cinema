//! Login, logout, and current-user endpoints.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use plurx_core::auth;
use serde::{Deserialize, Serialize};

use super::dto::UserDto;
use super::error::ApiError;
use super::extract::{AuthUser, RawToken};
use super::internal_auth_revocation::ClusterCacheRevocation;
use crate::state::AppState;

/// Human-entered secrets are small. The byte cap keeps every creation and
/// verification route on one contract and bounds request memory without
/// silently changing any existing stored hash.
pub(crate) const MAX_PASSWORD_BYTES: usize = 1024;
const PASSWORD_HASH_WORKERS: usize = 2;
const PASSWORD_HASH_WAITERS: usize = 16;
const PASSWORD_HASH_ADMISSION_WAIT: Duration = Duration::from_secs(2);

static PASSWORD_ACTIVE: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(PASSWORD_HASH_WORKERS)));
static PASSWORD_WAITERS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(PASSWORD_HASH_WAITERS)));

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub device: Option<String>,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserDto,
}

/// POST /api/v1/auth/login
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    validate_password_size(&req.password)?;
    let proof_ticket = state.cache_only_admin_proofs.authentication_ticket();
    let user = state.store.get_user_by_username(&req.username).await?;
    // Verify even on unknown user to keep timing uniform.
    let password = req.password;
    let (ok, user) = run_password_work(move || match user {
        Some(u) => {
            let ok = auth::verify_password(&password, &u.password_hash);
            (ok, Some(u))
        }
        None => {
            let _ = auth::verify_password(&password, &DUMMY_HASH);
            (false, None)
        }
    })
    .await?;
    let user = match (ok, user) {
        (true, Some(u)) => u,
        _ => return Err(ApiError::Unauthorized),
    };

    let token = auth::generate_token().map_err(|e| ApiError::Internal(e.to_string()))?;
    let hash = auth::hash_token(&token);
    state
        .store
        .create_token_if_password_matches(
            &hash,
            user.id,
            req.device.as_deref(),
            &user.password_hash,
        )
        .await?
        .then_some(())
        .ok_or(ApiError::Unauthorized)?;
    state
        .cache_only_admin_proofs
        .record_authenticated(proof_ticket, hash, &user);

    Ok(Json(LoginResponse {
        token,
        user: user.into(),
    }))
}

/// POST /api/v1/auth/logout — invalidate the presented token.
pub async fn logout(
    State(state): State<AppState>,
    _user: AuthUser,
    RawToken(token): RawToken,
) -> Result<Json<serde_json::Value>, ApiError> {
    let hash = auth::hash_token(&token);
    let proof_revocation = ClusterCacheRevocation::begin_digest(&state, &hash).await?;
    if !state
        .store
        .delete_token_with_cache_admin_claim(&hash, proof_revocation.mutation_claim())
        .await?
    {
        return Err(ApiError::ServiceUnavailable(
            "logout lost its cache-revocation exclusion; retry the request".into(),
        ));
    }
    proof_revocation.finish(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// GET /api/v1/me
pub async fn me(AuthUser(user): AuthUser) -> Json<UserDto> {
    Json(user.into())
}

pub(crate) fn validate_password_size(password: &str) -> Result<(), ApiError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::BadRequest(format!(
            "password must be at most {MAX_PASSWORD_BYTES} bytes"
        )));
    }
    Ok(())
}

pub(crate) fn validate_new_password(password: &str) -> Result<(), ApiError> {
    validate_password_size(password)?;
    if password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn hash_password_bounded(password: String) -> Result<String, ApiError> {
    run_password_work(move || auth::hash_password(&password))
        .await?
        .map_err(|error| ApiError::Internal(error.to_string()))
}

/// Admit both queued and active Argon2 work before leaving the async executor.
/// The active permit moves into the blocking closure, so cancellation of the
/// HTTP request cannot advertise capacity while its abandoned hash still runs.
async fn run_password_work<T, Work>(work: Work) -> Result<T, ApiError>
where
    T: Send + 'static,
    Work: FnOnce() -> T + Send + 'static,
{
    let _waiting = Arc::clone(&PASSWORD_WAITERS)
        .try_acquire_owned()
        .map_err(|_| password_capacity_error())?;
    let active = tokio::time::timeout(
        PASSWORD_HASH_ADMISSION_WAIT,
        Arc::clone(&PASSWORD_ACTIVE).acquire_owned(),
    )
    .await
    .map_err(|_| password_capacity_error())?
    .map_err(|_| password_capacity_error())?;
    tokio::task::spawn_blocking(move || {
        let _active = active;
        work()
    })
    .await
    .map_err(|error| ApiError::Internal(format!("password worker failed: {error}")))
}

fn password_capacity_error() -> ApiError {
    ApiError::ServiceUnavailable(
        "password verification capacity is busy; retry in a few seconds".into(),
    )
}

/// A real Argon2 hash (of a throwaway password), computed once, used to spend
/// the same verification time on unknown usernames (mitigates user enumeration
/// via login timing). Verifying against it always fails for real passwords.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    auth::hash_password("plurx-timing-placeholder")
        .unwrap_or_else(|_| "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned())
});

#[cfg(test)]
mod tests {
    use super::{validate_new_password, validate_password_size, MAX_PASSWORD_BYTES};

    #[test]
    fn every_password_route_has_one_encoded_byte_ceiling() {
        assert!(validate_password_size(&"x".repeat(MAX_PASSWORD_BYTES)).is_ok());
        assert!(validate_password_size(&"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
        assert!(validate_new_password(&"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
        assert!(validate_password_size(&"é".repeat(MAX_PASSWORD_BYTES / 2 + 1)).is_err());
    }
}
