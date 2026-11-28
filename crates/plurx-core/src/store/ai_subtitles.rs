//! AI subtitles: the per-user count of active work that bounds what one
//! non-admin may start. Admission authority itself lives in the queue SQL,
//! which reads `subtitles.ai.v1` (`who`, `translate_text`, `from_audio`).
use super::background_jobs::{decode, encode, QueueSql};
use crate::error::StoreError;
use serde_json::json;

/// The settings document every AI subtitle admission predicate reads.
pub const SETTINGS_KEY: &str = "subtitles.ai.v1";
/// Integer compare-and-set generation for [`SETTINGS_KEY`].
pub const SETTINGS_GENERATION_KEY: &str = "subtitles.ai.generation.v1";

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
    JOIN subtitle_translation_demands d ON d.operation_id=op.id AND d.operation_epoch=op.operation_epoch
    WHERE d.user_id=json_extract($1,'$.user_id') AND d.released=0
      AND d.expires_at_ms>json_extract($1,'$.now_ms')
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
