//! Offline caption publication shares one transaction with durable job success.
//! Existing acquired-caption storage is reused, with a separate local namespace.
use super::background_jobs::{decode, encode, JobPayload, JobPublishOutcome, JobToken, QueueSql};
use crate::{domain::DownloadedSubtitle, error::StoreError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn valid_language(value: &str) -> bool {
    (2..=3).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_lowercase())
}

pub fn artifact_key(payload: &JobPayload) -> Result<String, StoreError> {
    payload.validate()?;
    if !matches!(payload, JobPayload::SubtitleTranscribe { .. }) {
        return Err(StoreError::Task("not a transcription payload".into()));
    }
    Ok(hex::encode(Sha256::digest(encode(payload)?)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishTranscription {
    pub token: JobToken,
    pub source: JobPayload,
    pub caption: DownloadedSubtitle,
    pub now_ms: i64,
}

const OWNS: &str = r#"
 job.id = json_extract($1,'$.token.job_id') AND job.kind = 'subtitle_transcribe' AND job.payload_version = 1
 AND job.state = 'running' AND job.owner_node_id = json_extract($1,'$.token.node_id')
 AND job.owner_boot_id = json_extract($1,'$.token.boot_id') AND job.claim_id = json_extract($1,'$.token.claim_id')
 AND job.fence = json_extract($1,'$.token.fence') AND job.revision = json_extract($1,'$.token.revision')
 AND job.revision < 9223372036854775807
 AND job.lease_expires_ms = json_extract($1,'$.token.lease_expires_ms') AND job.lease_expires_ms > json_extract($1,'$.now_ms')
 AND json_extract(job.payload_json,'$.file_id') = json_extract($1,'$.source.file_id')
 AND json_extract(job.payload_json,'$.source_size') = json_extract($1,'$.source.source_size')
 AND json_extract(job.payload_json,'$.source_mtime') = json_extract($1,'$.source.source_mtime')
 AND json_extract(job.payload_json,'$.language') = json_extract($1,'$.source.language')
 AND json_extract(job.payload_json,'$.model_sha256') = json_extract($1,'$.source.model_sha256')
 AND json_extract(job.payload_json,'$.pipeline_digest') = json_extract($1,'$.source.pipeline_digest')
 AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || job.owner_node_id)
 AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required WHERE required.job_id = job.id
   AND NOT EXISTS (SELECT 1 FROM background_job_reservations held WHERE held.job_id = job.id AND held.fence = job.fence
     AND held.resource_key = required.resource_key AND held.expires_at_ms >= job.lease_expires_ms))
 AND EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = job.id AND interest.state = 'pending'
   AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract($1,'$.now_ms')))
"#;

pub(super) async fn publish<T: QueueSql>(
    store: &T,
    input: PublishTranscription,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    let key = artifact_key(&input.source)?;
    let JobPayload::SubtitleTranscribe {
        file_id: _,
        source_size,
        source_mtime,
        language,
        model_sha256,
        pipeline_digest,
    } = &input.source
    else {
        unreachable!()
    };
    let provenance = input
        .caption
        .transcription
        .as_ref()
        .ok_or_else(|| StoreError::Task("missing local caption provenance".into()))?;
    if input.now_ms < 0
        || provenance.artifact_key != key
        || &provenance.model_sha256 != model_sha256
        || &provenance.pipeline_digest != pipeline_digest
        || input.caption.source_size != *source_size
        || input.caption.source_mtime != *source_mtime
        || &input.caption.language != language
    {
        return Err(StoreError::Task(
            "caption does not match its transcription source".into(),
        ));
    }
    super::downloaded_subtitles::encode(&input.caption)?;
    let body = encode(&input)?;
    let caption_exists = "EXISTS (SELECT 1 FROM json_each(file.downloaded_subtitles) caption WHERE json_extract(caption.value,'$.transcription.artifact_key') = json_extract($1,'$.caption.transcription.artifact_key') AND json_extract(caption.value,'$.source_size') = file.size AND json_extract(caption.value,'$.source_mtime') = file.mtime)";
    let source_current = "file.id = json_extract($1,'$.source.file_id') AND file.size = json_extract($1,'$.source.source_size') AND file.mtime = json_extract($1,'$.source.source_mtime') AND file.probe_json IS NOT NULL";
    store.queue_transaction(vec![
        (format!(r#"UPDATE files AS file SET downloaded_subtitles = json_insert(
         CASE WHEN json_extract(downloaded_subtitles,'$[0].source_size') = size AND json_extract(downloaded_subtitles,'$[0].source_mtime') = mtime
          THEN downloaded_subtitles ELSE '[]' END, '$[#]', json(json_extract($1,'$.caption')))
         WHERE {source_current} AND NOT {caption_exists}
          AND (json_extract(downloaded_subtitles,'$[0].source_size') IS NOT size OR json_extract(downloaded_subtitles,'$[0].source_mtime') IS NOT mtime OR json_array_length(downloaded_subtitles) < 8)
          AND EXISTS (SELECT 1 FROM background_jobs job WHERE {OWNS})"#), body.clone()),
        (format!(r#"UPDATE background_jobs AS job SET state = 'succeeded', result_ref = json_extract($1,'$.caption.transcription.artifact_key'),
          owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
          revision = revision + 1, updated_at_ms = json_extract($1,'$.now_ms')
          WHERE {OWNS} AND EXISTS (SELECT 1 FROM files file WHERE {source_current} AND {caption_exists})"#), body.clone()),
        (r#"UPDATE background_job_waiters SET state = CASE WHEN deadline_ms <= json_extract($1,'$.now_ms') THEN 'cancelled' ELSE 'succeeded' END, result_ref = json_extract($1,'$.caption.transcription.artifact_key'), updated_at_ms = json_extract($1,'$.now_ms')
          WHERE job_id = json_extract($1,'$.token.job_id') AND state = 'pending' AND EXISTS (SELECT 1 FROM background_jobs job
            JOIN background_job_attempts attempt ON attempt.job_id = job.id AND attempt.fence = job.fence
            WHERE job.id = background_job_waiters.job_id AND job.state = 'succeeded'
             AND job.result_ref = json_extract($1,'$.caption.transcription.artifact_key') AND attempt.claim_id = json_extract($1,'$.token.claim_id'))"#.into(), body.clone()),
    ]).await?;
    let rows = store.queue_sql(r#"SELECT CASE WHEN EXISTS (SELECT 1 FROM background_jobs job JOIN background_job_attempts attempt ON attempt.job_id = job.id AND attempt.fence = job.fence
      WHERE job.id = json_extract($1,'$.token.job_id') AND job.state = 'succeeded' AND job.result_ref = json_extract($1,'$.caption.transcription.artifact_key')
       AND attempt.fence = json_extract($1,'$.token.fence') AND attempt.claim_id = json_extract($1,'$.token.claim_id')
       AND attempt.owner_node_id = json_extract($1,'$.token.node_id') AND attempt.owner_boot_id = json_extract($1,'$.token.boot_id'))
      THEN json_object('outcome','already_published','job_id',json_extract($1,'$.token.job_id'),'result_ref',json_extract($1,'$.caption.transcription.artifact_key'))
      ELSE json_object('outcome','lost_ownership') END AS result_json"#.into(), body, false, true).await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing transcription publication verdict".into()))?,
    )
}
