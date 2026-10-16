//! One authoritative subtitle setup, with CAS for current UIs and an explicit
//! last-writer-wins Advanced transition for the legacy endpoint. Worker readiness
//! is deliberately absent from validation: it cannot rewrite an operator choice.
use super::artifacts::CatalogModel;
use crate::{http::error::ApiError, subtitle_transcription::Settings};
use plurx_core::{
    error::StoreError,
    store::{
        background_jobs_transcription::{whisper_language, SubtitleConfigurationWrite},
        Store,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MANAGED_KEY: &str = "subtitles.transcription.managed.v1";
const LEGACY_KEY: &str = "subtitles.transcription.config";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    Managed,
    Advanced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModeStatus {
    Ok,
    Advanced,
    ConfigurationConflict,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Advanced {
    pub command: String,
    pub model_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateRequest {
    pub version: u8,
    pub expected_revision: String,
    pub mode: Mode,
    pub enabled: bool,
    pub interval_mins: u32,
    pub language: String,
    pub model_id: String,
    pub advanced: Option<Advanced>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Snapshot {
    pub version: u8,
    pub revision: String,
    pub mode: Mode,
    /// Saved operator choice, including during a configuration conflict.
    pub enabled: bool,
    /// Admission reads this value; observations never replace `enabled`.
    pub effective_enabled: bool,
    pub interval_mins: u32,
    pub language: String,
    pub model_id: String,
    pub advanced: Advanced,
    pub mode_status: ModeStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u8,
    revision: String,
    mode: Mode,
    enabled: bool,
    interval_mins: u32,
    language: String,
    model_id: String,
    advanced: Advanced,
    legacy_snapshot: Settings,
}

fn language(value: &str) -> Result<String, ApiError> {
    let value = crate::subtitle_transcription::normalized_language(value);
    if !whisper_language(&value) {
        return Err(ApiError::BadRequest(
            "Choose a spoken language supported by the transcription engine".into(),
        ));
    }
    Ok(value)
}

fn advanced(legacy: &Settings) -> Advanced {
    Advanced {
        command: legacy.command.clone(),
        model_path: legacy.model_path.clone(),
    }
}

fn snapshot(saved: &Saved, conflict: bool) -> Snapshot {
    Snapshot {
        version: 1,
        revision: saved.revision.clone(),
        mode: saved.mode,
        enabled: saved.enabled,
        effective_enabled: saved.enabled && !conflict,
        interval_mins: saved.interval_mins,
        language: saved.language.clone(),
        model_id: saved.model_id.clone(),
        advanced: saved.advanced.clone(),
        mode_status: if conflict {
            ModeStatus::ConfigurationConflict
        } else if saved.mode == Mode::Advanced {
            ModeStatus::Advanced
        } else {
            ModeStatus::Ok
        },
        reason: conflict.then(|| {
            "Another server changed the legacy transcription settings. Review and save the setup you intend to use.".into()
        }),
    }
}

fn invalid_saved() -> StoreError {
    StoreError::Task("invalid managed transcription configuration".into())
}

struct Loaded {
    snapshot: Snapshot,
    legacy: Settings,
    managed_revision: String,
    legacy_raw: Option<String>,
}

fn composite_revision(revision: &str, raw: Option<&str>) -> Result<String, StoreError> {
    // Settings documents contain only scalar fields. An explicit BTreeMap
    // makes canonical ordering independent of serde_json's feature selection.
    // Hash all observed keys; canonical null is reserved for exact absence.
    let canonical = match raw {
        Some(raw) => {
            let object: BTreeMap<String, serde_json::Value> =
                serde_json::from_str(raw).map_err(|_| {
                    StoreError::Task("invalid legacy transcription configuration".into())
                })?;
            serde_json::to_vec(&object).map_err(|_| invalid_saved())?
        }
        None => b"null".to_vec(),
    };
    Ok(format!("{revision}.{:x}", Sha256::digest(canonical)))
}

fn valid_revision(value: &str) -> bool {
    let Some((revision, digest)) = value.split_once('.') else {
        return false;
    };
    (revision == "0" || uuid::Uuid::parse_str(revision).is_ok())
        && digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn read_state(store: &dyn Store) -> Result<Loaded, StoreError> {
    // Both rows come from one snapshot, including the exact legacy bytes used
    // by the atomic compare-and-swap. Unknown or malformed legacy fields fail
    // explicitly rather than disappearing into a default or partial hash.
    let rows = store.get_settings(&[MANAGED_KEY, LEGACY_KEY]).await?;
    let legacy_raw = rows.get(LEGACY_KEY).cloned();
    let legacy = legacy_raw
        .as_deref()
        .map(serde_json::from_str::<Settings>)
        .transpose()
        .map_err(|_| StoreError::Task("invalid legacy transcription configuration".into()))?;
    let Some(raw) = rows.get(MANAGED_KEY) else {
        let configured = legacy.is_some();
        let legacy = legacy.unwrap_or_default();
        let saved = Saved {
            version: 1,
            revision: "0".into(),
            mode: if configured {
                Mode::Advanced
            } else {
                Mode::Managed
            },
            enabled: legacy.enabled,
            interval_mins: legacy.interval_mins,
            language: crate::subtitle_transcription::normalized_language(&legacy.language),
            model_id: "base.en".into(),
            advanced: advanced(&legacy),
            legacy_snapshot: legacy.clone(),
        };
        let mut snapshot = snapshot(&saved, false);
        snapshot.revision = composite_revision("0", legacy_raw.as_deref())?;
        return Ok(Loaded {
            snapshot,
            legacy,
            managed_revision: "0".into(),
            legacy_raw,
        });
    };
    let mut saved: Saved = serde_json::from_str(raw).map_err(|_| invalid_saved())?;
    if saved.version != 1
        || uuid::Uuid::parse_str(&saved.revision).is_err()
        || saved.interval_mins > 10_080
        || !whisper_language(&saved.language)
    {
        return Err(invalid_saved());
    }
    let managed_revision = saved.revision.clone();
    let legacy_matches = match &legacy {
        Some(actual) => {
            serde_json::to_value(actual).map_err(|_| invalid_saved())?
                == serde_json::to_value(&saved.legacy_snapshot).map_err(|_| invalid_saved())?
        }
        None => false,
    };
    let (mut snapshot, legacy) = if saved.mode == Mode::Advanced {
        // An older binary is an explicit competing Advanced writer. Preserve
        // its actual choices; this is compatibility, not a second active mode.
        if let Some(actual) = legacy {
            saved.enabled = actual.enabled;
            saved.interval_mins = actual.interval_mins;
            saved.language = crate::subtitle_transcription::normalized_language(&actual.language);
            saved.advanced = advanced(&actual);
            (snapshot(&saved, false), actual)
        } else {
            (snapshot(&saved, true), saved.legacy_snapshot)
        }
    } else {
        (snapshot(&saved, !legacy_matches), saved.legacy_snapshot)
    };
    snapshot.revision = composite_revision(&managed_revision, legacy_raw.as_deref())?;
    Ok(Loaded {
        snapshot,
        legacy,
        managed_revision,
        legacy_raw,
    })
}

pub(crate) async fn read(store: &dyn Store) -> Result<Snapshot, StoreError> {
    read_state(store).await.map(|loaded| loaded.snapshot)
}

async fn write(
    store: &dyn Store,
    saved: Saved,
    expected_revision: Option<String>,
    expected_legacy: Option<String>,
) -> Result<Snapshot, ApiError> {
    let legacy = serde_json::to_value(&saved.legacy_snapshot)
        .map_err(|_| ApiError::Internal("Could not encode legacy subtitle setup".into()))?;
    let raw = serde_json::to_string(&legacy)
        .map_err(|_| ApiError::Internal("Could not encode legacy subtitle setup".into()))?;
    let mut response = snapshot(&saved, false);
    response.revision = composite_revision(&saved.revision, Some(&raw))?;
    let updated = store
        .write_subtitle_configuration(SubtitleConfigurationWrite {
            expected_revision,
            expected_legacy,
            revision: saved.revision.clone(),
            managed: serde_json::to_value(&saved)
                .map_err(|_| ApiError::Internal("Could not encode subtitle setup".into()))?,
            legacy,
            now_ms: crate::state::clock_ms(),
        })
        .await?;
    if !updated {
        return Err(ApiError::Conflict(
            "Subtitle setup changed while you were editing it. Reload Settings before saving."
                .into(),
        ));
    }
    Ok(response)
}

pub(crate) async fn save(
    store: &dyn Store,
    request: UpdateRequest,
    catalog: &[CatalogModel],
) -> Result<Snapshot, ApiError> {
    if request.version != 1
        || !valid_revision(&request.expected_revision)
        || request.interval_mins > 10_080
    {
        return Err(ApiError::BadRequest(
            "Choose supported subtitle settings and a current configuration revision".into(),
        ));
    }
    let language = language(&request.language)?;
    let loaded = read_state(store).await?;
    let current = loaded.snapshot;
    let mut legacy = loaded.legacy;
    if current.revision != request.expected_revision {
        return Err(ApiError::Conflict(
            "Subtitle setup changed while you were editing it. Reload Settings before saving."
                .into(),
        ));
    }
    let advanced = match request.mode {
        Mode::Managed => {
            if request.advanced.is_some() {
                return Err(ApiError::BadRequest(
                    "Existing installation paths require explicit Advanced mode".into(),
                ));
            }
            let model = catalog
                .iter()
                .find(|model| model.id == request.model_id)
                .ok_or_else(|| ApiError::BadRequest("Choose a reviewed catalog model".into()))?;
            if !model.languages.contains(&language) {
                return Err(ApiError::BadRequest(
                    "Choose a spoken language supported by the selected model".into(),
                ));
            }
            legacy.enabled = false;
            legacy.interval_mins = 0;
            current.advanced
        }
        Mode::Advanced => {
            let advanced = request.advanced.ok_or_else(|| {
                ApiError::BadRequest(
                    "Advanced mode requires the existing local installation".into(),
                )
            })?;
            legacy = Settings {
                enabled: request.enabled,
                interval_mins: request.interval_mins,
                command: advanced.command.clone(),
                model_path: advanced.model_path.clone(),
                language: language.clone(),
            };
            legacy.validate()?;
            advanced
        }
    };
    write(
        store,
        Saved {
            version: 1,
            revision: uuid::Uuid::new_v4().to_string(),
            mode: request.mode,
            enabled: request.enabled,
            interval_mins: request.interval_mins,
            language,
            model_id: request.model_id,
            advanced,
            legacy_snapshot: legacy,
        },
        Some(loaded.managed_revision),
        loaded.legacy_raw,
    )
    .await
}

pub(crate) async fn activate_advanced(
    store: &dyn Store,
    mut legacy: Settings,
) -> Result<Snapshot, ApiError> {
    legacy.language = language(&legacy.language)?;
    legacy.validate()?;
    let current = read(store).await?;
    write(
        store,
        Saved {
            version: 1,
            revision: uuid::Uuid::new_v4().to_string(),
            mode: Mode::Advanced,
            enabled: legacy.enabled,
            interval_mins: legacy.interval_mins,
            language: legacy.language.clone(),
            model_id: current.model_id,
            advanced: advanced(&legacy),
            legacy_snapshot: legacy,
        },
        None,
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::store::{SettingsStore, SqliteStore};

    fn catalog() -> Vec<CatalogModel> {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("catalog.json")).expect("reviewed catalog");
        serde_json::from_value(value["models"].clone()).expect("catalog model types")
    }

    fn request(revision: &str) -> UpdateRequest {
        UpdateRequest {
            version: 1,
            expected_revision: revision.into(),
            mode: Mode::Managed,
            enabled: true,
            interval_mins: 45,
            language: "en".into(),
            model_id: "base.en".into(),
            advanced: None,
        }
    }

    #[tokio::test]
    async fn fresh_setup_is_virtual_disabled_and_saves_without_any_installed_model() {
        let store = SqliteStore::open_in_memory().expect("SQLite");
        let initial = read(&store).await.expect("virtual defaults");
        assert_eq!(initial.mode, Mode::Managed);
        assert!(initial.revision.starts_with("0."));
        assert!(!initial.enabled);
        assert_eq!(initial.interval_mins, 0);
        assert!(store.get_setting(MANAGED_KEY).await.expect("valid fresh setup is virtual disabled and saves without any installed model fixture").is_none());
        let saved = save(
            &store,
            request(&read(&store).await.expect("valid fresh setup is virtual disabled and saves without any installed model fixture").revision),
            &catalog(),
        )
        .await
        .expect("save");
        assert!(saved.enabled && saved.effective_enabled);
        assert_ne!(saved.revision, "0");
        let legacy: Settings =
            serde_json::from_str(&store.get_setting(LEGACY_KEY).await.expect("valid fresh setup is virtual disabled and saves without any installed model fixture").expect("valid fresh setup is virtual disabled and saves without any installed model fixture")).expect("valid fresh setup is virtual disabled and saves without any installed model fixture");
        assert!(!legacy.enabled);
        assert_eq!(legacy.interval_mins, 0);
        assert_eq!(read(&store).await.expect("valid fresh setup is virtual disabled and saves without any installed model fixture").revision, saved.revision);
    }

    #[tokio::test]
    async fn legacy_upgrade_preserves_explicit_advanced_choices_and_managed_retains_paths() {
        let store = SqliteStore::open_in_memory().expect("SQLite");
        let legacy = Settings {
            enabled: true,
            interval_mins: 71,
            command: "/srv/captions/whisper-cli".into(),
            model_path: "/srv/captions/existing.bin".into(),
            language: "por".into(),
        };
        let raw = serde_json::to_string(&legacy).expect("valid legacy upgrade preserves explicit advanced choices and managed retains paths fixture");
        store.put_settings(&[(LEGACY_KEY, &raw)]).await.expect("valid legacy upgrade preserves explicit advanced choices and managed retains paths fixture");
        let initial = read(&store).await.expect("valid legacy upgrade preserves explicit advanced choices and managed retains paths fixture");
        assert_eq!(initial.mode, Mode::Advanced);
        assert!(initial.enabled);
        assert_eq!(initial.language, "pt");
        assert_eq!(initial.advanced.model_path, legacy.model_path);
        let mut managed = request(&initial.revision);
        managed.model_id = "base".into();
        managed.language = "fr".into();
        let saved = save(&store, managed, &catalog()).await.expect("valid legacy upgrade preserves explicit advanced choices and managed retains paths fixture");
        let paused: Settings =
            serde_json::from_str(&store.get_setting(LEGACY_KEY).await.expect("valid legacy upgrade preserves explicit advanced choices and managed retains paths fixture").expect("valid legacy upgrade preserves explicit advanced choices and managed retains paths fixture")).expect("valid legacy upgrade preserves explicit advanced choices and managed retains paths fixture");
        assert!(!paused.enabled);
        assert_eq!(paused.interval_mins, 0);
        assert_eq!(paused.command, legacy.command);
        assert_eq!(paused.model_path, legacy.model_path);
        assert!(saved.enabled);
        assert_eq!(saved.language, "fr");
        assert_eq!(saved.interval_mins, 45);
    }

    #[tokio::test]
    async fn competing_current_saves_have_one_atomic_cas_winner() {
        let store = SqliteStore::open_in_memory().expect("SQLite");
        let models = catalog();
        let one = request(
            &read(&store)
                .await
                .expect("valid competing current saves have one atomic cas winner fixture")
                .revision,
        );
        let one_revision = one.expected_revision.clone();
        let mut two = one.clone();
        two.enabled = false;
        let (first, second) = tokio::join!(save(&store, one, &models), save(&store, two, &models));
        assert_ne!(first.is_ok(), second.is_ok());
        let winner = first.or(second).expect("one accepted save");
        assert_eq!(
            read(&store)
                .await
                .expect("valid competing current saves have one atomic cas winner fixture")
                .enabled,
            winner.enabled
        );
        assert!(matches!(
            save(&store, request(&one_revision), &models).await,
            Err(ApiError::Conflict(_))
        ));
        assert_eq!(
            read(&store)
                .await
                .expect("valid competing current saves have one atomic cas winner fixture")
                .revision,
            winner.revision
        );
    }

    #[tokio::test]
    async fn old_binary_conflict_pauses_admission_without_resetting_saved_enable() {
        let store = SqliteStore::open_in_memory().expect("SQLite");
        let saved = save(
            &store,
            request(&read(&store).await.expect("valid old binary conflict pauses admission without resetting saved enable fixture").revision),
            &catalog(),
        )
        .await
        .expect("valid old binary conflict pauses admission without resetting saved enable fixture");
        let old_writer = Settings {
            enabled: true,
            interval_mins: 3,
            ..Settings::default()
        };
        let raw = serde_json::to_string(&old_writer).expect(
            "valid old binary conflict pauses admission without resetting saved enable fixture",
        );
        store.put_settings(&[(LEGACY_KEY, &raw)]).await.expect(
            "valid old binary conflict pauses admission without resetting saved enable fixture",
        );
        let conflict = read(&store).await.expect(
            "valid old binary conflict pauses admission without resetting saved enable fixture",
        );
        assert_eq!(conflict.mode_status, ModeStatus::ConfigurationConflict);
        assert!(conflict.enabled);
        assert!(!conflict.effective_enabled);
        assert_ne!(conflict.revision, saved.revision);
        assert!(matches!(
            save(&store, request(&saved.revision), &catalog()).await,
            Err(ApiError::Conflict(_))
        ));
        assert!(serde_json::to_value(&conflict)
            .expect(
                "valid old binary conflict pauses admission without resetting saved enable fixture"
            )
            .get("legacy_snapshot")
            .is_none());
        let restored = save(&store, request(&conflict.revision), &catalog())
            .await
            .expect(
                "valid old binary conflict pauses admission without resetting saved enable fixture",
            );
        assert!(restored.enabled && restored.effective_enabled);
        assert_eq!(restored.mode_status, ModeStatus::Ok);
    }

    #[tokio::test]
    async fn legacy_endpoint_explicitly_switches_to_advanced_as_one_lww_write() {
        let store = SqliteStore::open_in_memory().expect("SQLite");
        let saved = save(
            &store,
            request(&read(&store).await.expect("valid legacy endpoint explicitly switches to advanced as one lww write fixture").revision),
            &catalog(),
        )
        .await
        .expect("valid legacy endpoint explicitly switches to advanced as one lww write fixture");
        let advanced = Settings {
            enabled: true,
            interval_mins: 73,
            command: "/srv/captions/whisper-cli".into(),
            model_path: "/srv/captions/model.bin".into(),
            language: "swe".into(),
        };
        let updated = activate_advanced(&store, advanced).await.expect(
            "valid legacy endpoint explicitly switches to advanced as one lww write fixture",
        );
        assert_eq!(updated.mode, Mode::Advanced);
        assert_eq!(updated.mode_status, ModeStatus::Advanced);
        assert_eq!(updated.language, "sv");
        assert_eq!(updated.interval_mins, 73);
        assert_ne!(updated.revision, saved.revision);
        assert!(updated.enabled && updated.effective_enabled);
        let restarted = read(&store).await.expect(
            "valid legacy endpoint explicitly switches to advanced as one lww write fixture",
        );
        assert_eq!(restarted.revision, updated.revision);
        assert_eq!(restarted.advanced.model_path, "/srv/captions/model.bin");
    }

    #[tokio::test]
    async fn unsupported_versions_models_languages_and_missing_advanced_are_refused() {
        let store = SqliteStore::open_in_memory().expect("SQLite");
        for invalid in [
            "version",
            "model",
            "language",
            "model_language",
            "catalog_yue",
            "advanced",
        ] {
            let mut candidate = request(&read(&store).await.expect("valid unsupported versions models languages and missing advanced are refused fixture").revision);
            match invalid {
                "version" => candidate.version = 2,
                "model" => candidate.model_id = "unreviewed-model".into(),
                "language" => candidate.language = "zz".into(),
                "model_language" => candidate.language = "fr".into(),
                "catalog_yue" => {
                    candidate.model_id = "base".into();
                    candidate.language = "yue".into();
                }
                _ => candidate.mode = Mode::Advanced,
            }
            assert!(matches!(
                save(&store, candidate, &catalog()).await,
                Err(ApiError::BadRequest(_))
            ));
        }
        assert!(store.get_setting(MANAGED_KEY).await.expect("valid unsupported versions models languages and missing advanced are refused fixture").is_none());
        let unknown = serde_json::json!({"version":1,"expected_revision":"0","mode":"managed","enabled":true,"interval_mins":0,"language":"en","model_id":"base.en","surprise":true});
        assert!(serde_json::from_value::<UpdateRequest>(unknown).is_err());
    }

    #[tokio::test]
    async fn opaque_revision_fences_full_legacy_changes_and_exact_absence() {
        let store = SqliteStore::open_in_memory().expect("SQLite");
        let absent = read(&store)
            .await
            .expect("valid opaque revision fences full legacy changes and exact absence fixture");
        let raw = serde_json::to_string(&Settings::default())
            .expect("valid opaque revision fences full legacy changes and exact absence fixture");
        store
            .put_settings(&[(LEGACY_KEY, &raw)])
            .await
            .expect("valid opaque revision fences full legacy changes and exact absence fixture");
        assert!(matches!(
            save(&store, request(&absent.revision), &catalog()).await,
            Err(ApiError::Conflict(_))
        ));
        assert!(store
            .get_setting(MANAGED_KEY)
            .await
            .expect("valid opaque revision fences full legacy changes and exact absence fixture")
            .is_none());
        assert_eq!(
            store
                .get_setting(LEGACY_KEY)
                .await
                .expect(
                    "valid opaque revision fences full legacy changes and exact absence fixture"
                )
                .as_deref(),
            Some(raw.as_str())
        );
        let complete = read(&store)
            .await
            .expect("valid opaque revision fences full legacy changes and exact absence fixture");
        // Missing optional fields decode to the same Settings, but they are a
        // different full document and must not share its observation token.
        store
            .put_settings(&[(LEGACY_KEY, "{\"enabled\":false}")])
            .await
            .expect("valid opaque revision fences full legacy changes and exact absence fixture");
        let projected = read(&store)
            .await
            .expect("valid opaque revision fences full legacy changes and exact absence fixture");
        assert_ne!(complete.revision, projected.revision);
        assert!(matches!(
            save(&store, request(&complete.revision), &catalog()).await,
            Err(ApiError::Conflict(_))
        ));
        let changed = Settings {
            model_path: "/srv/captions/other.bin".into(),
            ..Settings::default()
        };
        let changed_raw = serde_json::to_string(&changed)
            .expect("valid opaque revision fences full legacy changes and exact absence fixture");
        store
            .put_settings(&[(LEGACY_KEY, &changed_raw)])
            .await
            .expect("valid opaque revision fences full legacy changes and exact absence fixture");
        assert!(matches!(
            save(&store, request(&projected.revision), &catalog()).await,
            Err(ApiError::Conflict(_))
        ));
        assert_eq!(
            store
                .get_setting(LEGACY_KEY)
                .await
                .expect(
                    "valid opaque revision fences full legacy changes and exact absence fixture"
                )
                .as_deref(),
            Some(changed_raw.as_str())
        );
    }

    #[tokio::test]
    async fn canonical_legacy_identity_survives_reordering_and_restart_but_unknown_fields_fail() {
        let directory = tempfile::tempdir().expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
        let path = directory.path().join("subtitle-settings.sqlite");
        let store = SqliteStore::open(&path).expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
        let raw = serde_json::to_string(&Settings::default()).expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
        store.put_settings(&[(LEGACY_KEY, &raw)]).await.expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
        let initial = read(&store).await.expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
        let reordered = r#"{ "language":"en", "model_path":"", "command":"whisper-cli", "interval_mins":0, "enabled":false }"#;
        store
            .put_settings(&[(LEGACY_KEY, reordered)])
            .await
            .expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
        assert_eq!(read(&store).await.expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture").revision, initial.revision);
        let saved = save(&store, request(&initial.revision), &catalog())
            .await
            .expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
        drop(store);
        let reopened = SqliteStore::open(&path).expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
        assert_eq!(read(&reopened).await.expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture").revision, saved.revision);
        for invalid in ["not JSON", r#"{"enabled":false,"unknown_option":true}"#] {
            reopened
                .put_settings(&[(LEGACY_KEY, invalid)])
                .await
                .expect("valid canonical legacy identity survives reordering and restart but unknown fields fail fixture");
            assert!(read(&reopened).await.is_err());
            assert!(save(&reopened, request(&saved.revision), &catalog())
                .await
                .is_err());
        }
    }
}
