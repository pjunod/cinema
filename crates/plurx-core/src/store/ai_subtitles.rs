//! AI subtitles: the per-user count of active work that bounds what one
//! non-admin may start. Admission authority itself lives in the queue SQL,
//! which reads `subtitles.ai.v1` (`who`, `translate_text`, `from_audio`).
use super::background_jobs::{decode, encode, QueueSql};
use super::subtitle_translation::SourceRevision;
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The settings document every AI subtitle admission predicate reads.
pub const SETTINGS_KEY: &str = "subtitles.ai.v1";
/// Integer compare-and-set generation for [`SETTINGS_KEY`].
pub const SETTINGS_GENERATION_KEY: &str = "subtitles.ai.generation.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IntentRoute {
    Text,
    Audio,
}

/// Durable receipt reserved before submitting the selected producer.
/// The producer receives `id` as its own idempotency key.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Intent {
    pub id: String,
    pub digest: String,
    pub source: SourceRevision,
    pub route: IntentRoute,
    pub track_index: i64,
    pub language: Option<String>,
    pub playback_id: Option<String>,
    pub position_ms: i64,
    pub issued_at_ms: i64,
    pub producer_id: Option<String>,
}

pub enum IntentAccess {
    Request {
        request_id: String,
    },
    Operation {
        id: String,
    },
    Latest {
        source: SourceRevision,
    },
    Reserve {
        request_id: String,
        intent: Intent,
    },
    Link {
        request_id: String,
        id: String,
        producer_id: String,
    },
}

pub struct Viewer {
    pub operation_id: String,
    pub playback_id: String,
    pub user_id: i64,
    pub now_ms: i64,
    /// None releases only this user and playback.
    pub window: Option<(i64, f64)>,
}

