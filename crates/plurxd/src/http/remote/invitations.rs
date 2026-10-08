//! Native invitation installation lifecycle. Background verdicts never touch activity.
mod wire;
use super::{bearer, error_parts, fail, now_seconds, proof, secret};
use crate::{
    http::{error::ApiError, extract::authenticate_user_token},
    state::AppState,
};
use axum::{
    body::Body,
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
};
use plurx_core::{
    auth,
    error::StoreError,
    store::invitations::{InvitationPhone, NewInvitationPhone},
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;
use wire::*;
pub(crate) struct Hub {
    requests: Arc<Semaphore>,
    pub(crate) changed: Notify,
}
impl Default for Hub {
    fn default() -> Self {
        Self {
            requests: Arc::new(Semaphore::new(32)),
            changed: Notify::new(),
        }
    }
}
pub(crate) fn path(path: &str) -> bool {
    path.strip_prefix("/api/remote/v1/")
        .is_some_and(|p| p == "phones" || p.starts_with("phones/") || p.starts_with("invitations/"))
}
pub(crate) fn eligible(method: &Method, path: &str) -> bool {
    let Some(p) = path.strip_prefix("/api/remote/v1/") else {
        return false;
    };
    if *method == Method::POST && matches!(p, "phones" | "phones/list") {
        return true;
    }
    let parts = p.split('/').collect::<Vec<_>>();
    if parts.len() < 2 || parts[0] != "phones" || !valid_id(parts[1]) {
        return false;
    }
    (*method == Method::DELETE && parts.len() == 2)
        || (*method == Method::POST
            && parts.len() == 3
            && matches!(parts[2], "availability" | "rebind"))
}
fn valid_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id)
}
fn decode<T: DeserializeOwned + Envelope>(body: &[u8]) -> Result<T, ApiError> {
    let value: T = serde_json::from_slice(body).map_err(|_| fail(400, "invalid"))?;
    let Version::V1 = value.version();
    Ok(value)
}
fn store_error(error: StoreError) -> ApiError {
    if error.to_string().contains("migration_remediation") {
        fail(503, "migration_remediation")
    } else {
        fail(503, "unavailable")
    }
}
fn phone(p: InvitationPhone) -> Value {
    json!({"installation_id":p.id,"name":p.name,"platform":p.platform,"phone_generation":p.generation,"created_at":p.created_at,"permission_granted":p.permission_granted,"resident_active":p.resident_active})
}
pub(crate) fn error(error: ApiError) -> Response {
    let (status, mut value) = error_parts(error);
    value["version"] = json!("cinema.invitation.v1");
    (status, [("cache-control", "no-store")], axum::Json(value)).into_response()
}
fn response(status: u16, value: Value) -> Response {
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return error(fail(503, "unavailable"));
    };
    if bytes.len() > 64 * 1024 {
        return error(fail(503, "unavailable"));
    }
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| error(fail(503, "unavailable")))
}
pub(crate) async fn public(
    state: &AppState,
    uri: &Uri,
    method: Method,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    match perform(state, uri, method, headers, body).await {
        Ok((status, value)) => response(status, value),
        Err(e) => error(e),
    }
}
async fn perform(
    state: &AppState,
    uri: &Uri,
    method: Method,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(u16, Value), ApiError> {
    let _permit = state
        .invitations
        .requests
        .clone()
        .try_acquire_owned()
        .map_err(|_| fail(429, "busy"))?;
    if body.len() > 64 * 1024 {
        return Err(fail(413, "invalid"));
    }
    let token = bearer(headers, uri)?;
    let digest = auth::hash_token(&token);
    let p = uri
        .path()
        .strip_prefix("/api/remote/v1/")
        .ok_or_else(|| fail(404, "not_found"))?;
    if !eligible(&method, uri.path()) {
        return Err(fail(404, "not_found"));
    }
    // Availability is OS/background reconciliation, not user interaction.
    let user = if p.ends_with("/availability") {
        state
            .store
            .invitation_login(&digest, now_seconds()?)
            .await
            .map_err(store_error)?
            .ok_or_else(|| fail(401, "unauthorized"))?
            .user_id
    } else {
        authenticate_user_token(state, &token).await?.id
    };
    let value = if p == "phones" {
        let r: Register = decode(body)?;
        if !valid_id(&r.installation_id)
            || r.name.is_empty()
            || r.name.len() > 80
            || r.name.chars().any(char::is_control)
            || !matches!(r.platform.as_str(), "apple" | "android")
        {
            return Err(fail(400, "invalid"));
        }
        if state
            .store
            .invitation_phone(&r.installation_id, user)
            .await
            .map_err(store_error)?
            .is_some()
        {
            let hash = proof(headers, "x-cinema-phone-secret")?
                .ok_or_else(|| fail(401, "unauthorized"))?;
            let existing = state
                .store
                .invitation_phone_authority(&r.installation_id, user, &hash)
                .await
                .map_err(store_error)?
                .ok_or_else(|| fail(401, "unauthorized"))?;
            json!({"version":"cinema.invitation.v1","phone":phone(existing),"phone_secret":null})
        } else {
            let (raw, hash) = secret()?;
            let item = InvitationPhone {
                id: r.installation_id.clone(),
                user_id: user,
                name: r.name,
                platform: r.platform,
                generation: 1,
                created_at: now_seconds()?,
                permission_granted: false,
                resident_active: false,
            };
            if !state
                .store
                .create_invitation_phone(NewInvitationPhone {
                    phone: item.clone(),
                    secret_hash: hash,
                    token_digest: digest.clone(),
                })
                .await
                .map_err(store_error)?
            {
                return Err(fail(429, "retention_limit"));
            }
            json!({"version":"cinema.invitation.v1","phone":phone(item),"phone_secret":raw})
        }
    } else if p == "phones/list" {
        let r: PhoneList = decode(body)?;
        if !(1..=20).contains(&r.limit) || r.after_id.as_ref().is_some_and(|id| !valid_id(id)) {
            return Err(fail(400, "invalid"));
        }
        let mut rows = state
            .store
            .invitation_phones(user, r.after_id.as_deref().unwrap_or(""))
            .await
            .map_err(store_error)?;
        let more = rows.len() > usize::from(r.limit);
        rows.truncate(usize::from(r.limit));
        let cursor = if more {
            rows.last().map(|p| p.id.clone())
        } else {
            None
        };
        json!({"version":"cinema.invitation.v1","phones":rows.into_iter().map(phone).collect::<Vec<_>>(),"next_cursor":cursor})
    } else {
        let parts = p.split('/').collect::<Vec<_>>();
        let id = parts[1];
        if method == Method::DELETE {
            if !body.is_empty() {
                return Err(fail(400, "invalid"));
            }
            state
                .store
                .revoke_invitation_phone(id, user)
                .await
                .map_err(store_error)?;
            state.invitations.changed.notify_waiters();
            json!({"version":"cinema.invitation.v1"})
        } else {
            let hash = proof(headers, "x-cinema-phone-secret")?
                .ok_or_else(|| fail(401, "unauthorized"))?;
            let existing = state
                .store
                .invitation_phone_authority(id, user, &hash)
                .await
                .map_err(store_error)?
                .ok_or_else(|| fail(401, "unauthorized"))?;
            if parts[2] == "availability" {
                let r: Availability = decode(body)?;
                if r.expected_phone_generation != existing.generation {
                    return Err(fail(409, "stale_generation"));
                }
                if (r.permission_granted || r.resident_active)
                    && state
                        .store
                        .invitation_phone_binding(id, user, &hash)
                        .await
                        .map_err(store_error)?
                        .as_deref()
                        != Some(&digest)
                {
                    return Err(fail(409, "login_changed"));
                }
                if r.resident_active && (existing.platform != "android" || !r.permission_granted) {
                    return Err(fail(400, "invalid"));
                }
                if !state
                    .store
                    .set_invitation_availability(
                        id,
                        user,
                        &hash,
                        r.expected_phone_generation,
                        r.permission_granted,
                        r.resident_active,
                    )
                    .await
                    .map_err(store_error)?
                {
                    return Err(fail(409, "stale_generation"));
                }
            } else {
                let r: Rebind = decode(body)?;
                if !state
                    .store
                    .rebind_invitation_phone(id, user, &hash, r.expected_phone_generation, &digest)
                    .await
                    .map_err(store_error)?
                {
                    return Err(fail(409, "stale_generation"));
                }
            }
            state.invitations.changed.notify_waiters();
            let item = state
                .store
                .invitation_phone_authority(id, user, &hash)
                .await
                .map_err(store_error)?
                .ok_or_else(|| fail(401, "unauthorized"))?;
            json!({"version":"cinema.invitation.v1","phone":phone(item)})
        }
    };
    if state
        .store
        .invitation_login(&digest, now_seconds()?)
        .await
        .map_err(store_error)?
        .is_none_or(|u| u.user_id != user)
    {
        return Err(fail(401, "unauthorized"));
    }
    Ok((200, value))
}

#[cfg(test)]
mod tests;
