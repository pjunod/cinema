//! Durable verification observations and finite repair plans. A repair tries
//! verified copies first, then at most one original typed rebuild and delivery.
use super::background_jobs::{
    decode, digest, encode, JobPayload, JobPublishOutcome, JobToken, QueueSql,
};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = include_str!("background_jobs_integrity.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 82;
pub const MAX_REPAIR_PLANS: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRepair {
    pub id: String,
    pub original_key: String,
    pub target_node_id: String,
    pub location_generation: String,
    pub artifact_key: String,
    pub producer_payload: Option<JobPayload>,
    pub phase: String,
    pub job_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub expires_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscodeVerificationCandidate {
    pub recipe_hash: String,
    pub manifest_digest: String,
    pub relative_dir: String,
    pub publication_generation: i64,
    pub scrub_object_index: i64,
}
pub(super) async fn transcode_candidates<T: QueueSql>(
    store: &T,
    node: &str,
    artifact_key: Option<&str>,
) -> Result<Vec<TranscodeVerificationCandidate>, StoreError> {
    let rows = store.queue_sql(r#"
SELECT json_object('recipe_hash',recipe_hash,'manifest_digest',manifest_digest,
 'relative_dir',relative_dir,'publication_generation',publication_generation,'scrub_object_index',scrub_object_index) AS result_json
FROM transcode_cache_locations WHERE node_id = json_extract($1,'$.node') AND storage_class = 'local'
 AND complete = 1 AND manifest_digest IS NOT NULL AND publication_generation > 0
 AND (json_extract($1,'$.key') IS NULL OR 'transcode:' || recipe_hash || ':' || manifest_digest = json_extract($1,'$.key'))
ORDER BY MAX(last_seen_at * 1000,COALESCE((SELECT MAX(job.updated_at_ms) FROM background_jobs job
 WHERE job.kind = 'artifact_verify' AND job.target_node_id = transcode_cache_locations.node_id
 AND json_extract(job.payload_json,'$.artifact_key') = 'transcode:' || transcode_cache_locations.recipe_hash || ':' || transcode_cache_locations.manifest_digest),0)),recipe_hash LIMIT 128
"#.into(), encode(&serde_json::json!({"node":node,"key":artifact_key}))?,false,true).await?;
    rows.iter().map(|row| decode(row)).collect()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyTranscode {
    pub token: JobToken,
    pub recipe_hash: String,
    pub manifest_digest: String,
    pub relative_dir: String,
    pub publication_generation: i64,
    pub valid: bool,
    pub next_object_index: i64,
    pub now_ms: i64,
}

pub(super) async fn verify_transcode<T: QueueSql>(
    store: &T,
    input: VerifyTranscode,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    if !digest(&input.recipe_hash)
        || !digest(&input.manifest_digest)
        || input.relative_dir.is_empty()
        || input.relative_dir.len() > 1024
        || input.publication_generation <= 0
        || !(0..=8192).contains(&input.next_object_index)
        || input.now_ms < 0
        || input.now_ms.checked_add(604800000).is_none()
    {
        return Err(StoreError::Task(
            "invalid cache verification observation".into(),
        ));
    }
    let key =
        super::background_jobs_transcode::artifact_key(&input.recipe_hash, &input.manifest_digest);
    let generation = format!("{}:{}", input.relative_dir, input.publication_generation);
    let mut body =
        serde_json::to_value(&input).map_err(|error| StoreError::Task(error.to_string()))?;
    body["artifact_key"] = key.clone().into();
    body["location_generation"] = generation.clone().into();
    body["output"] = serde_json::json!({"artifact_kind":"verification", "artifact_key":key, "node_id":input.token.node_id,
        "location_generation":generation, "valid":input.valid, "next_object_index":input.next_object_index});
    let rows = store
        .queue_sql(
            VERIFY_SQL.replace("/* location predicate */", TRANSCODE_LOCATOR_SQL),
            encode(&body)?,
            true,
            true,
        )
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing verification publication verdict".into()))?,
    )
}
const TRANSCODE_LOCATOR_SQL: &str = r#"AND EXISTS (SELECT 1 FROM transcode_cache_locations location
    WHERE location.recipe_hash = json_extract(body,'$.recipe_hash') AND location.node_id = json_extract(body,'$.token.node_id')
      AND location.storage_class = 'local' AND location.relative_dir = json_extract(body,'$.relative_dir')
      AND location.manifest_digest = json_extract(body,'$.manifest_digest') AND location.complete = 1
      AND location.publication_generation = json_extract(body,'$.publication_generation'))
"#;

const VERIFY_SQL: &str = r#"
WITH input AS (SELECT json($1) AS body), observation AS (
 SELECT body, job.id, job.state, job.result_ref, json_extract(body,'$.output') AS output,
 EXISTS (SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id = job.id
  AND attempt.fence = job.fence AND attempt.fence = json_extract(body,'$.token.fence')
  AND attempt.claim_id = json_extract(body,'$.token.claim_id')
  AND attempt.owner_node_id = json_extract(body,'$.token.node_id')
  AND attempt.owner_boot_id = json_extract(body,'$.token.boot_id')) AS same_attempt,
 job.state = 'running' AND job.kind = 'artifact_verify' AND job.payload_version = 1
  AND job.owner_node_id = json_extract(body,'$.token.node_id')
  AND job.owner_boot_id = json_extract(body,'$.token.boot_id')
  AND job.claim_id = json_extract(body,'$.token.claim_id')
  AND job.fence = json_extract(body,'$.token.fence')
  AND job.revision = json_extract(body,'$.token.revision') AND job.revision < 9223372036854775807
  AND job.lease_expires_ms = json_extract(body,'$.token.lease_expires_ms')
  AND job.lease_expires_ms > json_extract(body,'$.now_ms')
  AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || job.owner_node_id)
  AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required WHERE required.job_id = job.id
    AND NOT EXISTS (SELECT 1 FROM background_job_reservations held WHERE held.job_id = job.id AND held.fence = job.fence
      AND held.resource_key = required.resource_key AND held.expires_at_ms >= job.lease_expires_ms))
  AND json_extract(job.payload_json,'$.artifact_key') = json_extract(body,'$.artifact_key')
  AND json_extract(job.payload_json,'$.target_node_id') = json_extract(body,'$.token.node_id')
  AND EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = job.id
    AND interest.state = 'pending' AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract(body,'$.now_ms')))
  /* location predicate */
  AND (json_extract(body,'$.valid') = 1 OR (SELECT COUNT(*) FROM background_artifact_repairs) < 4096
    OR EXISTS (SELECT 1 FROM background_artifact_repairs WHERE original_key = json_extract(body,'$.artifact_key')
      AND target_node_id = json_extract(body,'$.token.node_id') AND location_generation = json_extract(body,'$.location_generation')))
 AS owns
 FROM input LEFT JOIN background_jobs job ON job.id = json_extract(body,'$.token.job_id')
), verdict AS (
 SELECT *, CASE WHEN state = 'succeeded' AND same_attempt AND result_ref = output THEN 'already_published'
  WHEN NOT COALESCE(owns,0) THEN 'lost_ownership' ELSE 'published' END AS outcome FROM observation
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract(body,'$.token.claim_id'), 'verify_transcode', body,
 CASE WHEN outcome IN ('published','already_published') THEN json_object('outcome',outcome,'job_id',id,'result_ref',output || '')
 ELSE json_object('outcome',outcome) END FROM verdict RETURNING result_json
"#;

pub(super) async fn repairs<T: QueueSql>(
    store: &T,
    pending_only: bool,
) -> Result<Vec<ArtifactRepair>, StoreError> {
    let rows = store.queue_sql(r#"
SELECT json_object('id',id,'original_key',original_key,'target_node_id',target_node_id,
 'location_generation',location_generation,'artifact_key',artifact_key,'producer_payload',json(producer_payload),
 'phase',phase,'job_id',job_id,'created_at_ms',created_at_ms,'updated_at_ms',updated_at_ms,'expires_ms',expires_ms) AS result_json
FROM background_artifact_repairs WHERE json_extract($1,'$.pending') = 0 OR phase IN ('copy','build','deliver')
ORDER BY CASE WHEN phase IN ('ready','failed') THEN 1 ELSE 0 END,
 CASE WHEN json_extract($1,'$.pending') = 1 THEN updated_at_ms ELSE -updated_at_ms END,id LIMIT 64
"#.into(), encode(&serde_json::json!({"pending":pending_only}))?,false,true).await?;
    rows.iter().map(|row| decode(row)).collect()
}

pub(super) async fn enqueue_repair<T: QueueSql>(
    store: &T,
    repair: ArtifactRepair,
    now_ms: i64,
) -> Result<super::background_jobs::EnqueueOutcome, StoreError> {
    use super::background_jobs::{
        enqueue_body, EnqueueJob, EnqueueOutcome, JobRequest, ENQUEUE_SQL,
    };
    use sha2::{Digest, Sha256};
    if uuid::Uuid::parse_str(&repair.id).is_err()
        || !matches!(repair.phase.as_str(), "copy" | "build" | "deliver")
        || now_ms >= repair.expires_ms
    {
        return Ok(EnqueueOutcome::NoDemand);
    }
    let payload = if repair.phase == "build" {
        repair
            .producer_payload
            .ok_or_else(|| StoreError::Task("repair has no original producer".into()))?
    } else {
        JobPayload::ArtifactHydrate {
            artifact_key: repair.artifact_key,
            target_node_id: repair.target_node_id,
        }
    };
    payload.validate()?;
    let digest = hex::encode(Sha256::digest(encode(&payload)?));
    let dedupe_key = match &payload {
        JobPayload::ArtifactHydrate { .. } => format!("hydrate:{digest}"),
        JobPayload::TranscodePrepare { recipe_key, .. } => recipe_key.clone(),
        JobPayload::ArtworkDerivative { artifact_key, .. } => format!("artwork:{artifact_key}"),
        _ => {
            return Err(StoreError::Task(
                "unsupported artifact repair producer".into(),
            ))
        }
    };
    let request = EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload,
        dedupe_key,
        priority: 1,
        not_before_ms: now_ms,
        now_ms,
        request: JobRequest {
            scope: "artifact-repair".into(),
            request_id: format!("{}:{}", repair.id, repair.phase),
            request_digest: digest,
            consumer_kind: "artifact_repair".into(),
            consumer_ref: repair.id.clone(),
            target_node_id: None,
            deadline_ms: Some(repair.expires_ms),
            retain_identity: false,
        },
    };
    request.validate()?;
    let mut body = enqueue_body(store, &request).await?;
    body["repair_id"] = repair.id.into();
    body["repair_phase"] = repair.phase.into();
    let rows = store
        .queue_sql(ENQUEUE_SQL.into(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing artifact repair admission".into()))?,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyArtwork {
    pub token: JobToken,
    pub location: super::background_jobs_artwork::ArtworkLocation,
    pub valid: bool,
    pub now_ms: i64,
}
pub(super) async fn artwork_candidates<T: QueueSql>(
    store: &T,
    node: &str,
) -> Result<Vec<super::background_jobs_artwork::ArtworkLocation>, StoreError> {
    let rows = store.queue_sql(r#"
SELECT json_object('artifact_key',artifact_key,'node_id',node_id,'spec',json(spec_json),
 'blob_sha256',blob_sha256,'bytes',bytes,'built_by_node_id',built_by_node_id,'built_at_ms',built_at_ms,'verified_at_ms',verified_at_ms) AS result_json
FROM background_artwork_locations WHERE node_id = json_extract($1,'$.node')
ORDER BY MAX(verified_at_ms,COALESCE((SELECT MAX(job.updated_at_ms) FROM background_jobs job
 WHERE job.kind = 'artifact_verify' AND job.target_node_id = background_artwork_locations.node_id
 AND json_extract(job.payload_json,'$.artifact_key') = 'artwork:' || background_artwork_locations.artifact_key),0)),artifact_key LIMIT 64
"#.into(), encode(&serde_json::json!({"node":node}))?,false,true).await?;
    rows.iter().map(|row| decode(row)).collect()
}
pub(super) async fn verify_artwork<T: QueueSql>(
    store: &T,
    input: VerifyArtwork,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    input.location.spec.validate()?;
    let location = &input.location;
    if location.node_id != input.token.node_id
        || location.artifact_key != location.spec.artifact_key()
        || !digest(&location.blob_sha256)
        || location.built_at_ms < 0
        || location.verified_at_ms < 0
        || !(1..=crate::metadata::MAX_ARTWORK_BYTES as i64).contains(&location.bytes)
        || input.now_ms < 0
        || input.now_ms.checked_add(604800000).is_none()
    {
        return Err(StoreError::Task(
            "invalid artwork verification observation".into(),
        ));
    }
    let key = format!("artwork:{}", location.artifact_key);
    let generation = format!("{}:{}", location.blob_sha256, location.built_at_ms);
    let mut body =
        serde_json::to_value(&input).map_err(|error| StoreError::Task(error.to_string()))?;
    body["artifact_key"] = key.clone().into();
    body["location_generation"] = generation.clone().into();
    body["producer_payload"] = serde_json::to_value(JobPayload::ArtworkDerivative {
        artifact_key: location.artifact_key.clone(),
        spec: location.spec.clone(),
    })
    .map_err(|error| StoreError::Task(error.to_string()))?;
    body["output"] = serde_json::json!({"artifact_kind":"verification","artifact_key":key,"node_id":input.token.node_id,"location_generation":generation,"valid":input.valid});
    // Lease, attempt, reservations, waiter, replay and repair-capacity checks
    // are identical for both artifact types; only the physical locator differs.
    let sql = VERIFY_SQL.replace("/* location predicate */", r#"  AND EXISTS (SELECT 1 FROM background_artwork_locations location WHERE
    location.artifact_key = json_extract(body,'$.location.artifact_key') AND location.node_id = json_extract(body,'$.token.node_id')
    AND location.spec_json = json_extract(body,'$.location.spec') AND location.blob_sha256 = json_extract(body,'$.location.blob_sha256')
    AND location.bytes = json_extract(body,'$.location.bytes') AND location.built_by_node_id = json_extract(body,'$.location.built_by_node_id')
    AND location.built_at_ms = json_extract(body,'$.location.built_at_ms') AND location.verified_at_ms = json_extract(body,'$.location.verified_at_ms'))
"#).replace("'verify_transcode'", "'verify_artwork'");
    let rows = store.queue_sql(sql, encode(&body)?, true, true).await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing artwork verification verdict".into()))?,
    )
}
