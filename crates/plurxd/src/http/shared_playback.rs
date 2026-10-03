//! Private Source decision ingress. Reads do not allocate a media session.
use super::{error::ApiError, hls::SourcePlaybackTarget, shared_library, stream};
use crate::state::AppState;
use axum::{
    body::{to_bytes, Body},
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
    Router,
};
use plurx_core::{
    sharing::SourceId, sharing_catalogue_details::CatalogueRevisionKey,
    store::sharing_catalogue_details::SourceDetailsRead,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};
static DECISIONS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(16)));

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceDecisionRequest {
    pub reference: SourcePlaybackTarget,
    pub caps: plurx_core::playback::DeviceCaps,
    pub audio: Option<i64>,
    pub subtitle: Option<i64>,
    pub audio_offset_ms: Option<i64>,
    pub force: Option<String>,
}
fn refused() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_decision_unavailable",
        "Shared decision authority is unavailable",
    )
}
fn invalid() -> ApiError {
    ApiError::typed(
        StatusCode::BAD_REQUEST,
        "sharing_invalid_request",
        "Invalid shared decision request",
    )
}
pub(crate) fn peer_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/sharing/v1/items/{item}/files/{file}/decision",
            post(decision),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            shared_library::source_content_guard,
        ))
}
async fn decision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((item, file)): Path<(String, String)>,
    body: Body,
) -> Result<Response, ApiError> {
    let _permit = DECISIONS.clone().try_acquire_owned().map_err(|_| {
        ApiError::typed(
            StatusCode::TOO_MANY_REQUESTS,
            "sharing_decision_capacity",
            "Shared decision capacity is busy",
        )
    })?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let bytes = to_bytes(body, 128 * 1024).await.map_err(|_| invalid())?;
        let input: SourceDecisionRequest = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if SourceId::parse(&item).map_err(|_| invalid())? != input.reference.item_id
            || SourceId::parse(&file).map_err(|_| invalid())? != input.reference.file_id
        {
            return Err(invalid());
        }
        source_decision(&state, &headers, input).await
    })
    .await
    .map_err(|_| refused())?
}

pub(super) async fn source_decision(
    state: &AppState,
    headers: &HeaderMap,
    input: SourceDecisionRequest,
) -> Result<Response, ApiError> {
    let target = input.reference;
    if target.server_id.is_nil()
        || target.catalogue_epoch.is_nil()
        || input.caps.v != plurx_core::playback::DeviceCaps::VERSION
        || input.caps.is_empty()
        || !crate::sharing::enabled(state.store.as_ref()).await?
    {
        return Err(refused());
    }
    stream::validate_device_caps(&input.caps)?;
    let (hash, grant) = shared_library::authority(state, headers).await?;
    let SourceDetailsRead::Authorized(witness) = state
        .store
        .source_item_file_witness(&hash, grant, target.item_id.clone(), target.file_id.clone())
        .await?
    else {
        return Err(refused());
    };
    if !witness.matches_source_file(
        target.server_id,
        target.catalogue_epoch,
        &target.library_id,
        &target.item_id,
        &target.file_id,
    ) {
        return Err(refused());
    }
    let envelope = state
        .store
        .source_catalogue_revision_key(target.server_id, target.catalogue_epoch)
        .await?
        .ok_or_else(refused)?;
    let key = CatalogueRevisionKey::open(
        &state.sharing.key,
        plurx_core::sharing::SharingIdentity {
            server_id: target.server_id,
            catalogue_epoch: target.catalogue_epoch,
            created_at_ms: 0,
        },
        &envelope,
    )?;
    if key.file_revision(&witness)? != target.revision {
        return Err(refused());
    }
    // Only a current grant-authorized complete witness admits the ordinary
    // planning store; no B file identifier is ever treated as a Local ID.
    let snapshot = state
        .store
        .playback_planning_snapshot(
            target
                .file_id
                .as_str()
                .parse::<i64>()
                .map_err(|_| invalid())?,
            &crate::transcode::QUALITY_PLANNING_KEYS,
        )
        .await?
        .ok_or_else(refused)?;
    let q = stream::Caps {
        caps_v2: Some(input.caps),
        audio: input.audio,
        subtitle: input.subtitle,
        audio_offset_ms: input.audio_offset_ms,
        force: input.force,
        ..Default::default()
    };
    let decision = stream::decision_for_source_file(state, snapshot.file, q).await?;
    let SourceDetailsRead::Authorized(current) = state
        .store
        .source_item_file_witness(&hash, grant, target.item_id.clone(), target.file_id.clone())
        .await?
    else {
        return Err(refused());
    };
    if !current.matches_source_file(
        target.server_id,
        target.catalogue_epoch,
        &target.library_id,
        &target.item_id,
        &target.file_id,
    ) || key.file_revision(&current)? != target.revision
        || !crate::sharing::enabled(state.store.as_ref()).await?
        || shared_library::authority(state, headers).await? != (hash, grant)
    {
        return Err(refused());
    }
    shared_library::source_file_json(
        grant,
        &target,
        json!({"protocol":1,"reference":target,"decision":decision}),
    )
}
