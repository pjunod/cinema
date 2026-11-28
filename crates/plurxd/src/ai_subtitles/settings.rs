//! The one AI subtitles settings document, `subtitles.ai.v1`.
//!
//! It is saved with the integer generation compare-and-set that text
//! translation already uses (`put_settings_if_generation`). Until the first
//! save the document is derived from the three older switches, and the boot
//! seed writes that derivation once per cluster so the admission SQL, which
//! reads only this document, keeps admitting what the old switches admitted.
//!
//! For one release every save also writes the old switches as a mirror in the
//! same settings transaction, so a node still on the previous release keeps
//! admitting (or refusing) the same work during a rolling upgrade.
use crate::{http::error::ApiError, state::AppState};
use axum::http::StatusCode;
use plurx_core::store::{
    ai_subtitles::{SETTINGS_GENERATION_KEY as GENERATION, SETTINGS_KEY as KEY},
    subtitle_translation_settings::{TranslationSettings, SETTINGS_KEY as SPEECH_KEY},
    Store,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The previous release's text translation switch and its generation.
const TEXT_KEY: &str = crate::subtitle_text_translation::config::KEY;
const TEXT_GENERATION: &str = crate::subtitle_text_translation::config::GENERATION;
/// The automatic subtitle download switch (`"true"` when on).
const DOWNLOAD_KEY: &str = crate::http::subtitle_downloads::AUTOMATIC_KEY;

/// Both AI routes on, limited to admins: what admin-only fixtures expect.
#[cfg(test)]
pub(crate) const TEST_ADMINS_ALL_ON: &str = r#"{"version":1,"download_match":true,"translate_text":true,"from_audio":true,"ahead_of_time":false,"who":"admins","quality":"balanced"}"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Who {
    Everyone,
    Admins,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Quality {
    Fast,
    Balanced,
    Best,
}

/// The stored document. Admission SQL reads `version`, `who`,
/// `translate_text` and `from_audio` by name; keep them in step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub version: u8,
    pub download_match: bool,
    pub translate_text: bool,
    pub from_audio: bool,
    pub ahead_of_time: bool,
    pub who: Who,
    pub quality: Quality,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Snapshot {
    #[serde(flatten)]
    pub settings: Settings,
    /// The generation a save must name as `expected_revision`.
    pub revision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateRequest {
    pub version: u8,
    pub expected_revision: String,
    pub download_match: bool,
    pub translate_text: bool,
    pub from_audio: bool,
    pub ahead_of_time: bool,
    pub who: Who,
    pub quality: Quality,
}

fn revision_number(raw: &str) -> Result<i64, ApiError> {
    raw.parse::<i64>()
        .ok()
        .filter(|v| *v >= 0 && v.to_string() == raw)
        .ok_or_else(|| ApiError::BadRequest("Choose the current settings revision".into()))
}

fn invalid() -> ApiError {
    ApiError::Conflict("AI subtitle settings are invalid".into())
}

/// The settings this snapshot holds, or the derivation from the older
/// switches when the document has never been saved.
async fn current(store: &dyn Store) -> Result<(Settings, i64, BTreeMap<String, String>), ApiError> {
    let rows = store
        .get_settings(&[
            KEY,
            GENERATION,
            TEXT_KEY,
            TEXT_GENERATION,
            SPEECH_KEY,
            DOWNLOAD_KEY,
        ])
        .await?;
    let generation =
        revision_number(rows.get(GENERATION).map_or("0", String::as_str)).map_err(|_| invalid())?;
    let settings = match rows.get(KEY) {
        Some(raw) => {
            let saved: Settings = serde_json::from_str(raw).map_err(|_| invalid())?;
            if saved.version != 1 {
                return Err(invalid());
            }
            saved
        }
        None => derived(store, &rows).await,
    };
    Ok((settings, generation, rows))
}

/// Defaults from the previous release's switches. An unreadable old value
/// counts as off: the derivation never turns on work nobody enabled.
async fn derived(store: &dyn Store, rows: &BTreeMap<String, String>) -> Settings {
    let enabled = |key: &str| {
        rows.get(key)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .is_some_and(|value| value["enabled"] == serde_json::Value::Bool(true))
    };
    let managed = crate::subtitle_models::config::read(store)
        .await
        .ok()
        .filter(|managed| {
            managed.mode == crate::subtitle_models::config::Mode::Managed && managed.enabled
        });
    Settings {
        version: 1,
        download_match: rows.get(DOWNLOAD_KEY).is_none_or(|value| value == "true"),
        translate_text: enabled(TEXT_KEY),
        from_audio: enabled(SPEECH_KEY) || managed.is_some(),
        ahead_of_time: managed.is_some_and(|managed| managed.interval_mins > 0),
        who: Who::Everyone,
        quality: Quality::Balanced,
    }
}

pub(crate) async fn read(state: &AppState) -> Result<Snapshot, ApiError> {
    read_store(state.store.as_ref()).await
}

pub(crate) async fn read_store(store: &dyn Store) -> Result<Snapshot, ApiError> {
    let (settings, generation, _) = current(store).await?;
    Ok(Snapshot {
        settings,
        revision: generation.to_string(),
    })
}

/// Write the derived document once per cluster if it has never been saved.
/// Admission reads only `subtitles.ai.v1`, so this runs at boot before any
/// worker claims; a concurrent save or another node's seed wins unchanged.
pub(crate) async fn seed(store: &dyn Store) -> Result<(), ApiError> {
    let (settings, _, rows) = current(store).await?;
    if rows.contains_key(KEY) {
        return Ok(());
    }
    let raw = serde_json::to_string(&settings)
        .map_err(|_| ApiError::Internal("AI subtitle settings encoding failed".into()))?;
    store.put_setting_if_absent(KEY, &raw).await?;
    Ok(())
}

pub(crate) async fn save(state: &AppState, input: UpdateRequest) -> Result<Snapshot, ApiError> {
    save_store(state.store.as_ref(), input).await
}

pub(crate) async fn save_store(
    store: &dyn Store,
    input: UpdateRequest,
) -> Result<Snapshot, ApiError> {
    if input.version != 1 {
        return Err(ApiError::BadRequest(
            "Choose the supported AI subtitle settings version".into(),
        ));
    }
    let expected = revision_number(&input.expected_revision)?;
    let next = expected
        .checked_add(1)
        .ok_or_else(|| ApiError::Conflict("AI subtitle settings revision exhausted".into()))?;
    let settings = Settings {
        version: 1,
        download_match: input.download_match,
        translate_text: input.translate_text,
        from_audio: input.from_audio,
        ahead_of_time: input.ahead_of_time,
        who: input.who,
        quality: input.quality,
    };
    let (_, _, rows) = current(store).await?;
    let raw = serde_json::to_string(&settings)
        .map_err(|_| ApiError::Internal("AI subtitle settings encoding failed".into()))?;
    let (speech_raw, text_raw, text_generation) = mirror(&settings, &rows)?;
    let next_generation = next.to_string();
    if !store
        .put_settings_if_generation(
            GENERATION,
            expected,
            &[
                (GENERATION, &next_generation),
                (KEY, &raw),
                (SPEECH_KEY, &speech_raw),
                (TEXT_KEY, &text_raw),
                (TEXT_GENERATION, &text_generation),
            ],
        )
        .await?
    {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "configuration_conflict",
            "AI subtitle settings changed; refresh before saving",
        ));
    }
    Ok(Snapshot {
        settings,
        revision: next_generation,
    })
}

