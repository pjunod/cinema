//! Playback handoff — a one-time code hands one title from a signed-in client
//! to another local app (the Linux player), never a bearer in a URL.
//!
//! The web app mints a code with its own bearer and puts only the code in the
//! `plurx-player://` URL. The player redeems it, unauthenticated, for a token
//! of its own audience (`TokenAudience::PlaybackHandoff`). That token is
//! accepted only by the handlers in the last section of this file, and only
//! for the handed-off file; every other user route authenticates the native
//! audience, which refuses it: the token lives in its own table, never in
//! `tokens`, so no native lookup (on this binary or an older one during a
//! rolling upgrade) can find it. It expires six hours after redemption and is
//! revoked with the token that minted it, so signing out of the web app stops
//! the player too. Modelled on `file_grants.rs`, which binds the same way.

mod context;
pub use context::{advance_next, context};

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{FromRequestParts, Path as AxPath, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::Json;
use plurx_core::auth;
use plurx_core::domain::User;
use plurx_core::store::{
    NewPlaybackHandoff, PlaybackHandoffRedeemOutcome, PlaybackHandoffRedemption,
    PlaybackHandoffTokenScope, TokenAudience, PLAYBACK_HANDOFF_CODE_TTL_SECS,
    PLAYBACK_HANDOFF_TOKEN_TTL_SECS,
};
use serde::{Deserialize, Serialize};

use super::dto::UserDto;
use super::error::ApiError;
use super::extract::{
    authenticate_token_digest, authenticate_user_token, token_from_parts, AuthUser, RawToken,
};
use super::network::RemoteAddress;
use crate::state::AppState;

/// The sessions-list label when the redeeming app names none.
const DEFAULT_DEVICE: &str = "Playback handoff";
/// The subtitle selection that means "off", as `/decision` defines it.
const SUBTITLE_OFF: i64 = -1;
/// Longest origin recorded from a mint request.
const MAX_ORIGIN_BYTES: usize = 256;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn handoff_gone() -> ApiError {
    ApiError::typed(
        StatusCode::GONE,
        "handoff_gone",
        "The playback handoff code was already used, has expired, or was revoked.",
    )
}

fn outside_scope() -> ApiError {
    ApiError::typed(
        StatusCode::FORBIDDEN,
        "handoff_scope",
        "This playback handoff token is limited to one title.",
    )
}

/// The browser origin the mint came from, when the request carried a plain
/// `scheme://host[:port]` `Origin` header. plurx has no configured canonical
/// origin, so this is the origin the minting web app was served from — the
/// same value it puts in the `plurx-player://` URL.
fn request_origin(headers: &HeaderMap) -> Option<String> {
    let origin = headers
        .get(axum::http::header::ORIGIN)?
        .to_str()
        .ok()?
        .trim();
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    let well_formed = origin.len() <= MAX_ORIGIN_BYTES
        && !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    well_formed.then(|| origin.to_owned())
}

#[derive(Deserialize)]
pub struct MintRequest {
    file_id: i64,
    start_ms: Option<i64>,
    audio: Option<i64>,
    subtitle: Option<i64>,
}

#[derive(Serialize)]
pub struct MintResponse {
    id: String,
    code: String,
    expires_in: i64,
}

/// POST /api/v1/playback-handoffs — mint a one-time code for one file.
pub async fn mint(
    AuthUser(user): AuthUser,
    RawToken(source_token): RawToken,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<MintRequest>,
) -> Result<(StatusCode, Json<MintResponse>), ApiError> {
    for (name, value) in [("start_ms", request.start_ms), ("audio", request.audio)] {
        if value.is_some_and(|value| value < 0) {
            return Err(ApiError::BadRequest(format!("{name} must not be negative")));
        }
    }
    // `-1` is "subtitles off", exactly as `/decision` reads it.
    if request.subtitle.is_some_and(|value| value < SUBTITLE_OFF) {
        return Err(ApiError::BadRequest(
            "subtitle must be a stream index or -1 (off)".into(),
        ));
    }
    state
        .store
        .get_file(request.file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let code =
        auth::generate_handoff_code().map_err(|error| ApiError::Internal(error.to_string()))?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_unix();
    let created = state
        .store
        .create_playback_handoff(NewPlaybackHandoff {
            id: id.clone(),
            code_hash: auth::hash_token(&code),
            user_id: user.id,
            file_id: request.file_id,
            source_token_hash: auth::hash_token(&source_token),
            start_ms: request.start_ms,
            audio: request.audio,
            subtitle: request.subtitle,
            origin: request_origin(&headers),
            created_at: now,
            expires_at: now + PLAYBACK_HANDOFF_CODE_TTL_SECS,
        })
        .await?;
    if !created {
        // The minting token was revoked between authentication and insert.
        return Err(ApiError::Unauthorized);
    }
    Ok((
        StatusCode::CREATED,
        Json(MintResponse {
            id,
            code,
            expires_in: PLAYBACK_HANDOFF_CODE_TTL_SECS,
        }),
    ))
}

#[derive(Deserialize)]
pub struct RedeemRequest {
    code: String,
    device: Option<String>,
}

#[derive(Serialize)]
pub struct RedeemResponse {
    token: String,
    handoff_id: String,
    file_id: i64,
    /// The file's item, for `POST /api/v1/items/{id}/progress`.
    item_id: i64,
    start_ms: Option<i64>,
    audio: Option<i64>,
    subtitle: Option<i64>,
    user: UserDto,
    origin: Option<String>,
    expires_at: i64,
}

/// POST /api/v1/playback-handoffs/redeem — no bearer; the code is the
/// credential, and it works once.
pub async fn redeem(
    State(state): State<AppState>,
    Json(request): Json<RedeemRequest>,
) -> Result<Json<RedeemResponse>, ApiError> {
    if !auth::is_handoff_code(&request.code) {
        return Err(ApiError::NotFound("handoff"));
    }
    let device = request
        .device
        .as_deref()
        .map(str::trim)
        .filter(|device| !device.is_empty())
        .unwrap_or(DEFAULT_DEVICE)
        .to_owned();
    super::auth::validate_device_label(Some(&device))?;
    let token = auth::generate_token().map_err(|error| ApiError::Internal(error.to_string()))?;
    let now = now_unix();
    let expires_at = now + PLAYBACK_HANDOFF_TOKEN_TTL_SECS;
    let outcome = state
        .store
        .redeem_playback_handoff(&PlaybackHandoffRedemption {
            code_hash: auth::hash_token(&request.code),
            token_hash: auth::hash_token(&token),
            device: Some(device),
            now,
            token_expires_at: expires_at,
        })
        .await?;
    let handoff = match outcome {
        PlaybackHandoffRedeemOutcome::Redeemed(handoff) => handoff,
        PlaybackHandoffRedeemOutcome::Gone => return Err(handoff_gone()),
        PlaybackHandoffRedeemOutcome::Unknown => return Err(ApiError::NotFound("handoff")),
    };
    let user = state
        .store
        .get_user(handoff.user_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let file = state
        .store
        .get_file(handoff.file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    Ok(Json(RedeemResponse {
        token,
        handoff_id: handoff.id,
        file_id: handoff.file_id,
        item_id: file.item_id,
        start_ms: handoff.start_ms,
        audio: handoff.audio,
        subtitle: handoff.subtitle,
        user: user.into(),
        origin: handoff.origin,
        expires_at,
    }))
}

#[derive(Serialize)]
pub struct StatusResponse {
    redeemed: bool,
}

/// GET /api/v1/playback-handoffs/{id} — whether the code was redeemed; only
/// the bearer that minted it may ask.
pub async fn status(
    AuthUser(user): AuthUser,
    RawToken(source_token): RawToken,
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Json<StatusResponse>, ApiError> {
    let handoff = state
        .store
        .playback_handoff_for_minter(&id, user.id, &auth::hash_token(&source_token))
        .await?
        .ok_or(ApiError::NotFound("handoff"))?;
    Ok(Json(StatusResponse {
        redeemed: handoff.redeemed_at.is_some(),
    }))
}

/// A live playback handoff token: the redeemed bearer, its user and scope.
pub struct HandoffToken {
    pub user: User,
    pub scope: PlaybackHandoffTokenScope,
    digest: String,
}

async fn authenticate_handoff_digest(
    state: &AppState,
    digest: String,
) -> Result<HandoffToken, ApiError> {
    let user =
        authenticate_token_digest(state, digest.clone(), TokenAudience::PlaybackHandoff).await?;
    let scope = state
        .store
        .playback_handoff_token_scope(&digest, now_unix())
        .await?
        .filter(|scope| scope.user_id == user.id)
        .ok_or(ApiError::Unauthorized)?;
    Ok(HandoffToken {
        user,
        scope,
        digest,
    })
}

impl FromRequestParts<AppState> for HandoffToken {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = token_from_parts(parts).ok_or(ApiError::Unauthorized)?;
        authenticate_handoff_digest(state, auth::hash_token(&token)).await
    }
}

/// DELETE /api/v1/playback-handoffs/self — the player revokes its own token
/// on exit. Only a handoff token is accepted here.
pub async fn revoke_self(
    handoff: HandoffToken,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    // A handoff token is never a cached admin proof and never a `tokens`
    // row, so its own table is the whole revocation.
    if state
        .store
        .revoke_playback_handoff_token(&handoff.digest)
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// A caller of a route a handoff token may use: a native user, or a handoff
/// token, which is then held to its one file.
pub struct PlaybackPrincipal {
    pub user: User,
    pub handoff: Option<PlaybackHandoffTokenScope>,
}

impl PlaybackPrincipal {
    fn require_file(&self, file_id: i64) -> Result<(), ApiError> {
        match &self.handoff {
            Some(scope) if scope.file_id != file_id => Err(outside_scope()),
            _ => Ok(()),
        }
    }

    async fn require_item(&self, state: &AppState, item_id: i64) -> Result<(), ApiError> {
        let Some(scope) = &self.handoff else {
            return Ok(());
        };
        let file = state.store.get_file(scope.file_id).await?;
        if file.is_some_and(|file| file.item_id == item_id) {
            Ok(())
        } else {
            Err(outside_scope())
        }
    }
}

impl FromRequestParts<AppState> for PlaybackPrincipal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = token_from_parts(parts).ok_or(ApiError::Unauthorized)?;
        match authenticate_user_token(state, &token).await {
            Ok(user) => Ok(Self {
                user,
                handoff: None,
            }),
            // Not a native bearer: it may still be a live handoff token.
            Err(ApiError::Unauthorized) => {
                let handoff = authenticate_handoff_digest(state, auth::hash_token(&token)).await?;
                Ok(Self {
                    user: handoff.user,
                    handoff: Some(handoff.scope),
                })
            }
            Err(error) => Err(error),
        }
    }
}

// ---------------------------------------------------------------------------
// The routes a handoff token may use. Each one holds a handoff token to its
// file and then runs the ordinary handler unchanged. `/hls/{session}/…`
// (control, status, playlists, teardown) needs nothing here: the session id
// is that route family's capability and no bearer is read there (API.md §2.4).

/// GET /api/v1/files/{id}/decision
pub async fn scoped_decision(
    principal: PlaybackPrincipal,
    state: State<AppState>,
    AxPath(id): AxPath<i64>,
    query: Query<super::stream::Caps>,
    headers: HeaderMap,
    remote: RemoteAddress,
) -> Result<Json<super::stream::DecisionResponse>, ApiError> {
    principal.require_file(id)?;
    super::stream::decision(
        AuthUser(principal.user),
        state,
        AxPath(id),
        query,
        headers,
        remote,
    )
    .await
}

/// POST /api/v1/files/{id}/decision
pub async fn scoped_decision_post(
    principal: PlaybackPrincipal,
    state: State<AppState>,
    AxPath(id): AxPath<i64>,
    query: Query<super::stream::Caps>,
    headers: HeaderMap,
    remote: RemoteAddress,
    body: Json<super::stream::DecisionBody>,
) -> Result<Json<super::stream::DecisionResponse>, ApiError> {
    principal.require_file(id)?;
    super::stream::decision_post(
        AuthUser(principal.user),
        state,
        AxPath(id),
        query,
        headers,
        remote,
        body,
    )
    .await
}

/// GET /api/v1/files/{id}/direct — the original bytes, for direct play.
pub async fn scoped_direct(
    principal: PlaybackPrincipal,
    state: State<AppState>,
    AxPath(id): AxPath<i64>,
    query: Query<super::stream::DirectQuery>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    principal.require_file(id)?;
    super::stream::direct(
        AuthUser(principal.user),
        state,
        AxPath(id),
        query,
        method,
        headers,
    )
    .await
}

/// GET /api/v1/files/{id}/subs/{subtitle} — a text track as WebVTT.
pub async fn scoped_subtitles_vtt(
    principal: PlaybackPrincipal,
    state: State<AppState>,
    AxPath((id, subtitle)): AxPath<(i64, String)>,
    query: Query<super::stream::SubtitleVttQuery>,
) -> Result<Response, ApiError> {
    principal.require_file(id)?;
    super::stream::subtitles_vtt(
        AuthUser(principal.user),
        state,
        AxPath((id, subtitle)),
        query,
    )
    .await
}

/// POST /api/v1/files/{id}/hls/sessions
pub async fn scoped_hls_create(
    principal: PlaybackPrincipal,
    state: State<AppState>,
    AxPath(id): AxPath<i64>,
    headers: HeaderMap,
    remote: RemoteAddress,
    body: Json<super::hls::CreateSession>,
) -> Result<Json<super::hls::StartResponse>, ApiError> {
    principal.require_file(id)?;
    super::hls::create(
        AuthUser(principal.user),
        state,
        AxPath(id),
        headers,
        remote,
        body,
    )
    .await
}

/// POST /api/v1/items/{id}/progress — the handed-off file's item only.
pub async fn scoped_progress(
    principal: PlaybackPrincipal,
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
    body: Json<super::watch::ProgressRequest>,
) -> Result<Json<super::dto::WatchDto>, ApiError> {
    principal.require_item(&state, id).await?;
    super::watch::progress(AuthUser(principal.user), State(state), AxPath(id), body).await
}

/// POST /api/v1/client-log — a handoff token may report for its user.
pub async fn scoped_client_log(
    principal: PlaybackPrincipal,
    state: State<AppState>,
    headers: HeaderMap,
    remote: RemoteAddress,
    body: Json<super::system::ClientLog>,
) -> Response {
    super::system::client_log(AuthUser(principal.user), state, headers, remote, body).await
}

/// Prune dead handoff tokens and stale codes hourly. A dead token already
/// authenticates nobody; pruning only removes it from the sessions list.
pub async fn prune_loop(state: AppState, shutdown: tokio_util::sync::CancellationToken) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = interval.tick() => {
                let now = now_unix();
                if let Err(error) = state
                    .store
                    .prune_playback_handoffs(now, now.saturating_sub(24 * 60 * 60))
                    .await
                {
                    tracing::warn!(%error, "playback handoff pruning failed");
                }
            }
        }
    }
}
