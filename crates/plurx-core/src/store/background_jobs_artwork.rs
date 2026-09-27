//! Immutable artwork variants and exact-attempt publication. Bytes remain in
//! the existing node-local derivative cache; this table advertises verified
//! copies, not a second cache or a promise that an offline holder is usable.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::background_jobs::{
    decode, digest, encode, identifier, JobPublishOutcome, JobToken, QueueSql,
};
use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("background_jobs_artwork.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 77;
pub const ARTWORK_PIPELINE: &str = "ffmpeg-lanczos-fit-v1";
pub const MAX_ARTWORK_LOCATIONS: usize = 32_768;
pub const ARTWORK_LOCATION_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkVariantSpec {
    /// A basename only. It locates the input but is not its content identity.
    pub source_name: String,
    pub source_sha256: String,
    pub width: u32,
    pub format: String,
    pub pipeline: String,
}
impl ArtworkVariantSpec {
    pub fn validate(&self) -> Result<(), StoreError> {
        let expected_format = match self
            .source_name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .as_deref()
        {
            Some("jpg" | "jpeg") => "jpeg",
            Some("gif" | "png") => "png",
            Some("webp") => "webp",
            _ => "",
        };
        if !identifier(&self.source_name)
            || self.source_name.len() > 160
            || self.source_name.contains(':')
            || self.format != expected_format
            || self.source_name == "."
            || self.source_name == ".."
            || !digest(&self.source_sha256)
            || !matches!(self.width, 300 | 500 | 780)
            || !matches!(self.format.as_str(), "jpeg" | "png" | "webp")
            || !identifier(&self.pipeline)
        {
            return Err(StoreError::Task(
                "invalid artwork variant specification".into(),
            ));
        }
        Ok(())
    }
    pub fn artifact_key(&self) -> String {
        // Explicit tuple avoids field-order and source-alias dependence.
        let mut hash = Sha256::new();
        for value in [
            &self.source_sha256,
            &self.width.to_string(),
            &self.format,
            &self.pipeline,
        ] {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        hex::encode(hash.finalize())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkLocation {
    pub artifact_key: String,
    pub node_id: String,
    pub spec: ArtworkVariantSpec,
    pub blob_sha256: String,
    pub bytes: i64,
    pub built_by_node_id: String,
    pub built_at_ms: i64,
    pub verified_at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishArtworkJob {
    pub token: JobToken,
    pub location: ArtworkLocation,
    pub now_ms: i64,
}

pub(super) async fn locations<T: QueueSql>(
    store: &T,
    key: &str,
    now_ms: i64,
) -> Result<Vec<ArtworkLocation>, StoreError> {
    if !digest(key) || now_ms < 0 {
        return Err(StoreError::Task("invalid artwork location lookup".into()));
    }
    let rows = store.queue_sql(r#"
SELECT json_object('artifact_key', artifact_key, 'node_id', node_id,
  'spec', json(spec_json), 'blob_sha256', blob_sha256, 'bytes', bytes,
  'built_by_node_id', built_by_node_id, 'built_at_ms', built_at_ms,
  'verified_at_ms', verified_at_ms) AS result_json
FROM background_artwork_locations WHERE artifact_key = json_extract($1, '$.key')
  AND verified_at_ms > json_extract($1, '$.oldest')
  AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || node_id)
ORDER BY verified_at_ms DESC, node_id LIMIT 64
"#.into(), encode(&serde_json::json!({"key": key, "oldest": now_ms.saturating_sub(ARTWORK_LOCATION_TTL_MS)}))?, false, true).await?;
    rows.iter().map(|row| decode(row)).collect()
}

pub(super) async fn publish<T: QueueSql>(
    store: &T,
    request: PublishArtworkJob,
) -> Result<JobPublishOutcome, StoreError> {
    request.token.validate()?;
    let location = &request.location;
    location.spec.validate()?;
    if location.artifact_key != location.spec.artifact_key()
        || location.node_id != request.token.node_id
        || !digest(&location.blob_sha256)
        || !(1..=crate::metadata::MAX_ARTWORK_BYTES as i64).contains(&location.bytes)
        || !identifier(&location.built_by_node_id)
        || location.built_at_ms < 0
        || location.built_at_ms > request.now_ms
        || location.verified_at_ms != request.now_ms
        || request.now_ms < 0
    {
        return Err(StoreError::Task("invalid artwork publication".into()));
    }
    let mut body = serde_json::to_value(&request).map_err(|e| StoreError::Task(e.to_string()))?;
    // Exclude observation time from acknowledgement-replay identity.
    body["output"] = serde_json::json!({"artifact_kind":"artwork", "artifact_key":location.artifact_key,
        "node_id":location.node_id, "blob_sha256":location.blob_sha256, "bytes":location.bytes,
        "built_by_node_id":location.built_by_node_id, "built_at_ms":location.built_at_ms});
    let rows = store
        .queue_sql(PUBLISH_SQL.into(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing artwork publication verdict".into()))?,
    )
}

const PUBLISH_SQL: &str = r#"
WITH input AS (SELECT json($1) AS body), observation AS (
 SELECT body, job.id, job.state, job.result_ref,
   json_extract(body, '$.output') AS output,
   EXISTS (SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id = job.id
     AND attempt.fence = job.fence AND attempt.fence = json_extract(body, '$.token.fence')
     AND attempt.claim_id = json_extract(body, '$.token.claim_id')
     AND attempt.owner_node_id = json_extract(body, '$.token.node_id')
     AND attempt.owner_boot_id = json_extract(body, '$.token.boot_id')) AS same_attempt,
   job.state = 'running' AND job.payload_version = 1
     AND job.owner_node_id = json_extract(body, '$.token.node_id')
     AND job.owner_boot_id = json_extract(body, '$.token.boot_id')
     AND job.claim_id = json_extract(body, '$.token.claim_id')
     AND job.fence = json_extract(body, '$.token.fence')
     AND job.revision = json_extract(body, '$.token.revision')
     AND job.revision < 9223372036854775807
     AND job.lease_expires_ms = json_extract(body, '$.token.lease_expires_ms')
     AND job.lease_expires_ms > json_extract(body, '$.now_ms')
     AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || job.owner_node_id)
     AND EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = job.id
       AND interest.state = 'pending' AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract(body, '$.now_ms')))
     AND ((job.kind = 'artwork_derivative'
       AND json_extract(job.payload_json, '$.artifact_key') = json_extract(body, '$.location.artifact_key')
       AND json_remove(json_extract(job.payload_json, '$.spec'), '$.source_name') = json_remove(json_extract(body, '$.location.spec'), '$.source_name')
       AND (json_extract(body, '$.location.built_by_node_id') = job.owner_node_id
         OR EXISTS (SELECT 1 FROM background_artwork_locations original
           WHERE original.artifact_key = json_extract(body, '$.location.artifact_key')
             AND original.built_by_node_id = json_extract(body, '$.location.built_by_node_id')
             AND original.built_at_ms = json_extract(body, '$.location.built_at_ms'))))
     OR (job.kind = 'artifact_hydrate' AND job.target_node_id = job.owner_node_id
       AND json_extract(job.payload_json, '$.artifact_key') = 'artwork:' || json_extract(body, '$.location.artifact_key')
       AND EXISTS (SELECT 1 FROM background_artwork_locations original
         WHERE original.artifact_key = json_extract(body, '$.location.artifact_key')
           AND original.blob_sha256 = json_extract(body, '$.location.blob_sha256')
           AND original.bytes = json_extract(body, '$.location.bytes')
           AND original.built_by_node_id = json_extract(body, '$.location.built_by_node_id')
           AND original.built_at_ms = json_extract(body, '$.location.built_at_ms')))) AS owns,
   NOT EXISTS (SELECT 1 FROM background_artwork_locations original
     WHERE original.artifact_key = json_extract(body, '$.location.artifact_key')
       AND (original.blob_sha256 != json_extract(body, '$.location.blob_sha256')
         OR original.bytes != json_extract(body, '$.location.bytes')
         OR original.built_by_node_id != json_extract(body, '$.location.built_by_node_id')
         OR original.built_at_ms != json_extract(body, '$.location.built_at_ms'))) AS matches_artifact,
   (EXISTS (SELECT 1 FROM background_artwork_locations original
      WHERE original.artifact_key = json_extract(body, '$.location.artifact_key')
        AND original.node_id = json_extract(body, '$.token.node_id'))
    OR (SELECT COUNT(*) FROM background_artwork_locations) < 32768) AS has_capacity
 FROM input LEFT JOIN background_jobs job ON job.id = json_extract(body, '$.token.job_id')
), verdict AS (
 SELECT *, CASE WHEN state = 'succeeded' AND same_attempt AND result_ref = output THEN 'already_published'
   WHEN NOT COALESCE(owns, 0) THEN 'lost_ownership'
   WHEN NOT matches_artifact OR NOT has_capacity THEN 'artifact_conflict'
   ELSE 'published' END AS outcome FROM observation
)
INSERT INTO background_job_commands(id, operation, request_json, result_json)
SELECT json_extract(body, '$.token.claim_id'), 'publish_artwork', body,
 CASE WHEN outcome IN ('published','already_published') THEN json_object('outcome',outcome,'job_id',id,'result_ref',output || '')
 ELSE json_object('outcome',outcome) END FROM verdict RETURNING result_json
"#;

/// One active demand per receiving node and content identity. Repeated grid
/// requests neither append waiters nor reset a failed intent's retry budget;
/// a fresh real request may retry after an hour. Every intent expires in 24h.
pub(super) async fn enqueue<T: QueueSql>(
    store: &T,
    spec: ArtworkVariantSpec,
    node_id: &str,
    now_ms: i64,
) -> Result<super::background_jobs::EnqueueOutcome, StoreError> {
    use super::background_jobs::{enqueue_body, EnqueueJob, JobPayload, JobRequest, ENQUEUE_SQL};
    spec.validate()?;
    if !identifier(node_id) || node_id.len() > 200 {
        return Err(StoreError::Task("invalid artwork demand node".into()));
    }
    let key = spec.artifact_key();
    let request = EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: JobPayload::ArtworkDerivative {
            artifact_key: key.clone(),
            spec,
        },
        dedupe_key: format!("artwork:{key}"),
        priority: 1,
        not_before_ms: now_ms,
        now_ms,
        request: JobRequest {
            scope: format!("artwork:{node_id}"),
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: key.clone(),
            consumer_kind: "artwork".into(),
            consumer_ref: key,
            target_node_id: Some(node_id.into()),
            deadline_ms: Some(
                now_ms
                    .checked_add(86_400_000)
                    .ok_or_else(|| StoreError::Task("artwork deadline overflow".into()))?,
            ),
            retain_identity: false,
        },
    };
    request.validate()?;
    let mut body = enqueue_body(store, &request).await?;
    body["artwork_demand"] = true.into();
    let rows = store
        .queue_sql(ENQUEUE_SQL.into(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing artwork admission verdict".into()))?,
    )
}

/// Historical content/provenance is not a live-holder or transfer proof.
pub(super) async fn canonical<T: QueueSql>(
    store: &T,
    key: &str,
) -> Result<Option<ArtworkLocation>, StoreError> {
    if !digest(key) {
        return Err(StoreError::Task("invalid artwork identity".into()));
    }
    let rows = store
        .queue_sql(
            r#"
SELECT json_object('artifact_key', artifact_key, 'node_id', node_id,
  'spec', json(spec_json), 'blob_sha256', blob_sha256, 'bytes', bytes,
  'built_by_node_id', built_by_node_id, 'built_at_ms', built_at_ms,
  'verified_at_ms', verified_at_ms) AS result_json
FROM background_artwork_locations WHERE artifact_key = json_extract($1, '$.key')
ORDER BY verified_at_ms DESC, node_id LIMIT 1
"#
            .into(),
            encode(&serde_json::json!({"key": key}))?,
            false,
            true,
        )
        .await?;
    rows.first().map(|row| decode(row)).transpose()
}
