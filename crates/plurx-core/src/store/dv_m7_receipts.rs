//! Nonterminal M7 publication under the existing complete-title attempt.
use super::{
    background_jobs::{encode, JobToken, QueueSql},
    dv_title_jobs::TITLE_JOB_OWNERSHIP,
};
use crate::{
    error::StoreError,
    transcode::dv_m7_receipt::{
        M7Binding, M7Receipt, M7ReceiptError, M7SourceProbe, MAX_RECEIPT_BYTES,
        MAX_RECENT_RECEIPTS, MEASUREMENT_FIELDS, PROBE_KEY, SCHEMA,
    },
};
use serde_json::{json, Value};
use std::sync::LazyLock;

#[derive(Clone)]
pub struct RecordM7Receipt {
    pub token: JobToken,
    pub source_probe: M7SourceProbe,
    pub receipt: M7Receipt,
    pub now_ms: i64,
}

pub(super) async fn record<T: QueueSql>(
    store: &T,
    request: RecordM7Receipt,
) -> Result<bool, StoreError> {
    request.token.validate()?;
    let binding = request.receipt.binding();
    if request.now_ms < request.receipt.measured_at_ms()
        || request.source_probe.sha256() != binding.probe_sha256
    {
        return Err(StoreError::Task(
            "invalid M7 receipt publication source".into(),
        ));
    }
    let receipt = request.receipt.encode().map_err(receipt_error)?;
    let body = json!({
        "token": request.token,
        "output": source_binding(binding),
        "source_probe": request.source_probe.value(),
        "now_ms": request.now_ms,
        "binding_key": request.receipt.binding_key(),
        "receipt": receipt,
    });
    let rows = store
        .queue_sql(RECORD_SQL.clone(), encode(&body)?, true, true)
        .await?;
    Ok(!rows.is_empty())
}

pub(super) async fn lookup<T: QueueSql>(
    store: &T,
    binding: &M7Binding,
    source_probe: &M7SourceProbe,
) -> Result<Option<M7Receipt>, StoreError> {
    if source_probe.sha256() != binding.probe_sha256 {
        return Ok(None);
    }
    let key = binding.key().map_err(receipt_error)?;
    let body = json!({
        "output": source_binding(binding),
        "source_probe": source_probe.value(),
        "binding_key": key,
    });
    let rows = store
        .queue_sql(LOOKUP_SQL.clone(), encode(&body)?, false, true)
        .await?;
    let [encoded] = rows.as_slice() else {
        return Ok(None);
    };
    Ok(M7Receipt::decode(encoded, binding).ok())
}

fn receipt_error(error: M7ReceiptError) -> StoreError {
    StoreError::Task(error.to_string())
}

fn source_binding(binding: &M7Binding) -> Value {
    json!({
        "file_id": binding.file_id,
        "source_size": binding.source_size,
        "source_mtime": binding.source_mtime,
        "probe_revision": binding.probe_revision,
        "scanner_digest": binding.scanner_digest,
    })
}

// Compare the current ordinary probe structurally in the same statement as
// the write. JSON tree paths preserve array order while object member order
// is irrelevant. Both sides omit only the named application siblings.
static SOURCE_PROBE_MATCH: LazyLock<String> = LazyLock::new(|| {
    let removed = MEASUREMENT_FIELDS
        .map(|field| format!("'$.{field}'"))
        .join(",");
    let current = format!(
        "CASE WHEN json_valid(file.probe_json) THEN json_remove(file.probe_json,{removed}) ELSE '{{}}' END"
    );
    let observed = "json_extract($1,'$.source_probe')";
    format!(
        "NOT EXISTS(SELECT fullkey,type,atom FROM json_tree({current}) \
         EXCEPT SELECT fullkey,type,atom FROM json_tree({observed})) \
         AND NOT EXISTS(SELECT fullkey,type,atom FROM json_tree({observed}) \
         EXCEPT SELECT fullkey,type,atom FROM json_tree({current}))"
    )
});

const RECEIPT_OBJECT: &str = r#"
CASE WHEN json_valid(file.probe_json)
 THEN CASE WHEN json_type(file.probe_json,'$.__PROBE_KEY__')='object'
   THEN json_extract(file.probe_json,'$.__PROBE_KEY__') ELSE '{}' END
 ELSE '{}' END
"#;

