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
    if !matches!(
        payload,
        JobPayload::SubtitleTranscribe { .. }
            | JobPayload::SubtitleTranscribeManaged { .. }
            | JobPayload::SubtitleTextTranslate { .. }
    ) {
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
 job.id = json_extract($1,'$.token.job_id') AND job.kind IN ('subtitle_transcribe','subtitle_transcribe_managed') AND job.payload_version = 1
 AND job.state = 'running' AND job.owner_node_id = json_extract($1,'$.token.node_id')
 AND job.owner_boot_id = json_extract($1,'$.token.boot_id') AND job.claim_id = json_extract($1,'$.token.claim_id')
 AND job.fence = json_extract($1,'$.token.fence') AND job.revision = json_extract($1,'$.token.revision')
 AND job.revision < 9223372036854775807
 AND job.lease_expires_ms = json_extract($1,'$.token.lease_expires_ms') AND job.lease_expires_ms > json_extract($1,'$.now_ms')
 AND job.payload_json = json(json_extract($1,'$.source'))
 AND json_extract(job.payload_json,'$.audio_index') = json_extract($1,'$.source.audio_index')
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
    if matches!(&input.source, JobPayload::SubtitleTextTranslate { .. }) {
        return publish_text(store, input).await;
    }
    input.token.validate()?;
    let key = artifact_key(&input.source)?;
    let (JobPayload::SubtitleTranscribe {
        file_id: _,
        audio_index,
        source_size,
        source_mtime,
        language,
        model_sha256,
        pipeline_digest,
    }
    | JobPayload::SubtitleTranscribeManaged {
        file_id: _,
        audio_index,
        source_size,
        source_mtime,
        language,
        model_sha256,
        pipeline_digest,
        ..
    }) = &input.source
    else {
        unreachable!()
    };
    let provenance = input
        .caption
        .transcription
        .as_ref()
        .ok_or_else(|| StoreError::Task("missing local caption provenance".into()))?;
    if input.now_ms < 0
        || provenance.audio_index != *audio_index
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
    if let JobPayload::SubtitleTranscribeManaged { profile, .. } = &input.source {
        if provenance.adapter != profile.adapter_contract
            || !super::downloaded_subtitles::valid_managed_provenance(provenance)
        {
            return Err(StoreError::Task(
                "caption does not match its managed execution provenance".into(),
            ));
        }
    } else if provenance.adapter != "whisper.cpp" || provenance.execution.is_some() {
        return Err(StoreError::Task(
            "caption does not match its legacy execution provenance".into(),
        ));
    }
    super::downloaded_subtitles::encode(&input.caption)?;
    let body = encode(
        &serde_json::to_value(&input).map_err(|error| StoreError::Task(error.to_string()))?,
    )?;
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

/// Existing text captions publish through the same durable queue transaction,
/// with their own provenance and explicit administrator interest.
async fn publish_text<T: QueueSql>(
    store: &T,
    input: PublishTranscription,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    let key = artifact_key(&input.source)?;
    let JobPayload::SubtitleTextTranslate {
        source_size,
        source_mtime,
        subtitle_index,
        input_sha256,
        source_language,
        target_language,
        model_id,
        model_sha256,
        adapter_contract,
        engine_sha256,
        ..
    } = &input.source
    else {
        unreachable!()
    };
    let origin = input
        .caption
        .text_translation
        .as_ref()
        .ok_or_else(|| StoreError::Task("missing text translation provenance".into()))?;
    if input.now_ms < 0
        || input.caption.transcription.is_some()
        || input.caption.source_size != *source_size
        || input.caption.source_mtime != *source_mtime
        || origin.artifact_key != key
        || origin.subtitle_index != *subtitle_index
        || origin.input_sha256 != *input_sha256
        || origin.source_language != *source_language
        || origin.target_language != *target_language
        || origin.model_id != *model_id
        || origin.model_sha256 != *model_sha256
        || origin.adapter_contract != *adapter_contract
        || origin.engine_sha256 != *engine_sha256
        || input.caption.language != *target_language
    {
        return Err(StoreError::Task(
            "caption does not match text translation input".into(),
        ));
    }
    super::downloaded_subtitles::encode(&input.caption)?;
    let body = encode(&input)?;
    let owns = format!("{SUBTITLE_OWNS} AND job.kind='subtitle_text_translate' AND job.payload_json=json(json_extract($1,'$.source')) AND EXISTS(SELECT 1 FROM users WHERE id=json_extract($1,'$.source.admin_user_id') AND is_admin=1) AND EXISTS(SELECT 1 FROM settings WHERE key='subtitles.text_translation.config.v1' AND CASE WHEN json_valid(value) THEN json_extract(value,'$.enabled')=1 ELSE 0 END)");
    let current = "file.id=json_extract($1,'$.source.file_id') AND file.size=json_extract($1,'$.source.source_size') AND file.mtime=json_extract($1,'$.source.source_mtime') AND file.probe_json IS NOT NULL";
    let exists = "EXISTS(SELECT 1 FROM json_each(file.downloaded_subtitles) caption WHERE json_extract(caption.value,'$.text_translation.artifact_key')=json_extract($1,'$.caption.text_translation.artifact_key') AND json_extract(caption.value,'$.source_size')=file.size AND json_extract(caption.value,'$.source_mtime')=file.mtime)";
    store.queue_transaction(vec![
        (format!("UPDATE files AS file SET downloaded_subtitles=json_insert(CASE WHEN json_extract(downloaded_subtitles,'$[0].source_size')=size AND json_extract(downloaded_subtitles,'$[0].source_mtime')=mtime THEN downloaded_subtitles ELSE '[]' END,'$[#]',json(json_extract($1,'$.caption'))) WHERE {current} AND NOT {exists} AND (json_extract(downloaded_subtitles,'$[0].source_size') IS NOT size OR json_extract(downloaded_subtitles,'$[0].source_mtime') IS NOT mtime OR json_array_length(downloaded_subtitles)<8) AND EXISTS(SELECT 1 FROM background_jobs job WHERE {owns})"), body.clone()),
        (format!("UPDATE background_jobs AS job SET state='succeeded',result_ref=json_extract($1,'$.caption.text_translation.artifact_key'),owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL,revision=revision+1,updated_at_ms=json_extract($1,'$.now_ms') WHERE {owns} AND EXISTS(SELECT 1 FROM files file WHERE {current} AND {exists})"),body.clone()),
        ("UPDATE background_job_waiters SET state='succeeded',result_ref=json_extract($1,'$.caption.text_translation.artifact_key'),updated_at_ms=json_extract($1,'$.now_ms') WHERE job_id=json_extract($1,'$.token.job_id') AND state='pending' AND EXISTS(SELECT 1 FROM background_jobs job WHERE job.id=background_job_waiters.job_id AND job.state='succeeded' AND job.result_ref=json_extract($1,'$.caption.text_translation.artifact_key'))".into(),body.clone()),
        ("DELETE FROM subtitle_text_translation_cues WHERE recipe_digest=json_extract($1,'$.caption.text_translation.artifact_key') AND EXISTS(SELECT 1 FROM background_jobs WHERE id=json_extract($1,'$.token.job_id') AND state='succeeded' AND result_ref=json_extract($1,'$.caption.text_translation.artifact_key'))".into(),body.clone())
    ]).await?;
    let rows=store.queue_sql("SELECT CASE WHEN EXISTS(SELECT 1 FROM background_jobs job JOIN background_job_attempts attempt ON attempt.job_id=job.id AND attempt.fence=job.fence WHERE job.id=json_extract($1,'$.token.job_id') AND job.state='succeeded' AND job.result_ref=json_extract($1,'$.caption.text_translation.artifact_key') AND attempt.claim_id=json_extract($1,'$.token.claim_id') AND attempt.owner_node_id=json_extract($1,'$.token.node_id') AND attempt.owner_boot_id=json_extract($1,'$.token.boot_id')) THEN json_object('outcome','already_published','job_id',json_extract($1,'$.token.job_id'),'result_ref',json_extract($1,'$.caption.text_translation.artifact_key')) ELSE json_object('outcome','lost_ownership') END AS result_json".into(),body,false,true).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()?
        .ok_or_else(|| StoreError::Task("missing text publication verdict".into()))
}

/// One adapter contract across qualified platform builds; local paths never enter it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProfile {
    pub model_id: String,
    pub catalog_revision: String,
    pub adapter_contract: String,
}
impl ManagedProfile {
    pub fn validate(&self) -> bool {
        super::background_jobs::identifier(&self.model_id)
            && super::background_jobs::identifier(&self.catalog_revision)
            && super::background_jobs::identifier(&self.adapter_contract)
    }
}

