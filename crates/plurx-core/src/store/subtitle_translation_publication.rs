//! Source binding and immutable segment publication share durable job settlement.
use super::background_jobs::{decode, encode, JobPayload, JobToken, QueueSql};
use super::subtitle_translation::{RangeRecipe, SegmentDraft};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindTranslationSource {
    pub token: JobToken,
    pub source: JobPayload,
    pub source_sha256: String,
    pub local_identity: String,
    pub now_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishTranslationSegment {
    pub token: JobToken,
    pub source: JobPayload,
    pub segment: SegmentDraft,
    pub now_ms: i64,
}
// Callers revalidate held/named source identity immediately before this
// transaction. SQL then fences authoritative revision, demand and root epoch.
const OWNS: &str = r#"job.id=json_extract($1,'$.token.job_id') AND job.state='running'
 AND job.payload_version=1 AND job.payload_json=json(json_extract($1,'$.source'))
 AND job.owner_node_id=json_extract($1,'$.token.node_id')
 AND job.owner_boot_id=json_extract($1,'$.token.boot_id')
 AND job.claim_id=json_extract($1,'$.token.claim_id')
 AND job.fence=json_extract($1,'$.token.fence') AND job.revision=json_extract($1,'$.token.revision')
 AND job.lease_expires_ms=json_extract($1,'$.token.lease_expires_ms')
 AND job.lease_expires_ms>json_extract($1,'$.now_ms') AND job.revision<9223372036854775807
 AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||job.owner_node_id)
 AND NOT EXISTS(SELECT 1 FROM background_job_required_resources required WHERE required.job_id=job.id
  AND NOT EXISTS(SELECT 1 FROM background_job_reservations held WHERE held.job_id=job.id
   AND held.fence=job.fence AND held.resource_key=required.resource_key AND held.expires_at_ms>=job.lease_expires_ms))
 AND EXISTS(SELECT 1 FROM background_job_waiters w WHERE w.job_id=job.id AND w.state='pending'
  AND (w.deadline_ms IS NULL OR w.deadline_ms>json_extract($1,'$.now_ms')))"#;
const CURRENT: &str = r#"op.id=json_extract($1,'$.recipe.operation_id')
 AND op.operation_epoch=json_extract($1,'$.recipe.operation_epoch') AND op.stopped=0
 AND op.state NOT IN ('stopped','failed')
 AND op.target_node_id=json_extract($1,'$.token.node_id')
 AND op.target_node_id=json_extract($1,'$.source.target_node_id')
 AND (json_extract($1,'$.source.kind')='subtitle_translation_prepare'
  OR op.preparation_job_id=json_extract($1,'$.source.producer_preparation_job_id'))
 AND op.file_id=json_extract($1,'$.recipe.source.file_id')
 AND op.source_json=json(json_extract($1,'$.recipe.source'))
 AND op.profile_json=json(json_extract($1,'$.recipe.profile'))
 AND op.audio_index=json_extract($1,'$.recipe.audio_index') AND op.spoken_language=json_extract($1,'$.recipe.spoken_language')
 AND op.duration_ms=json_extract($1,'$.recipe.duration_ms')
 AND EXISTS(SELECT 1 FROM files f WHERE f.id=op.file_id AND f.size=json_extract($1,'$.recipe.source.source_size')
  AND f.mtime=json_extract($1,'$.recipe.source.source_mtime') AND f.probe_json IS NOT NULL)
 AND EXISTS(SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id
  WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch AND d.released=0
  AND d.expires_at_ms>json_extract($1,'$.now_ms') AND u.is_admin=1)"#;
