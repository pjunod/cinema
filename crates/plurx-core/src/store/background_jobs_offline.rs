//! Exact-recipe attachment for an already quota-admitted offline package.
//! Local delivery affinity is retained until portable transcode hydration exists.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::background_jobs::{
    decode, digest, encode, enqueue_body, identifier, invalid, BackgroundJob, EnqueueJob,
    EnqueueOutcome, JobRequest, JobToken, QueueSql, ENQUEUE_SQL, JOB_JSON,
};
use crate::error::StoreError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinOfflineJob {
    pub package_id: String,
    pub node_id: String,
    pub claim_generation: i64,
    pub recipe_hash: String,
    pub now_ms: i64,
}

pub(super) async fn bind_recipe<T: QueueSql>(
    store: &T,
    token: JobToken,
    recipe_hash: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    token.validate()?;
    if !digest(recipe_hash) || now_ms < 0 {
        return Err(invalid("invalid recipe binding"));
    }
    let rows = store.queue_sql(
        "UPDATE background_jobs SET checkpoint_json = json_set(CASE WHEN json_type(checkpoint_json) = 'object' THEN checkpoint_json ELSE '{}' END,
            '$.effective_recipe_hash', json_extract($1, '$.recipe_hash'), '$.recipe_node_id', owner_node_id)
         WHERE id = json_extract($1, '$.token.job_id') AND kind = 'transcode_prepare' AND state = 'running'
            AND owner_node_id = json_extract($1, '$.token.node_id')
            AND owner_boot_id = json_extract($1, '$.token.boot_id')
            AND claim_id = json_extract($1, '$.token.claim_id') AND fence = json_extract($1, '$.token.fence')
            AND revision = json_extract($1, '$.token.revision')
            AND lease_expires_ms = json_extract($1, '$.token.lease_expires_ms')
            AND lease_expires_ms > json_extract($1, '$.now_ms')
            AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || owner_node_id)
            AND (json_extract(checkpoint_json, '$.effective_recipe_hash') = json_extract($1, '$.recipe_hash')
                OR NOT EXISTS (SELECT 1 FROM background_job_waiters WHERE job_id = background_jobs.id
                    AND consumer_kind = 'offline_preparation' AND state = 'pending'
                    AND (deadline_ms IS NULL OR deadline_ms > json_extract($1, '$.now_ms'))))
         RETURNING 'true' AS result_json".into(),
        encode(&serde_json::json!({"token": token, "recipe_hash": recipe_hash, "now_ms": now_ms}))?, true, true,
    ).await?;
    Ok(!rows.is_empty())
}

pub(super) async fn join<T: QueueSql>(
    store: &T,
    input: JoinOfflineJob,
) -> Result<Option<EnqueueOutcome>, StoreError> {
    if !identifier(&input.package_id)
        || !identifier(&input.node_id)
        || input.claim_generation <= 0
        || !digest(&input.recipe_hash)
        || input.now_ms < 0
    {
        return Err(invalid("invalid offline join"));
    }
    #[derive(Deserialize)]
    struct Package {
        user_id: i64,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
        expires_at: i64,
    }
    let rows = store.queue_sql(
        "SELECT json_object('user_id', user_id, 'file_id', file_id, 'source_size', source_size,
            'source_mtime', source_mtime, 'expires_at', expires_at) AS result_json FROM offline_packages
         WHERE id = json_extract($1, '$.package_id') AND node_id = json_extract($1, '$.node_id')
            AND claim_generation = json_extract($1, '$.claim_generation') AND state = 'preparing'
            AND recipe_hash = json_extract($1, '$.recipe_hash') AND decoder_recovery_state = 'primary'
            AND expires_at > json_extract($1, '$.now_ms') / 1000".into(), encode(&input)?, false, true,
    ).await?;
    let Some(package) = rows.first() else {
        return Ok(None);
    };
    let package: Package = decode(package)?;
    let rows = store.queue_sql(format!(
        "SELECT {JOB_JSON} AS result_json FROM background_jobs
         WHERE kind = 'transcode_prepare' AND state = 'running' AND payload_version = 1
            AND owner_node_id = json_extract($1, '$.node_id') AND lease_expires_ms > json_extract($1, '$.now_ms')
            AND json_extract(checkpoint_json, '$.recipe_node_id') = owner_node_id
            AND json_extract(checkpoint_json, '$.effective_recipe_hash') = json_extract($1, '$.recipe_hash')
            AND json_extract(payload_json, '$.file_id') = json_extract($1, '$.file_id')
            AND json_extract(payload_json, '$.source_size') = json_extract($1, '$.source_size')
            AND json_extract(payload_json, '$.source_mtime') = json_extract($1, '$.source_mtime')
         ORDER BY priority DESC, created_at_ms, id LIMIT 1"
    ), encode(&serde_json::json!({"node_id": input.node_id, "recipe_hash": input.recipe_hash,
        "now_ms": input.now_ms, "file_id": package.file_id, "source_size": package.source_size,
        "source_mtime": package.source_mtime}))?, false, true).await?;
    let Some(job) = rows.first() else {
        return Ok(None);
    };
    let job: BackgroundJob = decode(job)?;
    let request = EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: job.supported_payload()?,
        dedupe_key: job.dedupe_key,
        priority: 2,
        not_before_ms: input.now_ms,
        now_ms: input.now_ms,
        request: JobRequest {
            scope: format!("user:{}", package.user_id),
            request_id: format!("{}:{}", input.package_id, input.claim_generation),
            request_digest: hex::encode(Sha256::digest(format!(
                "offline:{}:{}:{}",
                input.package_id, input.claim_generation, input.recipe_hash
            ))),
            consumer_kind: "offline_preparation".into(),
            consumer_ref: input.package_id.clone(),
            target_node_id: Some(input.node_id.clone()),
            deadline_ms: Some(
                package
                    .expires_at
                    .saturating_mul(1000)
                    .min(input.now_ms.saturating_add(86_400_000)),
            ),
            retain_identity: false,
        },
    };
    request.validate()?;
    let mut body = enqueue_body(store, &request).await?;
    body["offline_join"] =
        serde_json::to_value(&input).map_err(|error| invalid(&error.to_string()))?;
    body["offline_join"]["job_id"] = job.id.into();
    let rows = store
        .queue_sql(ENQUEUE_SQL.into(), encode(&body)?, true, true)
        .await?;
    Ok(Some(decode(rows.first().ok_or_else(|| {
        invalid("missing offline join verdict")
    })?)?))
}