/// Exact whisper.cpp 1.9.2 language tokens (not ISO syntax alone).
pub const WHISPER_LANGUAGES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su", "yue",
];
pub fn whisper_language(value: &str) -> bool {
    WHISPER_LANGUAGES.contains(&value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAction {
    Install,
    Verify,
    TranslationInstall,
    TranslationVerify,
}

impl ModelAction {
    pub fn is_translation(self) -> bool {
        matches!(self, Self::TranslationInstall | Self::TranslationVerify)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitlePhase {
    CheckingSource,
    Downloading,
    WaitingForCapacity,
    Verifying,
    Decoding,
    Transcribing,
    Translating,
    Validating,
    Publishing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveSubtitleJob {
    pub token: JobToken,
    pub phase: SubtitlePhase,
    #[serde(default)]
    pub completed_cues: Option<u32>,
    #[serde(default)]
    pub total_cues: Option<u32>,
    pub now_ms: i64,
}

/// Immutable identity measured by a successful production self-test. A ready
/// worker must additionally re-open/revalidate these local identities live.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleModelReceipt {
    pub node_id: String,
    pub boot_id: String,
    pub model_id: String,
    pub model_sha256: String,
    pub adapter_contract: String,
    pub engine_sha256: String,
    pub model_file_identity: String,
    pub engine_file_identity: String,
    pub job_id: String,
    pub fence: i64,
    pub verified_at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationQualification {
    pub profile: super::subtitle_translation::TranslationProfile,
    pub self_test_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishSubtitleModel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<TranslationQualification>,
    pub token: JobToken,
    pub source: JobPayload,
    pub receipt: SubtitleModelReceipt,
    pub now_ms: i64,
}

// Observations must not increment the job revision: the owned renewal token
// remains exact. A stale claim/renewal cannot write into its successor.
pub(super) const SUBTITLE_OWNS: &str = r#"
 job.id=json_extract($1,'$.token.job_id') AND job.payload_version=1 AND job.state='running'
 AND job.owner_node_id=json_extract($1,'$.token.node_id')
 AND job.owner_boot_id=json_extract($1,'$.token.boot_id') AND job.claim_id=json_extract($1,'$.token.claim_id')
 AND job.revision<9223372036854775807
 AND job.fence=json_extract($1,'$.token.fence') AND job.revision=json_extract($1,'$.token.revision')
 AND job.lease_expires_ms=json_extract($1,'$.token.lease_expires_ms') AND job.lease_expires_ms>json_extract($1,'$.now_ms')
 AND NOT EXISTS (SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.' || job.owner_node_id)
 AND EXISTS (SELECT 1 FROM background_job_waiters waiter WHERE waiter.job_id=job.id AND waiter.state='pending'
   AND (waiter.deadline_ms IS NULL OR waiter.deadline_ms>json_extract($1,'$.now_ms')))
 AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required WHERE required.job_id=job.id
   AND NOT EXISTS (SELECT 1 FROM background_job_reservations held WHERE held.job_id=job.id AND held.fence=job.fence
     AND held.resource_key=required.resource_key AND held.expires_at_ms>=job.lease_expires_ms))
"#;

pub(super) async fn observe<T: QueueSql>(
    store: &T,
    input: ObserveSubtitleJob,
) -> Result<bool, StoreError> {
    input.token.validate()?;
    if input.now_ms < 0
        || input
            .completed_cues
            .zip(input.total_cues)
            .is_some_and(|(done, total)| done > total || total > 20000)
    {
        return Err(StoreError::Task(
            "invalid subtitle observation clock".into(),
        ));
    }
    let body = encode(
        &serde_json::to_value(&input).map_err(|error| StoreError::Task(error.to_string()))?,
    )?;
    let sql=format!("UPDATE background_jobs AS job SET checkpoint_json=json_object('phase',json_extract($1,'$.phase'),'phase_at_ms',json_extract($1,'$.now_ms'),'fence',fence,'completed_cues',json_extract($1,'$.completed_cues'),'total_cues',json_extract($1,'$.total_cues')) WHERE {SUBTITLE_OWNS} AND kind IN ('subtitle_transcribe','subtitle_transcribe_managed','subtitle_model_operation','subtitle_translation_prepare','subtitle_translation_range','subtitle_text_translate','subtitle_text_model_operation') RETURNING 'true' AS result_json");
    Ok(!store.queue_sql(sql, body, true, true).await?.is_empty())
}

pub(super) async fn publish_model<T: QueueSql>(
    store: &T,
    input: PublishSubtitleModel,
) -> Result<bool, StoreError> {
    input.token.validate()?;
    input.source.validate()?;
    let (model_id, model_sha256, adapter_contract, target_node_id, action, translation_profile) =
        match &input.source {
            JobPayload::SubtitleModelOperation {
                model_id,
                model_sha256,
                adapter_contract,
                target_node_id,
                action,
                translation_profile,
                ..
            } => (
                model_id,
                model_sha256,
                adapter_contract,
                target_node_id,
                action,
                translation_profile.as_ref(),
            ),
            JobPayload::SubtitleTextModelOperation {
                model_id,
                model_sha256,
                adapter_contract,
                target_node_id,
                action,
                ..
            } => (
                model_id,
                model_sha256,
                adapter_contract,
                target_node_id,
                action,
                None,
            ),
            _ => return Err(StoreError::Task("not a model operation".into())),
        };
    match (
        action.is_translation(),
        translation_profile,
        &input.translation,
    ) {
        (false, None, None) => {}
        (true, Some(profile), Some(qualification))
            if profile == &qualification.profile
                && profile.model_id == *model_id
                && profile.model_sha256 == *model_sha256
                && super::background_jobs::digest(&qualification.self_test_sha256) =>
        {
            profile.validate()?;
        }
        _ => {
            return Err(StoreError::Task(
                "translation qualification does not match model action".into(),
            ))
        }
    }
    let receipt = &input.receipt;
    if input.now_ms < 0
        || receipt.node_id != *target_node_id
        || receipt.node_id != input.token.node_id
        || receipt.boot_id != input.token.boot_id
        || receipt.job_id != input.token.job_id
        || receipt.fence != input.token.fence
        || receipt.model_id != *model_id
        || receipt.model_sha256 != *model_sha256
        || receipt.adapter_contract != *adapter_contract
        || receipt.verified_at_ms < 0
        || receipt.verified_at_ms > input.now_ms
        || !super::background_jobs::digest(&receipt.engine_sha256)
        || receipt.model_file_identity.is_empty()
        || receipt.model_file_identity.len() > 1024
        || receipt.engine_file_identity.is_empty()
        || receipt.engine_file_identity.len() > 1024
    {
        return Err(StoreError::Task("model receipt identity mismatch".into()));
    }
    let mut value =
        serde_json::to_value(&input).map_err(|error| StoreError::Task(error.to_string()))?;
    if input.translation.is_none() {
        value["translation"] = serde_json::Value::Null;
    }
    if let Some(qualification) = &input.translation {
        value["translation"]["profile_id"] = qualification.profile.id()?.into();
    }
    let body = encode(&value)?;
    let owns=format!("{SUBTITLE_OWNS} AND job.kind IN ('subtitle_model_operation','subtitle_text_model_operation') AND job.payload_json=json(json_extract($1,'$.source')) AND (job.kind!='subtitle_text_model_operation' OR EXISTS(SELECT 1 FROM users WHERE id=json_extract($1,'$.source.admin_user_id') AND is_admin=1))");
    store.queue_transaction(vec![
      (format!("INSERT INTO subtitle_translation_capabilities(node_id,model_id,profile_id,receipt_json) SELECT json_extract($1,'$.receipt.node_id'),json_extract($1,'$.receipt.model_id'),json_extract($1,'$.translation.profile_id'),json_object('model',json(json_extract($1,'$.receipt')),'profile',json(json_extract($1,'$.translation.profile')),'profile_id',json_extract($1,'$.translation.profile_id'),'self_test_sha256',json_extract($1,'$.translation.self_test_sha256')) WHERE json_type($1,'$.translation')='object' AND EXISTS(SELECT 1 FROM background_jobs job WHERE {owns}) ON CONFLICT(node_id,model_id,profile_id) DO UPDATE SET receipt_json=excluded.receipt_json"),body.clone()),
      (format!("INSERT INTO subtitle_model_receipts(node_id,model_id,receipt_json) SELECT json_extract($1,'$.receipt.node_id'),json_extract($1,'$.receipt.model_id'),json(json_extract($1,'$.receipt')) WHERE json_type($1,'$.translation')='null' AND EXISTS(SELECT 1 FROM background_jobs job WHERE {owns}) ON CONFLICT(node_id,model_id) DO UPDATE SET receipt_json=excluded.receipt_json"),body.clone()),
      (format!("UPDATE background_jobs AS job SET state='succeeded',result_ref=json_extract($1,'$.receipt.model_sha256'),owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL,revision=revision+1,updated_at_ms=json_extract($1,'$.now_ms') WHERE {owns}"),body.clone()),
      ("UPDATE background_job_waiters SET state=CASE WHEN deadline_ms<=json_extract($1,'$.now_ms') THEN 'cancelled' ELSE 'succeeded' END,result_ref=json_extract($1,'$.receipt.model_sha256'),updated_at_ms=json_extract($1,'$.now_ms') WHERE job_id=json_extract($1,'$.token.job_id') AND state='pending' AND EXISTS(SELECT 1 FROM background_jobs job WHERE job.id=background_job_waiters.job_id AND job.state='succeeded' AND job.result_ref=json_extract($1,'$.receipt.model_sha256'))".into(),body.clone())
    ]).await?;
    let rows=store.queue_sql("SELECT 'true' AS result_json WHERE (json_type($1,'$.translation')='null' AND EXISTS(SELECT 1 FROM subtitle_model_receipts WHERE node_id=json_extract($1,'$.receipt.node_id') AND model_id=json_extract($1,'$.receipt.model_id') AND receipt_json=json(json_extract($1,'$.receipt')))) OR (json_type($1,'$.translation')='object' AND EXISTS(SELECT 1 FROM subtitle_translation_capabilities WHERE node_id=json_extract($1,'$.receipt.node_id') AND model_id=json_extract($1,'$.receipt.model_id') AND profile_id=json_extract($1,'$.translation.profile_id') AND json_extract(receipt_json,'$.model')=json(json_extract($1,'$.receipt'))))".into(),body,false,true).await?;
    Ok(!rows.is_empty())
}

pub(super) async fn model_receipts<T: QueueSql>(
    store: &T,
    node_id: &str,
) -> Result<Vec<SubtitleModelReceipt>, StoreError> {
    if !super::background_jobs::identifier(node_id) {
        return Err(StoreError::Task("invalid receipt node".into()));
    }
    store.queue_sql("SELECT receipt_json AS result_json FROM subtitle_model_receipts WHERE node_id=json_extract($1,'$.node_id') ORDER BY model_id LIMIT 16".into(),encode(&serde_json::json!({"node_id":node_id}))?,false,true).await?.iter().map(|row|decode(row)).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleConfigurationWrite {
    /// None is the explicit legacy administrator endpoint's last-writer-wins
    /// mode transition. New managed forms must supply their observed revision.
    pub expected_revision: Option<String>,
    /// Exact observed legacy bytes; None means observed absence for CAS.
    /// Only the explicit None-revision Advanced endpoint ignores this field.
    pub expected_legacy: Option<String>,
    pub revision: String,
    pub managed: serde_json::Value,
    pub legacy: serde_json::Value,
    pub now_ms: i64,
}
pub(super) async fn write_configuration<T: QueueSql>(
    store: &T,
    input: SubtitleConfigurationWrite,
) -> Result<bool, StoreError> {
    if uuid::Uuid::parse_str(&input.revision).is_err()
        || input
            .expected_revision
            .as_ref()
            .is_some_and(|v| v != "0" && uuid::Uuid::parse_str(v).is_err())
        || (input.expected_revision.is_none()
            && input
                .managed
                .get("mode")
                .and_then(serde_json::Value::as_str)
                != Some("advanced"))
        || input
            .expected_legacy
            .as_ref()
            .is_some_and(|raw| raw.len() > 64 * 1024)
        || !input.managed.is_object()
        || !input.legacy.is_object()
        || input.now_ms < 0
        || input
            .managed
            .get("revision")
            .and_then(serde_json::Value::as_str)
            != Some(input.revision.as_str())
        || input
            .managed
            .get("version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
        || !matches!(
            input
                .managed
                .get("mode")
                .and_then(serde_json::Value::as_str),
            Some("managed" | "advanced")
        )
    {
        return Err(StoreError::Task(
            "invalid subtitle configuration transition".into(),
        ));
    }
    let body = encode(&serde_json::to_value(input).map_err(|e| StoreError::Task(e.to_string()))?)?;
    if body.len() > 256 * 1024 {
        return Err(StoreError::Task(
            "subtitle configuration exceeds bound".into(),
        ));
    }
    let sql = r#"INSERT INTO background_job_commands(id,operation,request_json,result_json)
 SELECT json_extract($1,'$.revision'),'subtitle_managed_config',$1,
 CASE WHEN json_type($1,'$.expected_revision')='null'
 OR (COALESCE((SELECT CASE WHEN json_valid(value) THEN json_extract(value,'$.revision') END
   FROM settings WHERE key='subtitles.transcription.managed.v1'),'0')=json_extract($1,'$.expected_revision')
   AND (SELECT value FROM settings WHERE key='subtitles.transcription.config') IS json_extract($1,'$.expected_legacy'))
 OR (EXISTS(SELECT 1 FROM settings WHERE key='subtitles.transcription.managed.v1'
   AND value=json(json_extract($1,'$.managed')))
   AND EXISTS(SELECT 1 FROM settings WHERE key='subtitles.transcription.config'
     AND value=json(json_extract($1,'$.legacy')))) THEN 'true' ELSE 'false' END
 RETURNING result_json"#;
    let rows = store.queue_sql(sql.into(), body, true, true).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(|value| value.unwrap_or(false))
}
