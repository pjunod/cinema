//! AI subtitles: one settings document and the authorization every start
//! route shares. The queue SQL enforces the same `who` rule at claim time.
pub(crate) mod readiness;
pub(crate) mod routes;
pub(crate) mod selection;
pub(crate) mod settings;

use crate::{
    http::{
        error::ApiError,
        extract::{AdminUser, AuthUser},
    },
    state::AppState,
};
use axum::{extract::State, http::StatusCode, Json};
use plurx_core::{domain::User, store::Store};

/// The speech models the audio route needs at a quality tier; every node
/// that can run them installs them by itself
/// ([`crate::subtitle_models::self_install`]).
pub(crate) fn required_speech_models(quality: settings::Quality) -> &'static [&'static str] {
    match quality {
        settings::Quality::Fast | settings::Quality::Balanced => &["small"],
        settings::Quality::Best => &["small", "large-v3-q5_0"],
    }
}

/// Active AI subtitle operations and jobs a non-admin may hold at once.
pub(crate) const MAX_ACTIVE_PER_USER: i64 = 2;

/// Whether this user may start or steer AI subtitle work: an admin, or
/// anyone while Settings says everyone.
pub(crate) async fn allowed(store: &dyn Store, user: &User) -> Result<bool, ApiError> {
    Ok(user.is_admin || settings::read_store(store).await?.settings.who == settings::Who::Everyone)
}

/// Refuse a start unless the user is an admin or Settings allows everyone.
pub(crate) async fn may_start(state: &AppState, user: &User) -> Result<(), ApiError> {
    if allowed(state.store.as_ref(), user).await? {
        return Ok(());
    }
    Err(ApiError::typed(
        StatusCode::FORBIDDEN,
        "ai_subtitles_admins_only",
        "An administrator has limited AI subtitles to admins",
    ))
}

/// [`may_start`] plus the per-user bound for a request that adds new work.
/// `joining` names a progressive operation the request would join rather than
/// start, which is not counted against the user.
pub(crate) async fn may_start_new(
    state: &AppState,
    user: &User,
    joining: Option<&str>,
) -> Result<(), ApiError> {
    may_start(state, user).await?;
    if user.is_admin {
        return Ok(());
    }
    let active = state
        .store
        .ai_subtitle_active_work(user.id, joining, crate::state::clock_ms())
        .await?;
    if active >= MAX_ACTIVE_PER_USER {
        return Err(ApiError::typed(
            StatusCode::TOO_MANY_REQUESTS,
            "ai_subtitles_busy",
            format!(
                "You already have {MAX_ACTIVE_PER_USER} AI subtitle jobs in progress; \
                 wait for one to finish before starting another"
            ),
        ));
    }
    Ok(())
}

/// The settings document plus each node's `readiness` for it.
pub(crate) async fn get_settings(
    _user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let snapshot = settings::read(&state).await?;
    let mut body = serde_json::to_value(&snapshot)
        .map_err(|_| ApiError::Internal("Could not encode AI subtitles settings".into()))?;
    body["readiness"] = readiness::cluster(&state).await?;
    Ok(Json(body))
}

pub(crate) async fn put_settings(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(input): Json<settings::UpdateRequest>,
) -> Result<Json<settings::Snapshot>, ApiError> {
    Ok(Json(settings::save(&state, input).await?))
}
