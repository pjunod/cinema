//! Playback devices and what they decode (Curator ADR 0026 §5).
//!
//! Every native client sends a capability document with its playback
//! decision; this keeps the last one per signed-in device, so the title page
//! can say how a version plays on each of the account's devices and Curator
//! can derive a profile's HDR order from what a device actually decodes.
//!
//! The record belongs to the login token it arrived on
//! (`plurx_core::store::playback_device_caps`): it is removed with the token
//! on every path that removes one, and only live, unexpired sign-ins are
//! listed. Reads are keyed and bounded; nothing here reads the settings.
//!
//! `GET /api/v1/playback/devices` answers a `playback:read` key (every
//! account's devices, for Curator) or a signed-in user (their own).

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::Json;
use plurx_core::auth;
use plurx_core::domain::scopes;
use plurx_core::playback::hdr_devices::{device_hdr, stored_caps_json, DeviceHdr};
use plurx_core::playback::DeviceCaps;
use plurx_core::store::PlaybackDeviceCaps;
use serde::Serialize;

use super::error::ApiError;
use super::extract::{token_from_parts, AuthUser, ScopedKey};
use crate::state::AppState;

/// Remember the capability document a signed-in device's decision was just
/// made from. `token_hash` is the digest of the native login token that
/// authenticated it.
///
/// Called only after the decision succeeded, so a document the server
/// refused, or one sent for a file that does not exist, is never kept. Best
/// effort and never in the way of playback: a failure is logged and the
/// answer goes on. The volatile fields are stripped first
/// ([`plurx_core::playback::hdr_devices::stored_caps`]) and the store writes
/// only a changed document, so an ordinary Play is not a replicated write.
pub(crate) async fn record_device_caps(state: &AppState, token_hash: &str, caps: &DeviceCaps) {
    let Some(json) = stored_caps_json(caps) else {
        tracing::debug!("playback device capabilities too large to keep");
        return;
    };
    if let Err(error) = state
        .store
        .put_device_caps(token_hash, &json, super::users::unix_now())
        .await
    {
        tracing::warn!(%error, "could not record playback device capabilities");
    }
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
    /// When these capabilities last changed.
    pub recorded_at: i64,
    pub hdr: DeviceHdr,
}

/// The live signed-in devices with recorded capabilities, most recently used
/// first: one account's, or every account's.
pub(crate) async fn devices_for(
    state: &AppState,
    only_user: Option<i64>,
) -> Result<Vec<(PlaybackDeviceDto, DeviceCaps)>, ApiError> {
    let records = match only_user {
        Some(user_id) => state.store.device_caps_for_user(user_id).await?,
        None => state.store.device_caps_for_all_users().await?,
    };
    Ok(records.into_iter().filter_map(device).collect())
}

fn device(record: PlaybackDeviceCaps) -> Option<(PlaybackDeviceDto, DeviceCaps)> {
    let caps: DeviceCaps = serde_json::from_str(&record.caps).ok()?;
    let dto = PlaybackDeviceDto {
        id: record.token_prefix,
        user: record.username,
        device: record.device,
        client: caps.client.as_ref().map(|c| c.kind.clone()),
        last_seen_at: record.last_seen_at,
        recorded_at: record.updated_at,
        hdr: device_hdr(&caps),
    };
    Some((dto, caps))
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
