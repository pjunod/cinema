//! Saved preference CAS is independent of advisory model/worker readiness.
use super::background_jobs::{decode, encode, QueueSql};
use super::subtitle_translation::{digest, valid_digest};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
pub const SETTINGS_KEY: &str = "subtitles.translation.config.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationSettings {
    pub version: u8,
    pub revision: String,
    pub enabled: bool,
    pub model_id: Option<String>,
    pub profile_id: Option<String>,
    pub preferred_spoken_language: Option<String>,
}
impl Default for TranslationSettings {
    fn default() -> Self {
        Self {
            version: 1,
            revision: "0".into(),
            enabled: false,
            model_id: None,
            profile_id: None,
            preferred_spoken_language: None,
        }
    }
}
impl TranslationSettings {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1
            || (self.revision != "0" && uuid::Uuid::parse_str(&self.revision).is_err())
            || self.model_id.as_deref().is_some_and(|v| {
                v.is_empty()
                    || v.len() > 64
                    || !v
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            })
            || self.profile_id.as_deref().is_some_and(|v| !valid_digest(v))
            || self
                .preferred_spoken_language
                .as_deref()
                .is_some_and(|v| !super::background_jobs_transcription::valid_language(v))
        {
            return Err(StoreError::Task("invalid translation settings".into()));
        }
        Ok(())
    }
}
pub fn settings_revision(raw: Option<&str>) -> Result<String, StoreError> {
    match raw {
        None => Ok(format!("0.{}", digest(&Value::Null)?)),
        Some(raw) => {
            if raw.len() > 8192 {
                return Err(StoreError::Task("translation settings conflict".into()));
            }
            let settings: TranslationSettings = serde_json::from_str(raw)
                .map_err(|_| StoreError::Task("translation settings conflict".into()))?;
            settings.validate()?;
            let value: Value = serde_json::from_str(raw)
                .map_err(|_| StoreError::Task("translation settings conflict".into()))?;
            Ok(format!("{}.{}", settings.revision, digest(&value)?))
        }
    }
}
#[derive(Debug, Clone)]
pub struct TranslationSettingsWrite {
    pub user_id: i64,
    pub request_id: String,
    pub request_digest: String,
    pub intent_expires_at_ms: i64,
    pub now_ms: i64,
    pub expected_revision: String,
    pub expected_raw: Option<String>,
    pub settings: TranslationSettings,
}
pub(super) async fn write<T: QueueSql>(
    store: &T,
    input: TranslationSettingsWrite,
) -> Result<Value, StoreError> {
    input.settings.validate()?;
    if !valid_digest(&input.request_digest)
        || input.user_id <= 0
        || uuid::Uuid::parse_str(&input.request_id).is_err()
        || input.settings.revision != input.request_id
        || input.expected_revision.len() > 128
        || input.now_ms < 0
        || input.now_ms.checked_add(604800000).is_none()
        || input.expected_raw.as_ref().is_some_and(|v| v.len() > 8192)
    {
        return Err(StoreError::Task(
            "invalid translation settings request".into(),
        ));
    }
    let expected_matches = settings_revision(input.expected_raw.as_deref())
        .is_ok_and(|v| v == input.expected_revision);
    let value = encode(&input.settings)?;
    let revision = settings_revision(Some(&value))?;
    let body = encode(
        &json!({"command_id":uuid::Uuid::new_v4().to_string(),"user_id":input.user_id,"request_id":input.request_id,"intent_expires_at_ms":input.intent_expires_at_ms,"now_ms":input.now_ms,"expected_raw":input.expected_raw,"cas_matches":expected_matches,"settings":input.settings,"revision":revision,"body_digest":input.request_digest}),
    )?;
    let sql = r#"WITH input AS MATERIALIZED(SELECT json($1) body),observed AS MATERIALIZED(SELECT body,(SELECT body_digest FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id') AND request_id=json_extract(body,'$.request_id')) previous_digest,(SELECT outcome_json FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id') AND request_id=json_extract(body,'$.request_id')) previous FROM input),verdict AS MATERIALIZED(SELECT *,CASE
 WHEN NOT EXISTS(SELECT 1 FROM users WHERE id=json_extract(body,'$.user_id') AND is_admin=1) THEN 'forbidden'
 WHEN previous IS NOT NULL AND previous_digest!=json_extract(body,'$.body_digest') THEN 'request_conflict'
 WHEN previous IS NOT NULL THEN 'replay'
 WHEN json_extract(body,'$.intent_expires_at_ms')<=json_extract(body,'$.now_ms') OR json_extract(body,'$.intent_expires_at_ms')>json_extract(body,'$.now_ms')+300000 THEN 'intent_expired'
 WHEN NOT json_extract(body,'$.cas_matches') OR (SELECT value FROM settings WHERE key='subtitles.translation.config.v1') IS NOT json_extract(body,'$.expected_raw') THEN 'configuration_conflict'
 WHEN (SELECT count(*) FROM subtitle_translation_requests)>=16352 OR (SELECT count(*) FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id'))>=1016 OR (SELECT COALESCE(sum(length(CAST(outcome_json AS BLOB))),0)+2048 FROM subtitle_translation_requests)>=8323072 OR (SELECT COALESCE(sum(length(CAST(outcome_json AS BLOB))),0)+2048 FROM subtitle_translation_requests WHERE user_id=json_extract(body,'$.user_id'))>=507904 THEN 'receipt_quota'
 ELSE 'saved' END outcome FROM observed)
 INSERT INTO subtitle_translation_commands(id,operation,request_json,result_json) SELECT json_extract(body,'$.command_id'),'settings',body,CASE outcome WHEN 'replay' THEN previous ELSE json_object('outcome',outcome,'settings',CASE WHEN outcome='saved' THEN json(json_extract(body,'$.settings')) ELSE NULL END,'revision',CASE WHEN outcome='saved' THEN json_extract(body,'$.revision') ELSE NULL END,'intent_expires_at_ms',json_extract(body,'$.intent_expires_at_ms'),'server_now_ms',json_extract(body,'$.now_ms')) END FROM verdict RETURNING result_json"#;
    store.queue_transaction(vec![("DELETE FROM subtitle_translation_requests WHERE rowid IN(SELECT rowid FROM subtitle_translation_requests WHERE expires_at_ms<json_extract($1,'$.now_ms') ORDER BY expires_at_ms LIMIT 128)".into(),body.clone()),(sql.into(),body.clone())]).await?;
    let rows=store.queue_sql("DELETE FROM subtitle_translation_commands WHERE id=json_extract($1,'$.command_id') RETURNING result_json".into(),body,true,true).await?;
    rows.first()
        .map(|r| decode(r))
        .transpose()?
        .ok_or_else(|| StoreError::Task("translation settings verdict missing".into()))
}
