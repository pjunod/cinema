//! Portable sentence vectors. Queue attempts own computation; this bounded
//! artifact table supplies every node's local serving index.
use super::background_jobs::{decode, digest, encode, JobPublishOutcome, JobToken, QueueSql};
use super::classification::Entry;
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const SCHEMA: &str = include_str!("background_jobs_embeddings.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 80;
pub const MAX_DIMENSIONS: usize = 2048;
pub const MAX_EMBEDDINGS: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingModel {
    pub weights_sha256: String,
    pub config_sha256: String,
    pub tokenizer_sha256: String,
    pub tokenizer_version: String,
    pub dimensions: usize,
    pub normalization_version: String,
}
impl EmbeddingModel {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !digest(&self.weights_sha256)
            || !digest(&self.config_sha256)
            || !digest(&self.tokenizer_sha256)
            || !(1..=MAX_DIMENSIONS).contains(&self.dimensions)
            || [
                self.tokenizer_version.as_str(),
                self.normalization_version.as_str(),
            ]
            .iter()
            .any(|value| {
                value.is_empty() || value.len() > 64 || !super::background_jobs::identifier(value)
            })
        {
            return Err(StoreError::Task("invalid embedding model identity".into()));
        }
        Ok(())
    }
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        for value in [
            &self.weights_sha256,
            &self.config_sha256,
            &self.tokenizer_sha256,
            &self.tokenizer_version,
            &self.dimensions.to_string(),
            &self.normalization_version,
        ] {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        hex::encode(hash.finalize())
    }
}

