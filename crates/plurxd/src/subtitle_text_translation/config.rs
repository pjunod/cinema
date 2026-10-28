//! Saved text-translation preference; readiness never changes operator intent.
use crate::{http::error::ApiError, state::AppState};
use serde::{Deserialize, Serialize};

pub(crate) const KEY: &str = "subtitles.text_translation.config.v1";
const GENERATION: &str = "subtitles.text_translation.revision.v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub version: u8,
    pub revision: String,
    pub enabled: bool,
    pub model_id: String,
    pub target_language: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateRequest {
    pub version: u8,
    pub expected_revision: String,
    pub enabled: bool,
    pub model_id: String,
}
pub(crate) async fn read(state: &AppState) -> Result<Snapshot, ApiError> {
    let (raw, generation) = state.store.get_setting_pair(KEY, GENERATION).await?;
    let revision = generation.unwrap_or_else(|| "0".into());
    revision_number(&revision)?;
    let saved = match raw {
        Some(raw) => serde_json::from_str::<Snapshot>(&raw)
            .map_err(|_| ApiError::Conflict("Text translation configuration is invalid".into()))?,
        None => Snapshot {
            version: 1,
            revision: revision.clone(),
            enabled: false,
            model_id: super::MODEL_ID.into(),
            target_language: "en".into(),
        },
    };
    if saved.version != 1
        || saved.revision != revision
        || saved.model_id != super::MODEL_ID
        || saved.target_language != "en"
    {
        return Err(ApiError::Conflict(
            "Text translation configuration changed".into(),
        ));
    }
    Ok(saved)
}
fn revision_number(raw: &str) -> Result<i64, ApiError> {
    raw.parse::<i64>()
        .ok()
        .filter(|v| *v >= 0 && v.to_string() == raw)
        .ok_or_else(|| ApiError::BadRequest("Choose the current configuration revision".into()))
}
pub(crate) async fn save(state: &AppState, input: UpdateRequest) -> Result<Snapshot, ApiError> {
    if input.version != 1 || input.model_id != super::MODEL_ID {
        return Err(ApiError::BadRequest(
            "Choose the supported text translation model".into(),
        ));
    }
    let expected = revision_number(&input.expected_revision)?;
    let revision = expected
        .checked_add(1)
        .ok_or_else(|| ApiError::Conflict("Text translation revision exhausted".into()))?
        .to_string();
    let saved = Snapshot {
        version: 1,
        revision: revision.clone(),
        enabled: input.enabled,
        model_id: input.model_id,
        target_language: "en".into(),
    };
    let raw = serde_json::to_string(&saved)
        .map_err(|_| ApiError::Internal("Text configuration encoding failed".into()))?;
    if !state
        .store
        .put_settings_if_generation(
            GENERATION,
            expected,
            &[(GENERATION, &revision), (KEY, &raw)],
        )
        .await?
    {
        return Err(ApiError::typed(
            axum::http::StatusCode::CONFLICT,
            "configuration_conflict",
            "Text translation configuration changed; refresh before saving",
        ));
    }
    Ok(saved)
}