/// The previous release's two switches, rewritten in their own shapes.
///
/// Speech translation keeps its saved model, profile and spoken-language
/// choices and gets a fresh revision (its CAS token is `revision.digest`).
/// Text translation is bumped to its next generation with the document's
/// revision equal to that generation, which is what its reader requires.
fn mirror(
    settings: &Settings,
    rows: &BTreeMap<String, String>,
) -> Result<(String, String, String), ApiError> {
    let mut speech = rows
        .get(SPEECH_KEY)
        .and_then(|raw| serde_json::from_str::<TranslationSettings>(raw).ok())
        .filter(|saved| saved.validate().is_ok())
        .unwrap_or_default();
    speech.enabled = settings.from_audio;
    speech.revision = uuid::Uuid::new_v4().to_string();
    speech
        .validate()
        .map_err(|_| ApiError::Internal("Speech translation mirror is invalid".into()))?;
    let text_generation = rows
        .get(TEXT_GENERATION)
        .map_or(Some(0), |raw| raw.parse::<i64>().ok().filter(|v| *v >= 0))
        .and_then(|v| v.checked_add(1))
        .ok_or_else(invalid)?
        .to_string();
    let text = crate::subtitle_text_translation::config::Snapshot {
        version: 1,
        revision: text_generation.clone(),
        enabled: settings.translate_text,
        model_id: crate::subtitle_text_translation::MODEL_ID.into(),
        target_language: "en".into(),
    };
    let encoded = |result: serde_json::Result<String>| {
        result.map_err(|_| ApiError::Internal("AI subtitle mirror encoding failed".into()))
    };
    Ok((
        encoded(serde_json::to_string(&speech))?,
        encoded(serde_json::to_string(&text))?,
        text_generation,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::store::{SettingsStore, SqliteStore};

    fn update(expected: &str, translate_text: bool, from_audio: bool, who: Who) -> UpdateRequest {
        UpdateRequest {
            version: 1,
            expected_revision: expected.into(),
            download_match: false,
            translate_text,
            from_audio,
            ahead_of_time: false,
            who,
            quality: Quality::Best,
        }
    }

    /// The unsaved document derives from the old switches; a save is a CAS
    /// on the integer generation, a stale save changes nothing, and every
    /// save rewrites both old switches in their own shapes as the mirror a
    /// previous-release node admits from.
    #[tokio::test]
    async fn ai_subtitle_settings_round_trip_cas_conflict_and_mirror_old_switches() {
        let store = SqliteStore::open_in_memory().expect("SQLite settings fixture");
        let speech = TranslationSettings {
            enabled: true,
            model_id: Some("small".into()),
            ..Default::default()
        };
        store
            .put_setting(SPEECH_KEY, &serde_json::to_string(&speech).expect("speech"))
            .await
            .expect("old speech switch on");
        let initial = read_store(&store).await.expect("derived");
        assert_eq!(initial.revision, "0");
        assert_eq!(
            initial.settings,
            Settings {
                version: 1,
                download_match: true,
                translate_text: false,
                from_audio: true,
                ahead_of_time: false,
                who: Who::Everyone,
                quality: Quality::Balanced,
            }
        );

        let saved = save_store(&store, update("0", true, false, Who::Admins))
            .await
            .expect("first save");
        assert_eq!(saved.revision, "1");
        let reread = read_store(&store).await.expect("saved");
        assert_eq!(reread.revision, "1");
        assert_eq!(reread.settings, saved.settings);
        assert!(reread.settings.translate_text && !reread.settings.from_audio);
        assert_eq!(reread.settings.who, Who::Admins);

        let stale = save_store(&store, update("0", false, true, Who::Everyone))
            .await
            .expect_err("stale revision");
        assert!(
            matches!(
                stale,
                ApiError::Typed {
                    code: "configuration_conflict",
                    ..
                }
            ),
            "{stale:?}"
        );
        assert_eq!(
            read_store(&store).await.expect("unchanged").settings,
            saved.settings
        );

        let mirrored: TranslationSettings = serde_json::from_str(
            &store
                .get_setting(SPEECH_KEY)
                .await
                .expect("speech mirror")
                .expect("present"),
        )
        .expect("old speech shape");
        mirrored.validate().expect("old speech validation");
        assert!(!mirrored.enabled, "speech mirror follows from_audio");
        assert_eq!(
            mirrored.model_id.as_deref(),
            Some("small"),
            "old choices kept"
        );
        let (text, generation) = store
            .get_setting_pair(TEXT_KEY, TEXT_GENERATION)
            .await
            .expect("text mirror");
        let text: crate::subtitle_text_translation::config::Snapshot =
            serde_json::from_str(&text.expect("text present")).expect("old text shape");
        assert_eq!(generation.as_deref(), Some("1"));
        assert_eq!(
            text.revision, "1",
            "old text reader requires revision == generation"
        );
        assert!(text.enabled, "text mirror follows translate_text");
        assert_eq!(text.model_id, crate::subtitle_text_translation::MODEL_ID);
        assert_eq!(text.target_language, "en");

        let second = save_store(&store, update("1", false, true, Who::Everyone))
            .await
            .expect("second save");
        assert_eq!(second.revision, "2");
        let (_, generation) = store
            .get_setting_pair(TEXT_KEY, TEXT_GENERATION)
            .await
            .expect("text mirror bumped");
        assert_eq!(generation.as_deref(), Some("2"));
    }

    /// The boot seed writes the derivation once and never replaces a save.
    #[tokio::test]
    async fn ai_subtitle_settings_seed_writes_derivation_once_and_keeps_saves() {
        let store = SqliteStore::open_in_memory().expect("SQLite settings fixture");
        store
            .put_settings(&[
                (TEXT_KEY, r#"{"version":1,"revision":"0","enabled":true,"model_id":"opus-mt-mul-en","target_language":"en"}"#),
                (DOWNLOAD_KEY, "false"),
            ])
            .await
            .expect("old text switch on, downloads off");
        seed(&store).await.expect("seed");
        let seeded: Settings = serde_json::from_str(
            &store
                .get_setting(KEY)
                .await
                .expect("seeded")
                .expect("present"),
        )
        .expect("seeded shape");
        assert!(seeded.translate_text && !seeded.from_audio && !seeded.download_match);
        assert_eq!(seeded.who, Who::Everyone);
        save_store(&store, update("0", false, false, Who::Admins))
            .await
            .expect("save over seed");
        seed(&store).await.expect("second seed");
        let after = read_store(&store).await.expect("saved");
        assert_eq!(after.settings.who, Who::Admins);
        assert!(!after.settings.translate_text);
    }
}