pub(super) async fn viewer<T: QueueSql>(store: &T, viewer: Viewer) -> Result<bool, StoreError> {
    let invalid = || StoreError::Task("invalid AI subtitle viewer".into());
    if viewer.user_id <= 0
        || viewer.now_ms < 0
        || viewer.now_ms.checked_add(15_000).is_none()
        || uuid::Uuid::parse_str(&viewer.operation_id).is_err()
        || uuid::Uuid::parse_str(&viewer.playback_id).is_err()
        || viewer.window.is_some_and(|(position, rate)| {
            !(0..=14_400_000).contains(&position) || !rate.is_finite() || rate <= 0.0 || rate > 4.0
        })
    {
        return Err(invalid());
    }
    let (position, rate) = viewer.window.unwrap_or((0, 1.0));
    let body = encode(&json!({
        "operation":viewer.operation_id, "playback":viewer.playback_id,
        "user":viewer.user_id, "now":viewer.now_ms,"position":position,"rate":rate,
        "id":uuid::Uuid::new_v4().to_string(),"continuity":uuid::Uuid::new_v4().to_string(),
    }))?;
    let sql = if viewer.window.is_some() {
        r#"INSERT INTO subtitle_translation_demands(id,operation_id,operation_epoch,user_id,playback_id,generation,position_ms,playback_rate,paused,expires_at_ms,created_at_ms,updated_at_ms,continuity_id)
 SELECT json_extract($1,'$.id'),op.id,op.operation_epoch,user.id,json_extract($1,'$.playback'),1,
   min(op.duration_ms,json_extract($1,'$.position')),json_extract($1,'$.rate'),0,
   json_extract($1,'$.now')+15000,json_extract($1,'$.now'),json_extract($1,'$.now'),json_extract($1,'$.continuity')
 FROM subtitle_translation_operations op JOIN users user ON user.id=json_extract($1,'$.user')
 JOIN files f ON f.id=op.file_id AND f.size=json_extract(op.source_json,'$.source_size') AND f.mtime=json_extract(op.source_json,'$.source_mtime')
 JOIN settings config ON config.key='subtitles.ai.v1'
 WHERE op.id=json_extract($1,'$.operation') AND op.stopped=0 AND op.state NOT IN ('failed','complete') AND f.probe_json IS NOT NULL
  AND CASE WHEN json_valid(config.value) THEN json_extract(config.value,'$.version')=1
    AND json_type(config.value,'$.from_audio')='true'
    AND (user.is_admin=1 OR json_extract(config.value,'$.who')='everyone') ELSE 0 END
  AND (EXISTS(SELECT 1 FROM subtitle_translation_demands d WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch
       AND d.user_id=user.id AND d.playback_id=json_extract($1,'$.playback') AND d.released=0 AND d.expires_at_ms>json_extract($1,'$.now'))
   OR ((SELECT count(*) FROM subtitle_translation_demands d WHERE d.released=0 AND d.expires_at_ms>json_extract($1,'$.now'))<16
    AND (SELECT count(*) FROM subtitle_translation_demands d WHERE d.user_id=user.id AND d.released=0 AND d.expires_at_ms>json_extract($1,'$.now'))<4
    AND (SELECT count(*) FROM subtitle_translation_demands d WHERE d.operation_id=op.id AND d.released=0 AND d.expires_at_ms>json_extract($1,'$.now'))<4))
 ON CONFLICT(operation_id,operation_epoch,user_id,playback_id) DO UPDATE SET
  generation=generation+1,position_ms=excluded.position_ms,playback_rate=excluded.playback_rate,
  paused=0,expires_at_ms=excluded.expires_at_ms,updated_at_ms=excluded.updated_at_ms,released=0,
  continuity_id=CASE WHEN released=1 OR expires_at_ms<=excluded.updated_at_ms OR continuity_id IS NULL
    OR excluded.position_ms<position_ms-2000
    OR excluded.position_ms>position_ms+max(3000,(excluded.updated_at_ms-updated_at_ms)*playback_rate+1500)
    THEN excluded.continuity_id ELSE continuity_id END
 WHERE generation<9223372036854775807
 RETURNING 'true' AS result_json"#
    } else {
        r#"UPDATE subtitle_translation_demands SET released=1,generation=generation+1,updated_at_ms=json_extract($1,'$.now')
 WHERE operation_id=json_extract($1,'$.operation') AND user_id=json_extract($1,'$.user')
 AND playback_id=json_extract($1,'$.playback') AND released=0 AND generation<9223372036854775807
 RETURNING 'true' AS result_json"#
    };
    Ok(!store
        .queue_sql(sql.into(), body, true, true)
        .await?
        .is_empty())
}

pub(super) async fn delete_track<T: QueueSql>(
    store: &T,
    source: SourceRevision,
    track_id: &str,
) -> Result<(), StoreError> {
    source.validate()?;
    if !super::subtitle_translation::valid_digest(track_id) {
        return Err(StoreError::Task(
            "invalid AI subtitle track identity".into(),
        ));
    }
    let body = encode(&json!({"source":source,"track":track_id}))?;
    store.queue_transaction(vec![
        (r#"UPDATE files SET downloaded_subtitles=(SELECT json_group_array(json(caption)) FROM (
  SELECT value AS caption FROM json_each(files.downloaded_subtitles)
  WHERE json_extract(value,'$.transcription.artifact_key') IS NOT json_extract($1,'$.track')
    AND json_extract(value,'$.text_translation.artifact_key') IS NOT json_extract($1,'$.track')
  ORDER BY CAST(key AS INTEGER)
 ))
 WHERE id=json_extract($1,'$.source.file_id') AND size=json_extract($1,'$.source.source_size')
 AND mtime=json_extract($1,'$.source.source_mtime')"#.into(), body.clone()),
        (r#"DELETE FROM subtitle_translation_stored_tracks WHERE asset_id=json_extract($1,'$.track')
 AND file_id=json_extract($1,'$.source.file_id') AND source_size=json_extract($1,'$.source.source_size')
 AND source_mtime=json_extract($1,'$.source.source_mtime')
 AND EXISTS(SELECT 1 FROM files WHERE id=file_id AND size=source_size AND mtime=source_mtime)"#.into(), body),
    ]).await?;
    Ok(())
}