const CURRENT_FILE: &str = r#"
file.id=json_extract($1,'$.output.file_id')
AND file.size=json_extract($1,'$.output.source_size')
AND file.mtime=json_extract($1,'$.output.source_mtime')
AND file.video_codec='hevc' AND file.dv_profile=7 AND file.dv_rpu_present=1
AND CASE WHEN json_valid(file.probe_json) THEN json_type(file.probe_json)='object' ELSE 0 END
AND __SOURCE_PROBE_MATCH__
"#;

fn substitute(sql: &str) -> String {
    sql.replace("__CURRENT_FILE__", CURRENT_FILE)
        .replace("__SOURCE_PROBE_MATCH__", SOURCE_PROBE_MATCH.as_str())
        .replace("__RECEIPT_OBJECT__", RECEIPT_OBJECT)
        .replace("__PROBE_KEY__", PROBE_KEY)
        .replace("__RECEIPT_BYTES__", &MAX_RECEIPT_BYTES.to_string())
        .replace("__RECENT_RECEIPTS__", &MAX_RECENT_RECEIPTS.to_string())
}

const MAINTENANCE_AUTHORITY: &str = r#"
WITH input AS (SELECT json($1) AS body), owned AS (
 SELECT job.id FROM input JOIN background_jobs job ON job.id=json_extract(body,'$.token.job_id')
 WHERE __TITLE_JOB_OWNS__
 AND EXISTS(SELECT 1 FROM background_job_waiters w WHERE w.job_id=job.id
  AND w.consumer_kind='dv_title_scan' AND w.state='pending'
  AND (w.deadline_ms IS NULL OR w.deadline_ms>json_extract(body,'$.now_ms')))
 AND NOT EXISTS(SELECT 1 FROM background_job_waiters w WHERE w.job_id=job.id
  AND w.consumer_kind='playback_qualification' AND w.state IN ('pending','awaiting_hydration')
  AND w.deadline_ms>json_extract(body,'$.now_ms'))
)
"#;

/// Share the exact live title and current source guards between advisory
/// measurement publication and irreversible candidate reservations.
pub(super) fn fenced_maintenance_sql(sql: &str) -> String {
    sql.replace("__M7_AUTHORITY__", MAINTENANCE_AUTHORITY)
        .replace("__TITLE_JOB_OWNS__", TITLE_JOB_OWNERSHIP)
        .replace("__CURRENT_FILE__", CURRENT_FILE)
        .replace("__SOURCE_PROBE_MATCH__", SOURCE_PROBE_MATCH.as_str())
}

static RECORD_SQL: LazyLock<String> = LazyLock::new(|| {
    substitute(&fenced_maintenance_sql(
        r#"
__M7_AUTHORITY__
UPDATE files AS file SET probe_json=json_set(file.probe_json,'$.__PROBE_KEY__',json_set(
 COALESCE((SELECT json_group_object(key,json(value)) FROM (
  SELECT key,value FROM json_each(__RECEIPT_OBJECT__)
  WHERE key<>json_extract($1,'$.binding_key') AND type='object'
  AND length(key)=64 AND key NOT GLOB '*[^0-9a-f]*'
  AND length(CAST(value AS BLOB))<=__RECEIPT_BYTES__
  AND json_extract(value,'$.schema')=__SCHEMA__
  AND json_extract(value,'$.binding_key')=key
  ORDER BY json_extract(value,'$.payload.measured_at_ms') DESC,key LIMIT __PRIOR_LIMIT__
 )), '{}'),
 '$.'||json_extract($1,'$.binding_key'),json(json_extract($1,'$.receipt'))))
WHERE EXISTS(SELECT 1 FROM owned) AND __CURRENT_FILE__
RETURNING 'true' AS result_json
"#,
    ))
    .replace("__SCHEMA__", &SCHEMA.to_string())
    .replace("__PRIOR_LIMIT__", &(MAX_RECENT_RECEIPTS - 1).to_string())
});

static LOOKUP_SQL: LazyLock<String> = LazyLock::new(|| {
    substitute(
        r#"
SELECT receipt.value AS result_json
FROM files file
JOIN dv_title_jobs_demand demand ON demand.file_id=file.id
JOIN json_each(__RECEIPT_OBJECT__) receipt
WHERE __CURRENT_FILE__
AND demand.revision=json_extract($1,'$.output.probe_revision')
AND demand.scanner_digest=json_extract($1,'$.output.scanner_digest')
AND receipt.key=json_extract($1,'$.binding_key') AND receipt.type='object'
AND length(CAST(receipt.value AS BLOB))<=__RECEIPT_BYTES__
AND (SELECT count(*) FROM json_each(__RECEIPT_OBJECT__))<=__RECENT_RECEIPTS__
LIMIT 2
"#,
    )
});
