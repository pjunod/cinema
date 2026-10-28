//! Translation preference CAS is independent of installed model readiness.
use crate::http::error::ApiError;
use axum::http::StatusCode;
use plurx_core::store::{
    subtitle_translation_read::{TranslationQuery, TranslationRead},
    subtitle_translation_settings::{
        settings_revision, TranslationSettings, TranslationSettingsWrite, SETTINGS_KEY,
    },
    Store,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateRequest {
    pub version: u8,
    pub request_id: String,
    pub intent_expires_at_ms: i64,
    pub expected_revision: String,
    pub enabled: bool,
    #[serde(deserialize_with = "nullable_string")]
    pub model_id: Option<String>,
    #[serde(deserialize_with = "nullable_string")]
    pub profile_id: Option<String>,
    #[serde(deserialize_with = "nullable_string")]
    pub preferred_spoken_language: Option<String>,
}

fn nullable_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Snapshot {
    pub version: u8,
    pub revision: String,
    pub enabled: bool,
    pub model_id: Option<String>,
    pub profile_id: Option<String>,
    pub preferred_spoken_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'static str>,
    pub server_now_ms: i64,
}

pub(crate) fn failure(status: StatusCode, code: &'static str, now: i64) -> ApiError {
    ApiError::typed_detail(
        status,
        code,
        "Translation request was not accepted; refresh current status before a new action",
        json!({"version":1,"server_now_ms":now}),
    )
}

fn snapshot(
    saved: TranslationSettings,
    revision: String,
    now: i64,
    outcome: Option<&'static str>,
) -> Snapshot {
    Snapshot {
        version: 1,
        revision,
        enabled: saved.enabled,
        model_id: saved.model_id,
        profile_id: saved.profile_id,
        preferred_spoken_language: saved.preferred_spoken_language,
        outcome,
        server_now_ms: now,
    }
}

async fn observed(
    store: &dyn Store,
    now: i64,
) -> Result<(TranslationSettings, Option<String>, String), ApiError> {
    let raw = store.get_setting(SETTINGS_KEY).await?;
    let revision = settings_revision(raw.as_deref())
        .map_err(|_| failure(StatusCode::CONFLICT, "configuration_conflict", now))?;
    let saved = match raw.as_deref() {
        Some(raw) => serde_json::from_str(raw)
            .map_err(|_| failure(StatusCode::CONFLICT, "configuration_conflict", now))?,
        None => TranslationSettings::default(),
    };
    Ok((saved, raw, revision))
}

pub(crate) async fn read(store: &dyn Store, now: i64) -> Result<Snapshot, ApiError> {
    let (saved, _, revision) = observed(store, now).await?;
    Ok(snapshot(saved, revision, now, None))
}

pub(crate) fn request_digest(
    method: &str,
    route: &str,
    body: &impl Serialize,
) -> Result<String, ApiError> {
    Ok(plurx_core::store::subtitle_translation::digest(
        &json!({"method":method,"route":route,"body":body}),
    )?)
}
pub(crate) async fn replay(
    store: &dyn Store,
    user_id: i64,
    request_id: &str,
    request_digest: &str,
    now: i64,
) -> Result<Option<serde_json::Value>, ApiError> {
    if !store
        .get_user(user_id)
        .await?
        .is_some_and(|user| user.is_admin)
    {
        return Err(failure(StatusCode::FORBIDDEN, "permission_denied", now));
    }
    let rows = store
        .read_subtitle_translation(TranslationRead {
            user_id,
            now_ms: now,
            query: TranslationQuery::RequestReceipt {
                request_id: request_id.into(),
                request_digest: request_digest.into(),
            },
        })
        .await?;
    if rows.len() > 1 {
        return Err(ApiError::Internal(
            "Translation request receipt is not unique".into(),
        ));
    }
    if rows
        .first()
        .is_some_and(|row| row["outcome"] == "request_conflict")
    {
        return Err(failure(StatusCode::CONFLICT, "request_conflict", now));
    }
    Ok(rows.into_iter().next())
}
pub(crate) fn fresh_intent(expires: i64, now: i64) -> Result<(), ApiError> {
    if expires <= now
        || now
            .checked_add(300_000)
            .is_none_or(|maximum| expires > maximum)
    {
        return Err(failure(StatusCode::GONE, "intent_expired", now));
    }
    Ok(())
}

pub(crate) async fn save(
    store: &dyn Store,
    user_id: i64,
    request: UpdateRequest,
    languages: &[String],
    now: i64,
) -> Result<Snapshot, ApiError> {
    let invalid = || failure(StatusCode::BAD_REQUEST, "invalid_request", now);
    if request.version != 1
        || uuid::Uuid::parse_str(&request.request_id).is_err()
        || request.expected_revision.is_empty()
        || request.expected_revision.len() > 128
        || request.intent_expires_at_ms < 0
    {
        return Err(invalid());
    }
    let request_digest = request_digest("PUT", "/subtitle-translation", &request)?;
    if let Some(receipt) = replay(store, user_id, &request.request_id, &request_digest, now).await?
    {
        return saved_receipt(receipt, now);
    }
    fresh_intent(request.intent_expires_at_ms, now)?;
    match (&request.model_id, &request.profile_id) {
        (None, None) => {}
        (Some(model), Some(id))
            if super::profile::profiles().iter().any(|profile| {
                &profile.model_id == model && profile.id().is_ok_and(|candidate| candidate == *id)
            }) => {}
        _ => return Err(invalid()),
    }
    if request
        .preferred_spoken_language
        .as_ref()
        .is_some_and(|language| {
            !plurx_core::store::background_jobs_transcription::whisper_language(language)
                || (request.profile_id.is_some() && !languages.contains(language))
        })
    {
        return Err(invalid());
    }
    let (_, expected_raw, _) = observed(store, now).await?;
    // The store compares both the opaque revision and these exact observed
    // bytes in the same writer snapshot; a read-then-write check is not CAS.
    let result = store
        .write_translation_settings(TranslationSettingsWrite {
            user_id,
            request_digest,
            request_id: request.request_id.clone(),
            intent_expires_at_ms: request.intent_expires_at_ms,
            now_ms: now,
            expected_revision: request.expected_revision,
            expected_raw,
            settings: TranslationSettings {
                version: 1,
                revision: request.request_id,
                enabled: request.enabled,
                model_id: request.model_id,
                profile_id: request.profile_id,
                preferred_spoken_language: request.preferred_spoken_language,
            },
        })
        .await?;
    saved_receipt(result, now)
}
fn saved_receipt(result: serde_json::Value, now: i64) -> Result<Snapshot, ApiError> {
    match result["outcome"].as_str() {
        Some("saved") => {
            let saved: TranslationSettings = serde_json::from_value(result["settings"].clone())
                .map_err(|_| {
                    ApiError::Internal("Translation settings receipt is invalid".into())
                })?;
            let revision = result["revision"].as_str().ok_or_else(|| {
                ApiError::Internal("Translation settings receipt revision is absent".into())
            })?;
            let observed_at = result["server_now_ms"].as_i64().ok_or_else(|| {
                ApiError::Internal("Translation settings receipt clock is absent".into())
            })?;
            Ok(snapshot(saved, revision.into(), observed_at, Some("saved")))
        }
        Some("configuration_conflict") => {
            Err(failure(StatusCode::CONFLICT, "configuration_conflict", now))
        }
        Some("request_conflict") => Err(failure(StatusCode::CONFLICT, "request_conflict", now)),
        Some("intent_expired") => Err(failure(StatusCode::GONE, "intent_expired", now)),
        Some("forbidden") => Err(failure(StatusCode::FORBIDDEN, "permission_denied", now)),
        Some("receipt_quota") => Err(failure(
            StatusCode::TOO_MANY_REQUESTS,
            "quota_exceeded",
            now,
        )),
        _ => Err(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "network_error",
            now,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::store::{SettingsStore, SqliteStore, UserStore};
    use serde_json::Value;

    fn request(revision: &str, now: i64) -> UpdateRequest {
        UpdateRequest {
            version: 1,
            request_id: uuid::Uuid::new_v4().to_string(),
            intent_expires_at_ms: now + 60_000,
            expected_revision: revision.into(),
            enabled: true,
            model_id: None,
            profile_id: None,
            preferred_spoken_language: None,
        }
    }

    fn languages() -> Vec<String> {
        vec!["de".into(), "ar".into(), "ja".into()]
    }

    #[tokio::test]
    async fn enabled_unconfigured_choice_saves_without_worker_or_model_and_replays_exactly() {
        let store = SqliteStore::open_in_memory().expect("SQLite settings fixture");
        let admin = store
            .create_user("admin", "hash", true)
            .await
            .expect("administrator");
        let initial = read(&store, 1_000).await.expect("virtual settings");
        assert!(!initial.enabled);
        assert!(initial.model_id.is_none());
        let input = request(&initial.revision, 1_000);
        let first = save(&store, admin.id, input.clone(), &languages(), 1_000)
            .await
            .expect("save enabled without installed resources");
        assert!(first.enabled);
        assert!(first.model_id.is_none());
        store
            .put_settings(&[(SETTINGS_KEY, "{\"version\":999,\"enabled\":false}")])
            .await
            .expect("changed external writer before exact replay");
        let replay = save(&store, admin.id, input, &languages(), 1_001)
            .await
            .expect("exact request replay");
        assert_eq!(
            serde_json::to_value(first).expect("first DTO"),
            serde_json::to_value(replay).expect("replay DTO")
        );
    }

    #[tokio::test]
    async fn persisted_small_model_without_profile_remains_an_explicit_small_choice() {
        let store = SqliteStore::open_in_memory().expect("legacy settings fixture");
        let raw = serde_json::to_string(&serde_json::json!({"version":1,
            "revision":uuid::Uuid::new_v4().to_string(),"enabled":true,
            "model_id":"small","profile_id":null,"preferred_spoken_language":"de"}))
        .expect("legacy settings");
        store
            .put_settings(&[(SETTINGS_KEY, raw.as_str())])
            .await
            .expect("legacy saved choice");
        let saved = read(&store, 1_000).await.expect("legacy read");
        assert_eq!(saved.model_id.as_deref(), Some("small"));
        assert!(saved.profile_id.is_none());
        assert!(saved.enabled);
        assert_eq!(
            store
                .get_setting(SETTINGS_KEY)
                .await
                .expect("stored bytes")
                .as_deref(),
            Some(raw.as_str())
        );
    }

    #[tokio::test]
    async fn explicit_small_choice_preserves_identity_and_rejects_unregistered_model_pair() {
        let store = SqliteStore::open_in_memory().expect("settings fixture");
        let admin = store
            .create_user("admin", "hash", true)
            .await
            .expect("administrator");
        let initial = read(&store, 1_000).await.expect("unset choice");
        assert!(initial.model_id.is_none());
        let selected = super::super::profile::selected_profile();
        let mut input = request(&initial.revision, 1_000);
        input.model_id = Some(selected.model_id.clone());
        input.profile_id = Some(selected.id().expect("legacy identity"));
        let saved = save(&store, admin.id, input, &languages(), 1_000)
            .await
            .expect("save explicit Small without readiness");
        let observed = read(&store, 1_001).await.expect("saved Small choice");
        assert_eq!(observed.model_id.as_deref(), Some("small"));
        assert_eq!(observed.profile_id, saved.profile_id);
        let mut crosswired = request(&saved.revision, 1_001);
        crosswired.model_id = Some("base".into());
        crosswired.profile_id = saved.profile_id.clone();
        let error = save(&store, admin.id, crosswired, &languages(), 1_001)
            .await
            .expect_err("cross-model choice");
        assert_eq!(error.code(), Some("invalid_request"));
        assert_eq!(
            read(&store, 1_002)
                .await
                .expect("unchanged choice")
                .profile_id,
            saved.profile_id
        );
        let transcription = super::super::profile::default_for_task(
            plurx_core::store::subtitle_translation::SpeechTask::Transcribe,
        );
        let mut input = request(&saved.revision, 1_002);
        input.model_id = Some(transcription.model_id.clone());
        input.profile_id = Some(transcription.id().expect("transcription identity"));
        let saved = save(&store, admin.id, input, &languages(), 1_002)
            .await
            .expect("save Small transcription without readiness");
        assert_eq!(saved.model_id.as_deref(), Some("small"));
        assert_eq!(
            saved.profile_id,
            Some(transcription.id().expect("saved task identity"))
        );
        assert!(saved.enabled);
    }

    #[tokio::test]
    async fn stale_full_document_revision_refuses_and_preserves_the_saved_choice() {
        let store = SqliteStore::open_in_memory().expect("SQLite CAS fixture");
        let admin = store
            .create_user("admin", "hash", true)
            .await
            .expect("administrator");
        let initial = read(&store, 1_000).await.expect("virtual settings");
        save(
            &store,
            admin.id,
            request(&initial.revision, 1_000),
            &languages(),
            1_000,
        )
        .await
        .expect("first save");
        let before = store
            .get_setting(SETTINGS_KEY)
            .await
            .expect("saved raw document");
        let error = save(
            &store,
            admin.id,
            request(&initial.revision, 1_001),
            &languages(),
            1_001,
        )
        .await
        .expect_err("stale CAS refuses");
        assert_eq!(error.code(), Some("configuration_conflict"));
        assert_eq!(
            store
                .get_setting(SETTINGS_KEY)
                .await
                .expect("unchanged raw document"),
            before
        );
        assert!(read(&store, 1_002).await.expect("saved choice").enabled);
    }

    #[tokio::test]
    async fn malformed_stored_document_is_visible_and_nonadministrator_cannot_save() {
        let store = SqliteStore::open_in_memory().expect("SQLite permission fixture");
        let viewer = store
            .create_user("viewer", "hash", false)
            .await
            .expect("viewer");
        let initial = read(&store, 1_000).await.expect("virtual settings");
        let error = save(
            &store,
            viewer.id,
            request(&initial.revision, 1_000),
            &languages(),
            1_000,
        )
        .await
        .expect_err("current stored administrator authority required");
        assert_eq!(error.code(), Some("permission_denied"));
        assert!(store
            .get_setting(SETTINGS_KEY)
            .await
            .expect("no mutation")
            .is_none());
        store
            .put_settings(&[(SETTINGS_KEY, "{\"enabled\":true,\"unknown\":1}")])
            .await
            .expect("external malformed writer fixture");
        let error = read(&store, 1_001)
            .await
            .expect_err("malformed settings cannot become defaults");
        assert_eq!(error.code(), Some("configuration_conflict"));
    }

    #[test]
    fn settings_request_is_closed_and_never_accepts_advisory_or_path_fields() {
        let mut value = json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),"intent_expires_at_ms":61000,"expected_revision":"0.sha","enabled":true,"model_id":null,"profile_id":null,"preferred_spoken_language":null});
        assert!(serde_json::from_value::<UpdateRequest>(value.clone()).is_ok());
        let mut missing = value.clone();
        missing
            .as_object_mut()
            .expect("request object")
            .remove("model_id");
        assert!(serde_json::from_value::<UpdateRequest>(missing).is_err());
        value["model_path"] = Value::String("/private/model".into());
        assert!(serde_json::from_value::<UpdateRequest>(value).is_err());
    }
}