const INTENT_RECEIPT: &str = "json_extract(outcome_json,'$.kind')='ai_subtitle_intent'";
const READ: &str = "SELECT json_extract(outcome_json,'$.intent') AS result_json
 FROM subtitle_translation_requests
 WHERE user_id=json_extract($1,'$.user_id') AND expires_at_ms>json_extract($1,'$.now_ms')";

pub(super) async fn intent<T: QueueSql>(
    store: &T,
    user_id: i64,
    now_ms: i64,
    access: IntentAccess,
) -> Result<Option<Intent>, StoreError> {
    let invalid = || StoreError::Task("invalid AI subtitle intent".into());
    if user_id <= 0 || now_ms < 0 || now_ms.checked_add(604_800_000).is_none() {
        return Err(invalid());
    }
    let uuid = |value: &str| uuid::Uuid::parse_str(value).is_ok();
    let mut body = json!({"user_id":user_id,"now_ms":now_ms});
    let condition = match access {
        IntentAccess::Latest { source } => {
            source.validate()?;
            body["source"] = json!(source);
            "json_extract(outcome_json,'$.intent.source')=json(json_extract($1,'$.source'))
             AND json_type(outcome_json,'$.intent.producer_id')='text' ORDER BY created_at_ms DESC,request_id DESC LIMIT 1"
        }
        IntentAccess::Operation { id } if uuid(&id) => {
            body["id"] = id.into();
            "json_extract(outcome_json,'$.intent.id')=json_extract($1,'$.id')"
        }
        IntentAccess::Request { request_id } if uuid(&request_id) => {
            body["request_id"] = request_id.into();
            "request_id=json_extract($1,'$.request_id')"
        }
        IntentAccess::Reserve { request_id, intent }
            if uuid(&request_id)
                && uuid(&intent.id)
                && super::subtitle_translation::valid_digest(&intent.digest)
                && intent.issued_at_ms == now_ms
                && intent.producer_id.is_none()
                && (0..=255).contains(&intent.track_index)
                && (0..=14_400_000).contains(&intent.position_ms)
                && intent
                    .language
                    .as_ref()
                    .is_none_or(|language| language.len() <= 16)
                && intent.playback_id.as_deref().is_none_or(uuid) =>
        {
            intent.source.validate()?;
            body["request_id"] = request_id.into();
            body["intent"] = json!(intent);
            store.queue_sql(
                r#"INSERT INTO subtitle_translation_requests(user_id,request_id,body_digest,outcome_json,created_at_ms,expires_at_ms)
 SELECT json_extract($1,'$.user_id'),json_extract($1,'$.request_id'),json_extract($1,'$.intent.digest'),
   json_object('kind','ai_subtitle_intent','intent',json(json_extract($1,'$.intent'))),
   json_extract($1,'$.now_ms'),json_extract($1,'$.now_ms')+604800000
 WHERE EXISTS(SELECT 1 FROM files WHERE id=json_extract($1,'$.intent.source.file_id')
    AND size=json_extract($1,'$.intent.source.source_size') AND mtime=json_extract($1,'$.intent.source.source_mtime'))
   AND EXISTS(SELECT 1 FROM users WHERE id=json_extract($1,'$.user_id'))
   AND (SELECT count(*) FROM subtitle_translation_requests)<16352
   AND (SELECT count(*) FROM subtitle_translation_requests WHERE user_id=json_extract($1,'$.user_id'))<1016
   AND (SELECT COALESCE(sum(length(CAST(outcome_json AS BLOB))),0) FROM subtitle_translation_requests)+2048<8323072
   AND (SELECT COALESCE(sum(length(CAST(outcome_json AS BLOB))),0) FROM subtitle_translation_requests WHERE user_id=json_extract($1,'$.user_id'))+2048<507904
 ON CONFLICT(user_id,request_id) DO NOTHING"#.into(),
                encode(&body)?, true, false,
            ).await?;
            "request_id=json_extract($1,'$.request_id')"
        }
        IntentAccess::Link {
            request_id,
            id,
            producer_id,
        } if uuid(&request_id) && uuid(&id) && uuid(&producer_id) => {
            body["request_id"] = request_id.into();
            body["id"] = id.into();
            body["producer_id"] = producer_id.into();
            store.queue_sql(format!(
                "UPDATE subtitle_translation_requests
                 SET outcome_json=json_set(outcome_json,'$.intent.producer_id',json_extract($1,'$.producer_id'))
                 WHERE {INTENT_RECEIPT} AND user_id=json_extract($1,'$.user_id')
                 AND request_id=json_extract($1,'$.request_id')
                 AND json_extract(outcome_json,'$.intent.id')=json_extract($1,'$.id')
                 AND json_extract(outcome_json,'$.intent.producer_id') IS NULL"
            ), encode(&body)?, true, false).await?;
            "request_id=json_extract($1,'$.request_id') AND json_extract(outcome_json,'$.intent.producer_id')=json_extract($1,'$.producer_id')"
        }
        _ => return Err(invalid()),
    };
    let rows = store
        .queue_sql(
            format!("{READ} AND {INTENT_RECEIPT} AND {condition}"),
            encode(&body)?,
            false,
            true,
        )
        .await?;
    rows.first().map(|row| decode(row)).transpose()
}

