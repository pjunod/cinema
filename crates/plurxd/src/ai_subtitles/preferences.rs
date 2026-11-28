use crate::{
    http::{error::ApiError, extract::AuthUser},
    state::AppState,
};
use axum::{extract::State, Json};
use plurx_core::{
    domain::User,
    tracks::{LangPrefs, SubMode},
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    language: String,
    mode: Mode,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Auto,
    Always,
    Off,
}

impl Preferences {
    pub(crate) fn apply(self, mut defaults: LangPrefs) -> LangPrefs {
        defaults.sub_lang = self.language;
        defaults.sub_mode = match self.mode {
            Mode::Auto => SubMode::Auto,
            Mode::Always => SubMode::Always,
            Mode::Off => SubMode::Off,
        };
        defaults
    }

    fn valid(&self) -> bool {
        let mut parts = self.language.split('-');
        let Some(language) = parts.next() else {
            return false;
        };
        (2..=3).contains(&language.len())
            && language.bytes().all(|byte| byte.is_ascii_alphabetic())
            && self.language.len() <= 16
            && parts.all(|part| {
                (2..=8).contains(&part.len())
                    && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
    }
}

pub(crate) fn key(user_id: i64) -> String {
    format!("playback.subtitle_preferences.{user_id}")
}

pub(crate) fn decode(raw: &str) -> Option<Preferences> {
    serde_json::from_str::<Preferences>(raw)
        .ok()
        .filter(Preferences::valid)
}

pub(crate) async fn playback(state: &AppState, user: Option<&User>) -> LangPrefs {
    let defaults = state.transcode.lang_prefs().await;
    if let Some(user) = user {
        let key = key(user.id);
        if let Ok(settings) = state.catalogue.display_settings(&[&key]).await {
            if let Some(personal) = settings.get(&key).and_then(|raw| decode(raw)) {
                return personal.apply(defaults);
            }
        }
    }
    defaults
}

pub(crate) async fn get(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Preferences>, ApiError> {
    if let Some(personal) = state
        .store
        .get_setting(&key(user.id))
        .await?
        .as_deref()
        .and_then(decode)
    {
        return Ok(Json(personal));
    }
    let defaults = state.transcode.lang_prefs().await;
    Ok(Json(Preferences {
        language: defaults.sub_lang,
        mode: match defaults.sub_mode {
            SubMode::Auto => Mode::Auto,
            SubMode::Always => Mode::Always,
            SubMode::Off => Mode::Off,
        },
    }))
}

pub(crate) async fn put(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(input): Json<Preferences>,
) -> Result<Json<Preferences>, ApiError> {
    if !input.valid() {
        return Err(ApiError::BadRequest("Invalid subtitle language".into()));
    }
    let encoded = serde_json::to_string(&input)
        .map_err(|_| ApiError::Internal("Could not encode subtitle preferences".into()))?;
    state.store.put_setting(&key(user.id), &encoded).await?;
    Ok(Json(input))
}