// Unique (asset,core) rows with valid bounds and accepted snapshot revisions
// prove every expected core is readable. A range control row or cached count
// alone does not: neither establishes immutable bytes in the visible manifest.
const READABLE_COMPLETE: &str = r#"EXISTS(SELECT 1 FROM subtitle_translation_assets a
 WHERE a.id=json_extract($1,'$.recipe.asset_id') AND a.operation_id=json_extract($1,'$.recipe.operation_id')
 AND a.file_id=json_extract($1,'$.recipe.source.file_id') AND a.profile_id=json_extract($1,'$.profile_id')
 AND a.retired_at_ms IS NULL AND a.source_json=json(json_extract($1,'$.recipe.source'))
 AND a.source_sha256=json_extract($1,'$.recipe.source_sha256') AND a.profile_json=json(json_extract($1,'$.recipe.profile'))
 AND a.audio_index=json_extract($1,'$.recipe.audio_index') AND a.spoken_language=json_extract($1,'$.recipe.spoken_language')
 AND a.duration_ms=json_extract($1,'$.recipe.duration_ms')
 AND a.core_count=(a.duration_ms+json_extract(a.profile_json,'$.core_ms')-1)/json_extract(a.profile_json,'$.core_ms')
 AND a.core_count=(SELECT count(*) FROM subtitle_translation_segments s WHERE s.asset_id=a.id
  AND s.core_index>=0 AND s.core_index<a.core_count
  AND s.start_ms=s.core_index*json_extract(a.profile_json,'$.core_ms')
  AND s.end_ms=min((s.core_index+1)*json_extract(a.profile_json,'$.core_ms'),a.duration_ms)
  AND s.accepted_revision>0 AND s.accepted_revision<=a.manifest_revision))"#;
fn operation_progress() -> String {
    format!("state=CASE WHEN {READABLE_COMPLETE} THEN 'complete' ELSE 'partial' END,reason=CASE WHEN {READABLE_COMPLETE} THEN NULL ELSE reason END,retryable=CASE WHEN {READABLE_COMPLETE} THEN 0 ELSE retryable END")
}
fn body_size(body: &str) -> Result<(), StoreError> {
    if body.len() > super::subtitle_translation::MAX_TRANSLATION_COMMAND_BYTES {
        return Err(StoreError::Task("translation command too large".into()));
    }
    Ok(())
}
async fn settled<T: QueueSql>(store: &T, body: String) -> Result<bool, StoreError> {
    let rows=store.queue_sql(r#"SELECT json(CASE WHEN EXISTS(SELECT 1 FROM background_jobs j JOIN background_job_attempts a ON a.job_id=j.id AND a.fence=j.fence
 WHERE j.id=json_extract($1,'$.token.job_id') AND j.state='succeeded' AND j.result_ref=json_extract($1,'$.result_ref')
 AND a.claim_id=json_extract($1,'$.token.claim_id') AND a.owner_node_id=json_extract($1,'$.token.node_id')
 AND a.owner_boot_id=json_extract($1,'$.token.boot_id') AND a.fence=json_extract($1,'$.token.fence')) THEN 'true' ELSE 'false' END) AS result_json"#.into(),body,false,true).await?;
    rows.first()
        .map(|r| decode(r))
        .transpose()
        .map(|r| r.unwrap_or(false))
}
fn continuation() -> String {
    let interest = super::subtitle_translation_read::range_interest(
        "json_extract($1,'$.source')",
        "json_extract($1,'$.token.job_id')",
        "json_extract($1,'$.token.fence')",
        "json_extract($1,'$.now_ms')",
    );
    format!("(json_extract($1,'$.source.kind')='subtitle_translation_prepare' OR {interest})")
}
fn success_sql() -> String {
    let continuation = continuation();
    format!(
        r#"UPDATE background_jobs AS job SET state='succeeded',result_ref=json_extract($1,'$.result_ref'),
 owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL,revision=revision+1,updated_at_ms=json_extract($1,'$.now_ms')
 WHERE {OWNS} AND {continuation} AND EXISTS(SELECT 1 FROM subtitle_translation_operations op WHERE {CURRENT} AND op.asset_id=json_extract($1,'$.recipe.asset_id'))
 AND EXISTS(SELECT 1 FROM subtitle_translation_assets a WHERE a.id=json_extract($1,'$.recipe.asset_id') AND a.operation_id=json_extract($1,'$.recipe.operation_id') AND a.retired_at_ms IS NULL
  AND a.source_sha256=json_extract($1,'$.recipe.source_sha256'))
 AND (job.kind='subtitle_translation_prepare' OR EXISTS(SELECT 1 FROM subtitle_translation_segments s
  WHERE s.asset_id=json_extract($1,'$.recipe.asset_id') AND s.core_index=json_extract($1,'$.recipe.core_index')
  ))
"#
    )
}
const SETTLE_WAITERS: &str = r#"UPDATE background_job_waiters SET state='succeeded',result_ref=json_extract($1,'$.result_ref'),updated_at_ms=json_extract($1,'$.now_ms')
 WHERE job_id=json_extract($1,'$.token.job_id') AND state='pending'
 AND EXISTS(SELECT 1 FROM background_jobs j WHERE j.id=background_job_waiters.job_id AND j.state='succeeded'
 AND j.result_ref=json_extract($1,'$.result_ref') AND j.fence=json_extract($1,'$.token.fence'))"#;

