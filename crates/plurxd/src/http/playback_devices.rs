//! Playback devices and what they decode (Curator ADR 0026 §5).
//!
//! Every native client sends a capability document with its playback
//! decision; this keeps the last one per signed-in device, so the title page
//! can say how a version plays on each of the account's devices and Curator
//! can derive a profile's HDR order from what a device actually decodes.
//!
//! `GET /api/v1/playback/devices` answers a `playback:read` key (every
//! account's devices, for Curator) or a signed-in user (their own).

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::Json;
use plurx_core::auth;
use plurx_core::domain::scopes;
use plurx_core::playback::hdr_devices::{
    device_caps_key, device_hdr, DeviceCapsRecord, DeviceHdr, DEVICE_CAPS_KEY_PREFIX,
};
use plurx_core::playback::DeviceCaps;
use serde::Serialize;

use super::error::ApiError;
use super::extract::{token_from_parts, AuthUser, ScopedKey};
use crate::state::AppState;

/// The bearer login token of a request, when it carries one. API keys are
/// not devices, so they are not returned.
fn login_token(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = value.strip_prefix("Bearer ")?.trim();
    (!token.is_empty() && !auth::is_api_key(token)).then(|| token.to_owned())
}

/// Remember the capability document a signed-in device just sent.
///
/// Best effort, never in the way of playback: a failure is logged and the
/// decision goes on. The record is written only when the device's abilities
/// changed, judged against this node's own replica, so an ordinary Play is
/// not a replicated write.
pub(crate) async fn record_device_caps(
    state: &AppState,
    user_id: i64,
    headers: &HeaderMap,
    caps: &DeviceCaps,
) {
    let Some(token) = login_token(headers) else {
        return;
    };
    let hash = auth::hash_token(&token);
    let key = device_caps_key(&hash);
    let record = DeviceCapsRecord::new(user_id, &hash, caps.clone(), super::users::unix_now());
    if let Ok(Some(prior)) = state.store.local_setting_hint(&key).await {
        if serde_json::from_str::<DeviceCapsRecord>(&prior)
            .is_ok_and(|prior| prior.same_abilities(&record))
        {
            return;
        }
    }
    let Ok(json) = serde_json::to_string(&record) else {
        return;
    };
    if let Err(error) = state.store.put_setting(&key, &json).await {
        tracing::warn!(%error, "could not record playback device capabilities");
        return;
    }
    // The device list changed for this account: drop records of its devices
    // that have since been signed out, so revoked tokens leave nothing behind.
    if let Err(error) = prune_signed_out(state, user_id).await {
        tracing::debug!(?error, "could not prune signed-out playback devices");
    }
}

/// Every stored capability record, by settings key.
async fn records(state: &AppState) -> Result<Vec<(String, DeviceCapsRecord)>, ApiError> {
    let snapshot = state.store.settings_snapshot().await?;
    Ok(snapshot
        .into_iter()
        .filter(|(key, _)| key.starts_with(DEVICE_CAPS_KEY_PREFIX))
        .filter_map(|(key, value)| Some((key, serde_json::from_str(&value).ok()?)))
        .collect())
}

async fn prune_signed_out(state: &AppState, user_id: i64) -> Result<(), ApiError> {
    let live: Vec<String> = state
        .store
        .list_tokens_for_user(user_id)
        .await?
        .into_iter()
        .map(|t| t.token_hash_prefix)
        .collect();
    for (key, record) in records(state).await? {
        if record.user_id == user_id && !live.contains(&record.token_prefix) {
            state.store.delete_setting(&key).await?;
        }
    }
    Ok(())
}

/// One device in `GET /api/v1/playback/devices`.
#[derive(Debug, Serialize)]
pub struct PlaybackDeviceDto {
    /// The device's token prefix, as the account's device list names it.
    pub id: String,
    pub user: String,
    /// The label the device signed in with ("Living room", "iPhone").
    pub device: Option<String>,
    /// The client family that sent the capabilities (`apple`, `android`,
    /// `web`, `linux-player`).
    pub client: Option<String>,
    pub last_seen_at: i64,
    /// When these capabilities were last reported.
    pub recorded_at: i64,
    pub hdr: DeviceHdr,
}

/// The signed-in devices with recorded capabilities, joined to the live
/// token list so a signed-out device is never reported.
pub(crate) async fn devices_for(
    state: &AppState,
    only_user: Option<i64>,
) -> Result<Vec<(PlaybackDeviceDto, DeviceCapsRecord)>, ApiError> {
    let mut out = Vec::new();
    let mut by_user: std::collections::BTreeMap<i64, Vec<DeviceCapsRecord>> = Default::default();
    for (_, record) in records(state).await? {
        if only_user.is_none_or(|u| u == record.user_id) {
            by_user.entry(record.user_id).or_default().push(record);
        }
    }
    for (user_id, records) in by_user {
        let Some(user) = state.store.get_user(user_id).await? else {
            continue;
        };
        let tokens = state.store.list_tokens_for_user(user_id).await?;
        for record in records {
            let Some(token) = tokens
                .iter()
                .find(|t| t.token_hash_prefix == record.token_prefix)
            else {
                continue;
            };
            let dto = PlaybackDeviceDto {
                id: record.token_prefix.clone(),
                user: user.username.clone(),
                device: token.device.clone(),
                client: record.caps.client.as_ref().map(|c| c.kind.clone()),
                last_seen_at: token.last_seen_at,
                recorded_at: record.recorded_at,
                hdr: device_hdr(&record.caps),
            };
            out.push((dto, record));
        }
    }
    out.sort_by_key(|d| std::cmp::Reverse(d.0.last_seen_at));
    Ok(out)
}

/// GET /api/v1/playback/devices
pub async fn list(
    State(state): State<AppState>,
    mut parts: Parts,
) -> Result<Json<serde_json::Value>, ApiError> {
    let only_user = match token_from_parts(&parts) {
        Some(secret) if auth::is_api_key(&secret) => {
            ScopedKey::require(&mut parts, &state, scopes::PLAYBACK_READ).await?;
            None
        }
        _ => {
            let AuthUser(user) = AuthUser::from_request_parts(&mut parts, &state).await?;
            Some(user.id)
        }
    };
    let devices: Vec<PlaybackDeviceDto> = devices_for(&state, only_user)
        .await?
        .into_iter()
        .map(|(d, _)| d)
        .collect();
    Ok(Json(serde_json::json!({ "devices": devices })))
}