pub fn content_digest(source_json: &str, classification_revision: i64) -> String {
    hex::encode(Sha256::digest(format!(
        "{source_json}:{classification_revision}"
    )))
}
pub fn entry_digest(entry: &Entry) -> String {
    content_digest(
        &entry.source_json,
        entry.record.as_ref().map_or(0, |record| record.revision),
    )
}
pub fn embedding_text(entry: &Entry) -> Result<String, StoreError> {
    let input = entry.input()?;
    let labels = entry
        .record
        .as_ref()
        .filter(|record| record.source_json == entry.source_json)
        .map(|record| record.classification.terms(&record.overrides).join(", "))
        .unwrap_or_default();
    Ok(format!(
        "{}. {}. {}. {}. {}",
        input.title,
        input.genres.join(", "),
        input.tags.join(", "),
        labels,
        input.overview
    )
    .chars()
    .take(8000)
    .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedEmbedding {
    pub item_id: i64,
    pub content_digest: String,
    pub model: EmbeddingModel,
    pub vector: Vec<f32>,
    pub vector_sha256: String,
}
impl SharedEmbedding {
    pub fn vector_digest(vector: &[f32]) -> String {
        let mut hash = Sha256::new();
        for value in vector {
            hash.update(value.to_le_bytes());
        }
        hex::encode(hash.finalize())
    }
    pub fn validate(&self) -> Result<(), StoreError> {
        self.model.validate()?;
        let norm = self
            .vector
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>();
        if self.item_id <= 0
            || !digest(&self.content_digest)
            || self.vector.len() != self.model.dimensions
            || !norm.is_finite()
            || (norm - 1.0).abs() > 0.005
            || Self::vector_digest(&self.vector) != self.vector_sha256
        {
            return Err(StoreError::Task("invalid portable embedding".into()));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingLookup {
    pub artifact: Option<SharedEmbedding>,
    /// A missing/evicted successful artifact permits one new computation.
    /// Failed attempts keep this predecessor unchanged, preserving their budget.
    pub last_completed_job: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishEmbeddingJob {
    pub token: JobToken,
    pub artifact: SharedEmbedding,
    pub source_json: String,
    pub classification_revision: i64,
    pub now_ms: i64,
}

pub(super) async fn lookup<T: QueueSql>(
    store: &T,
    item_id: i64,
    content: &str,
    model: &str,
) -> Result<EmbeddingLookup, StoreError> {
    if item_id <= 0 || !digest(content) || !digest(model) {
        return Err(StoreError::Task("invalid embedding lookup".into()));
    }
    let rows = store
        .queue_sql(
            r#"
SELECT json_object('artifact_json', (SELECT artifact_json FROM background_embeddings
 WHERE item_id = json_extract($1,'$.item') AND content_digest = json_extract($1,'$.content')
 AND model_digest = json_extract($1,'$.model')),
 'last_completed_job', (SELECT id FROM background_jobs WHERE kind = 'semantic_embedding'
 AND state = 'succeeded' AND json_extract(payload_json,'$.item_id') = json_extract($1,'$.item')
 AND json_extract(payload_json,'$.content_digest') = json_extract($1,'$.content')
 AND json_extract(payload_json,'$.model_digest') = json_extract($1,'$.model')
 ORDER BY updated_at_ms DESC,id DESC LIMIT 1)) AS result_json
"#
            .into(),
            encode(&serde_json::json!({"item":item_id,"content":content,"model":model}))?,
            false,
            true,
        )
        .await?;
    #[derive(Deserialize)]
    struct Stored {
        artifact_json: Option<String>,
        last_completed_job: Option<String>,
    }
    let stored: Stored = decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing embedding lookup".into()))?,
    )?;
    let artifact = stored
        .artifact_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<SharedEmbedding>(raw).ok())
        .filter(|artifact| {
            artifact.validate().is_ok()
                && artifact.item_id == item_id
                && artifact.content_digest == content
                && artifact.model.digest() == model
        });
    if artifact.is_none() {
        if let Some(raw) = stored.artifact_json {
            // Drop only the corrupt version we observed. A concurrent valid
            // replacement wins this CAS and remains available to the next read.
            store.queue_sql("DELETE FROM background_embeddings WHERE item_id = json_extract($1,'$.item') AND model_digest = json_extract($1,'$.model') AND content_digest = json_extract($1,'$.content') AND artifact_json = json_extract($1,'$.raw') RETURNING 'true' AS result_json".into(),
                encode(&serde_json::json!({"item":item_id,"model":model,"content":content,"raw":raw}))?,true,true).await?;
        }
    }
    Ok(EmbeddingLookup {
        artifact,
        last_completed_job: stored.last_completed_job,
    })
}

pub(super) async fn publish<T: QueueSql>(
    store: &T,
    input: PublishEmbeddingJob,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    input.artifact.validate()?;
    if input.now_ms < 0
        || input.classification_revision < 0
        || input.source_json.len() > 64 * 1024
        || content_digest(&input.source_json, input.classification_revision)
            != input.artifact.content_digest
    {
        return Err(StoreError::Task("invalid embedding source proof".into()));
    }
    let mut body = serde_json::to_value(&input).map_err(|e| StoreError::Task(e.to_string()))?;
    body["model_digest"] = input.artifact.model.digest().into();
    body["output"] = serde_json::json!({"artifact_kind":"embedding", "item_id":input.artifact.item_id,
        "content_digest":input.artifact.content_digest, "model_digest":input.artifact.model.digest(),
        "vector_sha256":input.artifact.vector_sha256});
    let sql = PUBLISH_SQL.replace("__SOURCE__", super::classification::SOURCE);
    let rows = store.queue_sql(sql, encode(&body)?, true, true).await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing embedding publication verdict".into()))?,
    )
}

const PUBLISH_SQL: &str = r#"
WITH input AS (SELECT json($1) AS body), observation AS (
 SELECT body, job.id, job.state, job.result_ref, json_extract(body,'$.output') AS output,
 EXISTS (SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id = job.id
  AND attempt.fence = job.fence AND attempt.fence = json_extract(body,'$.token.fence')
  AND attempt.claim_id = json_extract(body,'$.token.claim_id')
  AND attempt.owner_node_id = json_extract(body,'$.token.node_id')
  AND attempt.owner_boot_id = json_extract(body,'$.token.boot_id')) AS same_attempt,
 job.state = 'running' AND job.kind = 'semantic_embedding' AND job.payload_version = 1
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
  AND json_extract(job.payload_json,'$.item_id') = json_extract(body,'$.artifact.item_id')
  AND json_extract(job.payload_json,'$.content_digest') = json_extract(body,'$.artifact.content_digest')
  AND json_extract(job.payload_json,'$.model_digest') = json_extract(body,'$.model_digest')
  AND EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = job.id
   AND interest.state = 'pending' AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract(body,'$.now_ms')))
  AND EXISTS (SELECT 1 FROM items i WHERE i.id = json_extract(body,'$.artifact.item_id')
   AND __SOURCE__ = json_extract(body,'$.source_json')
   AND COALESCE((SELECT revision FROM media_classifications WHERE item_id = i.id),0) = json_extract(body,'$.classification_revision')) AS owns
 FROM input LEFT JOIN background_jobs job ON job.id = json_extract(body,'$.token.job_id')
), verdict AS (
 SELECT *, CASE WHEN state = 'succeeded' AND same_attempt AND result_ref = output THEN 'already_published'
  WHEN NOT COALESCE(owns,0) THEN 'lost_ownership' ELSE 'published' END AS outcome FROM observation
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract(body,'$.token.claim_id'), 'publish_embedding', body,
 CASE WHEN outcome IN ('published','already_published') THEN json_object('outcome',outcome,'job_id',id,'result_ref',output || '')
 ELSE json_object('outcome',outcome) END FROM verdict RETURNING result_json
"#;