pub(super) async fn bind<T: QueueSql>(
    store: &T,
    input: BindTranslationSource,
) -> Result<bool, StoreError> {
    input.token.validate()?;
    input.source.validate()?;
    let JobPayload::SubtitleTranslationPrepare {
        file_id,
        source_size,
        source_mtime,
        operation_id,
        operation_epoch,
        profile,
        audio_index,
        spoken_language,
        duration_ms,
        ..
    } = &input.source
    else {
        return Err(StoreError::Task("not a translation preparation".into()));
    };
    if !super::subtitle_translation::valid_digest(&input.source_sha256)
        || input.local_identity.is_empty()
        || input.local_identity.len() > 256
        || input.now_ms < 0
    {
        return Err(StoreError::Task("invalid translation source proof".into()));
    }
    let mut recipe = RangeRecipe {
        asset_id: String::new(),
        operation_id: operation_id.clone(),
        operation_epoch: *operation_epoch,
        source: super::subtitle_translation::SourceRevision {
            file_id: *file_id,
            source_size: *source_size,
            source_mtime: *source_mtime,
        },
        source_sha256: input.source_sha256.clone(),
        profile: profile.clone(),
        audio_index: *audio_index,
        spoken_language: spoken_language.clone(),
        duration_ms: *duration_ms,
        core_index: 0,
    };
    recipe.asset_id = recipe.expected_asset_id()?;
    recipe.validate()?;
    let body = encode(
        &json!({"token":input.token,"source":input.source,"recipe":recipe,"local_identity":input.local_identity,"profile_id":profile.id()?,"core_count":recipe.core_count(),"result_ref":recipe.asset_id,"now_ms":input.now_ms}),
    )?;
    body_size(&body)?;
    let progress = operation_progress();
    let guard=format!("EXISTS(SELECT 1 FROM background_jobs job WHERE {OWNS}) AND EXISTS(SELECT 1 FROM subtitle_translation_operations op WHERE {CURRENT} AND op.preparation_job_id=json_extract($1,'$.token.job_id') AND op.view_revision<9223372036854775807 AND (op.asset_id IS NULL OR op.asset_id=json_extract($1,'$.recipe.asset_id')) AND NOT EXISTS(SELECT 1 FROM subtitle_translation_source_bindings b WHERE b.operation_id=op.id AND b.operation_epoch=op.operation_epoch AND b.source_sha256!=json_extract($1,'$.recipe.source_sha256')))");
    store.queue_transaction(vec![
 (format!(r#"INSERT INTO subtitle_translation_assets(id,operation_id,file_id,source_json,source_sha256,profile_json,profile_id,audio_index,spoken_language,duration_ms,core_count,created_at_ms,last_read_at_ms)
 SELECT json_extract($1,'$.recipe.asset_id'),json_extract($1,'$.recipe.operation_id'),json_extract($1,'$.recipe.source.file_id'),json(json_extract($1,'$.recipe.source')),json_extract($1,'$.recipe.source_sha256'),json(json_extract($1,'$.recipe.profile')),json_extract($1,'$.profile_id'),json_extract($1,'$.recipe.audio_index'),json_extract($1,'$.recipe.spoken_language'),json_extract($1,'$.recipe.duration_ms'),json_extract($1,'$.core_count'),json_extract($1,'$.now_ms'),json_extract($1,'$.now_ms') WHERE {guard} AND (SELECT count(*) FROM subtitle_translation_assets WHERE retired_at_ms IS NULL)<128 ON CONFLICT(id) DO UPDATE SET retired_at_ms=NULL,last_read_at_ms=excluded.last_read_at_ms WHERE subtitle_translation_assets.retired_at_ms IS NOT NULL AND subtitle_translation_assets.operation_id=excluded.operation_id AND subtitle_translation_assets.source_json=excluded.source_json AND subtitle_translation_assets.source_sha256=excluded.source_sha256 AND subtitle_translation_assets.profile_json=excluded.profile_json"#),body.clone()),
 (format!(r#"INSERT INTO subtitle_translation_source_bindings(operation_id,operation_epoch,node_id,boot_id,local_identity,source_sha256,verified_at_ms)
 SELECT json_extract($1,'$.recipe.operation_id'),json_extract($1,'$.recipe.operation_epoch'),json_extract($1,'$.token.node_id'),json_extract($1,'$.token.boot_id'),json_extract($1,'$.local_identity'),json_extract($1,'$.recipe.source_sha256'),json_extract($1,'$.now_ms') WHERE {guard} AND EXISTS(SELECT 1 FROM subtitle_translation_assets a WHERE a.id=json_extract($1,'$.recipe.asset_id') AND a.operation_id=json_extract($1,'$.recipe.operation_id') AND a.retired_at_ms IS NULL AND a.source_sha256=json_extract($1,'$.recipe.source_sha256'))
 ON CONFLICT(operation_id,operation_epoch,node_id) DO UPDATE SET boot_id=excluded.boot_id,local_identity=excluded.local_identity,source_sha256=excluded.source_sha256,verified_at_ms=excluded.verified_at_ms"#),body.clone()),
 (format!(r#"UPDATE subtitle_translation_operations AS op SET asset_id=json_extract($1,'$.recipe.asset_id'),{progress},view_revision=view_revision+1,updated_at_ms=json_extract($1,'$.now_ms')
 WHERE {CURRENT} AND op.view_revision<9223372036854775807 AND (op.asset_id IS NULL OR op.asset_id=json_extract($1,'$.recipe.asset_id')) AND NOT EXISTS(SELECT 1 FROM subtitle_translation_source_bindings b WHERE b.operation_id=op.id AND b.operation_epoch=op.operation_epoch AND b.source_sha256!=json_extract($1,'$.recipe.source_sha256')) AND op.preparation_job_id=json_extract($1,'$.token.job_id') AND EXISTS(SELECT 1 FROM subtitle_translation_assets a WHERE a.id=json_extract($1,'$.recipe.asset_id') AND a.operation_id=json_extract($1,'$.recipe.operation_id') AND a.retired_at_ms IS NULL) AND EXISTS(SELECT 1 FROM background_jobs job WHERE {OWNS})"#),body.clone()),
 (success_sql(),body.clone()),(SETTLE_WAITERS.into(),body.clone())]).await?;
    settled(store, body).await
}

pub(super) async fn publish<T: QueueSql>(
    store: &T,
    input: PublishTranslationSegment,
) -> Result<bool, StoreError> {
    input.token.validate()?;
    input.source.validate()?;
    let JobPayload::SubtitleTranslationRange { recipe, .. } = &input.source else {
        return Err(StoreError::Task("not a translation range".into()));
    };
    input.segment.validate(recipe)?;
    if input.segment.provenance.worker_fence != input.token.fence || input.now_ms < 0 {
        return Err(StoreError::Task(
            "translation execution fence mismatch".into(),
        ));
    }
    let bytes = input.segment.canonical_body()?;
    let segment_body = String::from_utf8(bytes.clone())
        .map_err(|_| StoreError::Task("translation UTF-8".into()))?;
    use sha2::{Digest, Sha256};
    let body = encode(
        &json!({"token":input.token,"source":input.source,"recipe":recipe,"segment_body":segment_body,"profile_id":recipe.profile.id()?,"publication_id":uuid::Uuid::new_v4().to_string(),"segment_id":input.segment.segment_id(),"segment_digest":hex::encode(Sha256::digest(bytes)),"result_ref":input.segment.segment_id(),"now_ms":input.now_ms}),
    )?;
    body_size(&body)?;
    let progress = operation_progress();
    let continuation = continuation();
    let guard = format!(
        r#"{continuation} AND EXISTS(SELECT 1 FROM background_jobs job WHERE {OWNS}) AND EXISTS(SELECT 1 FROM subtitle_translation_operations op WHERE {CURRENT}
 AND op.view_revision<9223372036854775807 AND op.asset_id=json_extract($1,'$.recipe.asset_id') AND EXISTS(SELECT 1 FROM subtitle_translation_source_bindings b
 WHERE b.operation_id=op.id AND b.operation_epoch=op.operation_epoch AND b.node_id=json_extract($1,'$.token.node_id')
 AND b.boot_id=json_extract($1,'$.token.boot_id') AND b.source_sha256=json_extract($1,'$.recipe.source_sha256'))
 AND EXISTS(SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch
 AND d.released=0 AND d.expires_at_ms>json_extract($1,'$.now_ms') AND u.is_admin=1))"#
    );
    store.queue_transaction(vec![
 (format!(r#"INSERT INTO subtitle_translation_segments(asset_id,core_index,segment_id,publication_id,digest,body,start_ms,end_ms,accepted_revision,created_at_ms)
 SELECT a.id,json_extract($1,'$.recipe.core_index'),json_extract($1,'$.segment_id'),json_extract($1,'$.publication_id'),json_extract($1,'$.segment_digest'),json_extract($1,'$.segment_body'),json_extract(json_extract($1,'$.segment_body'),'$.start_ms'),json_extract(json_extract($1,'$.segment_body'),'$.end_ms'),a.manifest_revision+1,json_extract($1,'$.now_ms')
 FROM subtitle_translation_assets a WHERE a.id=json_extract($1,'$.recipe.asset_id') AND a.operation_id=json_extract($1,'$.recipe.operation_id') AND a.retired_at_ms IS NULL AND a.manifest_revision<9223372036854775807
 AND (SELECT COALESCE(sum(accepted_bytes),0) FROM subtitle_translation_assets)+length(CAST(json_extract($1,'$.segment_body') AS BLOB))<=134217728 AND a.accepted_bytes+length(CAST(json_extract($1,'$.segment_body') AS BLOB))<=16777216 AND {guard}
 ON CONFLICT(asset_id,core_index) DO NOTHING"#),body.clone()),
 (r#"UPDATE subtitle_translation_assets SET manifest_revision=manifest_revision+1,
 accepted_bytes=accepted_bytes+length(CAST(json_extract($1,'$.segment_body') AS BLOB))
 WHERE id=json_extract($1,'$.recipe.asset_id') AND EXISTS(SELECT 1 FROM subtitle_translation_segments s WHERE s.asset_id=subtitle_translation_assets.id
 AND s.core_index=json_extract($1,'$.recipe.core_index') AND s.publication_id=json_extract($1,'$.publication_id') AND s.accepted_revision=subtitle_translation_assets.manifest_revision+1)"#.into(),body.clone()),
 (format!(r#"UPDATE subtitle_translation_operations AS op SET view_revision=view_revision+1,{progress},updated_at_ms=json_extract($1,'$.now_ms')
 WHERE {CURRENT} AND {continuation} AND EXISTS(SELECT 1 FROM background_jobs job WHERE {OWNS}) AND EXISTS(SELECT 1 FROM subtitle_translation_segments s WHERE s.asset_id=op.asset_id AND s.core_index=json_extract($1,'$.recipe.core_index') AND s.publication_id=json_extract($1,'$.publication_id'))"#),body.clone()),
 (success_sql(),body.clone()),(SETTLE_WAITERS.into(),body.clone())]).await?;
    settled(store, body).await
}