/// Non-terminal AI subtitle work this user is waiting on: progressive
/// operations they hold a live demand on, plus text translation jobs they
/// requested. `exclude_operation_id` leaves out an operation the caller is
/// about to join, since joining shared work starts nothing new.
pub(super) async fn active_work<T: QueueSql>(
    store: &T,
    user_id: i64,
    exclude_operation_id: Option<&str>,
    now_ms: i64,
) -> Result<i64, StoreError> {
    if user_id <= 0 || now_ms < 0 || exclude_operation_id.is_some_and(|id| id.len() > 128) {
        return Err(StoreError::Task("invalid AI subtitle work query".into()));
    }
    let body = encode(&json!({
        "user_id": user_id,
        "scope": format!("subtitle-text-user:{user_id}"),
        "exclude": exclude_operation_id,
        "now_ms": now_ms,
    }))?;
    let rows = store
        .queue_sql(
            "SELECT json_object('count',
  (SELECT count(DISTINCT op.id) FROM subtitle_translation_operations op
    WHERE (EXISTS(SELECT 1 FROM subtitle_translation_demands d
      WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch
        AND d.user_id=json_extract($1,'$.user_id') AND d.released=0
        AND d.expires_at_ms>json_extract($1,'$.now_ms'))
      OR EXISTS(SELECT 1 FROM subtitle_translation_finish_demands finish
        WHERE finish.operation_id=op.id AND finish.requested_by=json_extract($1,'$.user_id')))
      AND op.stopped=0 AND op.state NOT IN ('failed','stopped','complete')
      AND op.id IS NOT json_extract($1,'$.exclude'))
  +(SELECT count(DISTINCT j.id) FROM background_job_waiters w JOIN background_jobs j ON j.id=w.job_id
    WHERE w.request_scope=json_extract($1,'$.scope')
      AND j.kind IN ('subtitle_text_translate','subtitle_text_translate_artifact')
      AND j.state IN ('queued','running','cancelling'))) AS result_json"
                .into(),
            body,
            false,
            true,
        )
        .await?;
    let value: serde_json::Value = rows
        .first()
        .map(|row| decode(row))
        .transpose()?
        .ok_or_else(|| StoreError::Task("AI subtitle work count missing".into()))?;
    value["count"]
        .as_i64()
        .ok_or_else(|| StoreError::Task("AI subtitle work count invalid".into()))
}
