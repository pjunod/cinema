//! Common durable-work identities and ownership contract.
//!
//! A request, a computation, an execution attempt and its immutable output
//! have different identities. In particular a retry must never manufacture
//! a new claim ID after a write acknowledgement is lost.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::StoreError;

pub const JOB_LEASE_MS: i64 = 30_000;
pub const JOB_RENEW_INTERVAL_MS: i64 = 10_000;
pub const CLAIM_RESOLUTION_WINDOW_MS: i64 = 120_000;
pub const REQUEST_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
pub const MAX_ACTIVE_JOBS: usize = 4_096;
pub const FOREGROUND_RESERVED_JOBS: usize = 256;
pub const MAX_RETAINED_JOBS: usize = 10_000;
pub const MAX_WAITERS: usize = 16_384;
pub const MAX_USER_WAITERS: usize = 128;
pub const MAX_ATTEMPTS: usize = 40_000;
pub const RESOLVED_ATTEMPT_HISTORY: usize = 16;
pub const MAX_PAGE_SIZE: usize = 128;
pub const MAX_RENEW_BATCH: usize = 64;
pub const MAX_PAYLOAD_BYTES: usize = 16 * 1_024;
pub const MAX_CHECKPOINT_BYTES: usize = 4 * 1_024;

pub(crate) const SCHEMA: &str = include_str!("background_jobs_schema.sql");

// Both backends execute the same admission statement and schema trigger.
// The returned snapshot is the verdict that authorized the mutation, not a
// follow-up read which might observe a different concurrent request.
pub(super) const ENQUEUE_SQL: &str = r#"
WITH request AS (SELECT json($1) AS body), snapshot AS (
  SELECT body,
    (SELECT job_id FROM background_job_waiters
      WHERE request_scope = json_extract(body, '$.request.scope')
        AND request_id = json_extract(body, '$.request.request_id')) AS prior_job,
    (SELECT request_digest FROM background_job_waiters
      WHERE request_scope = json_extract(body, '$.request.scope')
        AND request_id = json_extract(body, '$.request.request_id')) AS prior_digest,
    (SELECT receipt_expires_ms FROM background_job_waiters
      WHERE request_scope = json_extract(body, '$.request.scope')
        AND request_id = json_extract(body, '$.request.request_id')) AS prior_expiry,
    (SELECT state FROM background_job_waiters
      WHERE request_scope = json_extract(body, '$.request.scope')
        AND request_id = json_extract(body, '$.request.request_id')) AS prior_state,
    (SELECT id FROM background_jobs WHERE dedupe_key = json_extract(body, '$.dedupe_key')
      AND state IN ('queued','running','cancelling')) AS active_job,
    (SELECT state FROM background_jobs WHERE dedupe_key = json_extract(body, '$.dedupe_key')
      AND state IN ('queued','running','cancelling')) AS active_state,
    (SELECT payload_json FROM background_jobs WHERE dedupe_key = json_extract(body, '$.dedupe_key')
      AND state IN ('queued','running','cancelling')) AS active_payload
  FROM request
), classified AS (
  SELECT *, CASE
    WHEN prior_job IS NOT NULL AND prior_digest != json_extract(body, '$.request.request_digest') THEN 'conflict'
    WHEN prior_job IS NOT NULL THEN 'existing'
    WHEN active_state = 'cancelling' THEN 'job_cancelling'
    WHEN active_job IS NOT NULL AND active_payload != json_extract(body, '$.payload') THEN 'conflict'
    WHEN EXISTS (SELECT 1 FROM background_jobs WHERE id = json_extract(body, '$.id')) THEN 'conflict'
    WHEN (SELECT COUNT(*) FROM background_job_waiters) >= 16384 THEN 'queue_full'
    WHEN json_extract(body, '$.request.scope') LIKE 'user:%' AND (SELECT COUNT(*) FROM background_job_waiters
      WHERE request_scope = json_extract(body, '$.request.scope')
        AND state IN ('pending','awaiting_hydration')) >= 128 THEN 'queue_full'
    WHEN active_job IS NULL AND (SELECT COUNT(*) FROM background_jobs) >= 10000 THEN 'queue_full'
    WHEN active_job IS NULL AND (SELECT COUNT(*) FROM background_jobs
      WHERE state IN ('queued','running','cancelling')) >=
        CASE WHEN json_extract(body, '$.priority') >= 2 THEN 4096 ELSE 3840 END THEN 'queue_full'
    WHEN active_job IS NULL AND (SELECT COUNT(*) FROM background_job_attempts) +
      (SELECT COUNT(*) FROM background_jobs WHERE state IN ('queued','running','cancelling')) >= 40000 THEN 'queue_full'
    ELSE 'accepted' END AS outcome
  FROM snapshot
)
INSERT INTO background_job_commands (id, operation, request_json, result_json)
SELECT json_extract(body, '$.id'), 'enqueue', body,
  CASE outcome
    WHEN 'accepted' THEN json_object('outcome', outcome,
      'job_id', COALESCE(active_job, json_extract(body, '$.id')),
      'receipt_expires_ms', json_extract(body, '$.now_ms') + 604800000)
    WHEN 'existing' THEN json_object('outcome', outcome, 'job_id', prior_job,
      'receipt_expires_ms', prior_expiry, 'cancelled', json(CASE WHEN prior_state = 'cancelled' THEN 'true' ELSE 'false' END))
    WHEN 'job_cancelling' THEN json_object('outcome', outcome, 'retry_after_ms', 1000)
    ELSE json_object('outcome', outcome) END
FROM classified
RETURNING result_json
"#;

pub(super) const JOB_JSON: &str = r#"json_object(
    'id', id, 'payload_version', payload_version, 'payload', json(payload_json),
    'dedupe_key', dedupe_key, 'priority', priority, 'state', state,
    'token', CASE WHEN owner_node_id IS NOT NULL THEN json_object(
      'job_id', id, 'node_id', owner_node_id, 'boot_id', owner_boot_id,
      'claim_id', claim_id, 'fence', fence, 'revision', revision,
      'lease_expires_ms', lease_expires_ms) ELSE NULL END,
    'fence', fence, 'revision', revision, 'failed_attempts', failed_attempts,
    'not_before_ms', not_before_ms, 'checkpoint', json(checkpoint_json),
    'result_ref', result_ref, 'last_error_code', last_error_code,
    'created_at_ms', created_at_ms, 'updated_at_ms', updated_at_ms
)"#;

pub(super) const CLAIM_SQL: &str = r#"
UPDATE background_jobs SET
  state = 'running', owner_node_id = json_extract($1, '$.node_id'),
  owner_boot_id = json_extract($1, '$.boot_id'), claim_id = json_extract($1, '$.claim_id'),
  fence = fence + 1, revision = revision + 1,
  lease_expires_ms = json_extract($1, '$.now_ms') + 30000,
  updated_at_ms = json_extract($1, '$.now_ms')
WHERE id = json_extract($1, '$.job_id')
  AND revision = json_extract($1, '$.expected_revision')
  AND kind = json_extract($1, '$.kind') AND payload_version = json_extract($1, '$.payload_version')
  AND (target_node_id IS NULL OR target_node_id = json_extract($1, '$.node_id'))
  AND (state = 'queued' OR (state = 'running' AND lease_expires_ms <= json_extract($1, '$.now_ms')))
  AND not_before_ms <= json_extract($1, '$.now_ms')
  AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || json_extract($1, '$.node_id'))
  AND fence < 9223372036854775807 AND revision < 9223372036854775807
  AND NOT EXISTS (SELECT 1 FROM background_job_attempts WHERE claim_id = json_extract($1, '$.claim_id'))
  AND (SELECT COUNT(*) FROM background_job_attempts) < 40000
  AND (SELECT COUNT(*) FROM background_job_reservations
    WHERE resource_key = 'source_io' AND expires_at_ms > json_extract($1, '$.now_ms')
      AND job_id != json_extract($1, '$.job_id')) < 2
"#;

const RENEW_SQL: &str = r#"
WITH inputs AS (
  SELECT token.value AS token, json_extract($1, '$.now_ms') AS now_ms
  FROM json_each($1, '$.tokens') token
), outcomes AS (
  SELECT CASE WHEN job.id IS NOT NULL THEN json_object('outcome', 'renewed',
    'token', json_object('job_id', job.id, 'node_id', job.owner_node_id,
      'boot_id', job.owner_boot_id, 'claim_id', job.claim_id, 'fence', job.fence,
      'revision', job.revision + 1, 'lease_expires_ms', now_ms + 30000))
  ELSE json_object('outcome', 'lost_ownership', 'job_id', json_extract(token, '$.job_id')) END AS result
  FROM inputs LEFT JOIN background_jobs job ON job.id = json_extract(token, '$.job_id')
    AND job.owner_node_id = json_extract(token, '$.node_id')
    AND job.owner_boot_id = json_extract(token, '$.boot_id')
    AND job.claim_id = json_extract(token, '$.claim_id') AND job.fence = json_extract(token, '$.fence')
    AND job.revision = json_extract(token, '$.revision')
    AND job.lease_expires_ms = json_extract(token, '$.lease_expires_ms')
    AND job.lease_expires_ms > now_ms AND job.state = 'running'
    AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || job.owner_node_id)
    AND job.revision < 9223372036854775807
)
INSERT INTO background_job_commands (id, operation, request_json, result_json)
SELECT json_extract($1, '$.tokens[0].claim_id'), 'renew', $1,
  COALESCE(json_group_array(json(result)), '[]') FROM outcomes
RETURNING result_json
"#;

const SETTLE_SQL: &str = r#"
UPDATE background_jobs SET
  state = CASE json_extract($1, '$.settlement.disposition')
    WHEN 'yield' THEN 'queued'
    WHEN 'retry' THEN CASE WHEN failed_attempts + 1 >= 5 THEN 'failed' ELSE 'queued' END
    WHEN 'fail' THEN 'failed'
    WHEN 'cancel' THEN 'cancelled' END,
  failed_attempts = failed_attempts + CASE WHEN json_extract($1, '$.settlement.disposition') IN ('retry','fail') THEN 1 ELSE 0 END,
  not_before_ms = COALESCE(json_extract($1, '$.settlement.not_before_ms'), not_before_ms),
  checkpoint_json = CASE WHEN json_extract($1, '$.settlement.disposition') = 'yield'
    THEN json_extract($1, '$.settlement.checkpoint') ELSE checkpoint_json END,
  last_error_code = json_extract($1, '$.settlement.error_code'),
  owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
  revision = revision + 1, updated_at_ms = json_extract($1, '$.now_ms')
WHERE id = json_extract($1, '$.token.job_id')
  AND owner_node_id = json_extract($1, '$.token.node_id')
  AND owner_boot_id = json_extract($1, '$.token.boot_id')
  AND claim_id = json_extract($1, '$.token.claim_id')
  AND fence = json_extract($1, '$.token.fence')
  AND revision = json_extract($1, '$.token.revision')
  AND lease_expires_ms = json_extract($1, '$.token.lease_expires_ms')
  AND lease_expires_ms > json_extract($1, '$.now_ms')
  AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || owner_node_id)
  AND revision < 9223372036854775807
  AND ((state = 'running' AND json_extract($1, '$.settlement.disposition') != 'cancel')
    OR (state = 'cancelling' AND json_extract($1, '$.settlement.disposition') = 'cancel'))
"#;

/// Closed registry. Supporting a payload version is an execution constraint,
/// never a fleet-wide enablement prerequisite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    TranscodePrepare,
    FragmentIndexBuild,
    ArtifactHydrate,
    SubtitleExtract,
    LibraryScan,
    MetadataRefresh,
    ArtifactVerify,
    ArtworkDerivative,
    SemanticEmbedding,
    MediaProbe,
}

/// Closed payload variants carry domain identifiers, never executable text,
/// credentials, external URLs or caller-selected filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobPayload {
    TranscodePrepare {
        file_id: i64,
        source_generation: String,
        recipe_key: String,
        target_height: u32,
    },
    FragmentIndexBuild {
        file_id: i64,
        source_generation: String,
        pipeline_digest: String,
    },
    ArtifactHydrate {
        artifact_key: String,
        target_node_id: String,
    },
    SubtitleExtract {
        file_id: i64,
        source_generation: String,
        track: u32,
        pipeline_digest: String,
    },
    LibraryScan {
        library_id: i64,
        generation: String,
    },
    MetadataRefresh {
        library_id: i64,
        generation: String,
    },
    ArtifactVerify {
        artifact_key: String,
        target_node_id: String,
    },
    ArtworkDerivative {
        artifact_key: String,
        width: u32,
        height: u32,
    },
    SemanticEmbedding {
        item_id: i64,
        content_digest: String,
        model_digest: String,
    },
    MediaProbe {
        file_id: i64,
        source_generation: String,
        probe_digest: String,
    },
}

impl JobPayload {
    pub const fn kind(&self) -> JobKind {
        match self {
            Self::TranscodePrepare { .. } => JobKind::TranscodePrepare,
            Self::FragmentIndexBuild { .. } => JobKind::FragmentIndexBuild,
            Self::ArtifactHydrate { .. } => JobKind::ArtifactHydrate,
            Self::SubtitleExtract { .. } => JobKind::SubtitleExtract,
            Self::LibraryScan { .. } => JobKind::LibraryScan,
            Self::MetadataRefresh { .. } => JobKind::MetadataRefresh,
            Self::ArtifactVerify { .. } => JobKind::ArtifactVerify,
            Self::ArtworkDerivative { .. } => JobKind::ArtworkDerivative,
            Self::SemanticEmbedding { .. } => JobKind::SemanticEmbedding,
            Self::MediaProbe { .. } => JobKind::MediaProbe,
        }
    }

    pub fn target_node_id(&self) -> Option<&str> {
        match self {
            Self::ArtifactHydrate { target_node_id, .. }
            | Self::ArtifactVerify { target_node_id, .. } => Some(target_node_id),
            _ => None,
        }
    }

    pub const fn requires_voter(&self) -> bool {
        matches!(
            self,
            Self::LibraryScan { .. } | Self::MetadataRefresh { .. }
        )
    }

    pub fn validate(&self) -> Result<(), StoreError> {
        let valid = match self {
            Self::TranscodePrepare {
                file_id,
                source_generation,
                recipe_key,
                target_height,
            } => {
                *file_id > 0
                    && identifier(source_generation)
                    && identifier(recipe_key)
                    && (1..=8_640).contains(target_height)
            }
            Self::FragmentIndexBuild {
                file_id,
                source_generation,
                pipeline_digest,
            }
            | Self::SubtitleExtract {
                file_id,
                source_generation,
                pipeline_digest,
                ..
            } => *file_id > 0 && identifier(source_generation) && digest(pipeline_digest),
            Self::MediaProbe {
                file_id,
                source_generation,
                probe_digest,
            } => *file_id > 0 && identifier(source_generation) && digest(probe_digest),
            Self::ArtifactHydrate {
                artifact_key,
                target_node_id,
            }
            | Self::ArtifactVerify {
                artifact_key,
                target_node_id,
            } => identifier(artifact_key) && identifier(target_node_id),
            Self::LibraryScan {
                library_id,
                generation,
            }
            | Self::MetadataRefresh {
                library_id,
                generation,
            } => *library_id > 0 && identifier(generation),
            Self::ArtworkDerivative {
                artifact_key,
                width,
                height,
            } => {
                identifier(artifact_key)
                    && (1..=8_192).contains(width)
                    && (1..=8_192).contains(height)
            }
            Self::SemanticEmbedding {
                item_id,
                content_digest,
                model_digest,
            } => *item_id > 0 && digest(content_digest) && digest(model_digest),
        };
        if !valid || encode(self)?.len() > MAX_PAYLOAD_BYTES {
            return Err(invalid("invalid background job payload"));
        }
        Ok(())
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn invalid(message: &str) -> StoreError {
    StoreError::Task(message.to_owned())
}

pub(super) fn encode(value: &impl Serialize) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| invalid(&format!("background job JSON: {error}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

/// Ownership revision deliberately excludes scheduling and waiter edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobToken {
    pub job_id: String,
    pub node_id: String,
    pub boot_id: String,
    pub claim_id: String,
    pub fence: i64,
    pub revision: i64,
    pub lease_expires_ms: i64,
}

impl JobToken {
    pub fn validate(&self) -> Result<(), StoreError> {
        if uuid::Uuid::parse_str(&self.job_id).is_err()
            || !identifier(&self.node_id)
            || uuid::Uuid::parse_str(&self.boot_id).is_err()
            || uuid::Uuid::parse_str(&self.claim_id).is_err()
            || self.fence <= 0
            || self.revision <= 0
            || self.lease_expires_ms <= 0
        {
            return Err(invalid("invalid background job ownership token"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundJob {
    pub id: String,
    pub payload_version: i64,
    pub payload: JobPayload,
    pub dedupe_key: String,
    pub priority: u8,
    pub state: JobState,
    pub token: Option<JobToken>,
    pub fence: i64,
    pub revision: i64,
    pub failed_attempts: i64,
    pub not_before_ms: i64,
    pub checkpoint: Option<serde_json::Value>,
    pub result_ref: Option<String>,
    pub last_error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub scope: String,
    pub request_id: String,
    pub request_digest: String,
    pub consumer_kind: String,
    pub consumer_ref: String,
    pub deadline_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnqueueJob {
    pub id: String,
    pub payload: JobPayload,
    pub dedupe_key: String,
    pub priority: u8,
    pub not_before_ms: i64,
    pub now_ms: i64,
    pub request: JobRequest,
}

impl EnqueueJob {
    pub fn validate(&self) -> Result<(), StoreError> {
        self.payload.validate()?;
        if uuid::Uuid::parse_str(&self.id).is_err()
            || !identifier(&self.dedupe_key)
            || self.priority > 3
            || !identifier(&self.request.scope)
            || !identifier(&self.request.request_id)
            || !digest(&self.request.request_digest)
            || !identifier(&self.request.consumer_kind)
            || !identifier(&self.request.consumer_ref)
            || self.now_ms < 0
            || self.not_before_ms < 0
            || self.now_ms.checked_add(REQUEST_RETENTION_MS).is_none()
            || self
                .request
                .deadline_ms
                .is_some_and(|deadline| deadline <= self.now_ms)
        {
            return Err(invalid("invalid background job enqueue request"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EnqueueOutcome {
    Accepted {
        job_id: String,
        receipt_expires_ms: i64,
    },
    Existing {
        job_id: String,
        receipt_expires_ms: i64,
        cancelled: bool,
    },
    Conflict,
    JobCancelling {
        retry_after_ms: i64,
    },
    QueueFull,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimJob {
    pub job_id: String,
    pub expected_revision: i64,
    pub node_id: String,
    pub boot_id: String,
    pub claim_id: String,
    pub kind: JobKind,
    pub payload_version: i64,
    pub now_ms: i64,
    /// Original dispatch time; retries retain this and the claim identity.
    pub dispatched_at_ms: i64,
}

impl ClaimJob {
    pub fn validate(&self) -> Result<(), StoreError> {
        if uuid::Uuid::parse_str(&self.job_id).is_err()
            || !identifier(&self.node_id)
            || uuid::Uuid::parse_str(&self.boot_id).is_err()
            || uuid::Uuid::parse_str(&self.claim_id).is_err()
            || self.expected_revision < 0
            || self.payload_version <= 0
            || self.dispatched_at_ms < 0
            || self.now_ms < self.dispatched_at_ms
            || self.now_ms.saturating_sub(self.dispatched_at_ms) >= CLAIM_RESOLUTION_WINDOW_MS
            || self.now_ms.checked_add(JOB_LEASE_MS).is_none()
        {
            return Err(invalid("invalid or expired background job claim request"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ClaimOutcome {
    Claimed { job: Box<BackgroundJob> },
    Contended,
    ResourceUnavailable,
    Unsupported,
    Cancelled,
    ExpiredOrPruned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveClaim {
    pub job_id: String,
    pub node_id: String,
    pub boot_id: String,
    pub claim_id: String,
    /// Required for renewal resolution; absent only for a lost initial reply.
    pub fence: Option<i64>,
    pub dispatched_at_ms: i64,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ClaimResolution {
    Running {
        token: JobToken,
    },
    CancelRequested {
        cleanup_token: JobToken,
    },
    Settled {
        state: JobState,
        result_ref: Option<String>,
    },
    LostOwnership,
    ExpiredOrPruned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobQuery {
    pub state: Option<JobState>,
    pub kind: Option<JobKind>,
    pub after_id: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPage {
    pub jobs: Vec<BackgroundJob>,
    pub next_after_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewJobs {
    pub tokens: Vec<JobToken>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RenewOutcome {
    Renewed { token: JobToken },
    LostOwnership { job_id: String },
}

/// Execution outcomes that do not publish an artifact. Publication is a
/// domain transaction, and is deliberately not an arbitrary result string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum JobSettlement {
    Yield {
        checkpoint: Option<serde_json::Value>,
        not_before_ms: i64,
    },
    Retry {
        error_code: String,
        not_before_ms: i64,
    },
    Fail {
        error_code: String,
    },
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettleJob {
    pub token: JobToken,
    pub settlement: JobSettlement,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelJob {
    pub job_id: String,
    pub now_ms: i64,
}

/// No generic public enqueue endpoint is implied by this internal boundary.
/// Domain producers authorize the request before attaching a waiter.
#[async_trait]
pub trait BackgroundJobStore: Send + Sync {
    async fn enqueue_job(&self, request: EnqueueJob) -> Result<EnqueueOutcome, StoreError>;
    async fn claim_job(&self, request: ClaimJob) -> Result<ClaimOutcome, StoreError>;
    async fn resolve_claim(&self, request: ResolveClaim) -> Result<ClaimResolution, StoreError>;
    async fn list_jobs(&self, query: JobQuery) -> Result<JobPage, StoreError>;
    async fn renew_jobs(&self, request: RenewJobs) -> Result<Vec<RenewOutcome>, StoreError>;
    async fn settle_job(&self, request: SettleJob) -> Result<Option<BackgroundJob>, StoreError>;
    async fn cancel_job(&self, request: CancelJob) -> Result<Option<BackgroundJob>, StoreError>;
}

/// Narrow internal SQL bridge: callers cannot submit SQL through Store.
/// Authority reads use the replicated leader barrier; listing is advisory.
#[async_trait]
pub(super) trait QueueSql: Send + Sync {
    async fn queue_sql(
        &self,
        sql: String,
        request: String,
        mutation: bool,
        authoritative: bool,
    ) -> Result<Vec<String>, StoreError>;
}

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value)
        .map_err(|error| StoreError::Database(format!("invalid background job row: {error}")))
}

#[async_trait]
impl<T: QueueSql> BackgroundJobStore for T {
    async fn renew_jobs(&self, request: RenewJobs) -> Result<Vec<RenewOutcome>, StoreError> {
        if request.tokens.is_empty()
            || request.tokens.len() > MAX_RENEW_BATCH
            || request.now_ms < 0
            || request.now_ms.checked_add(JOB_LEASE_MS).is_none()
        {
            return Err(invalid("invalid background renewal batch"));
        }
        let mut ids = std::collections::HashSet::new();
        for token in &request.tokens {
            token.validate()?;
            if !ids.insert(&token.job_id) {
                return Err(invalid("duplicate job in background renewal batch"));
            }
        }
        let rows = self
            .queue_sql(RENEW_SQL.to_owned(), encode(&request)?, true, true)
            .await?;
        decode(
            rows.first()
                .ok_or_else(|| invalid("background renewal returned no verdict"))?,
        )
    }

    async fn settle_job(&self, request: SettleJob) -> Result<Option<BackgroundJob>, StoreError> {
        request.token.validate()?;
        if request.now_ms < 0 {
            return Err(invalid("invalid settlement time"));
        }
        match &request.settlement {
            JobSettlement::Yield {
                checkpoint,
                not_before_ms,
            } => {
                if *not_before_ms < request.now_ms
                    || checkpoint
                        .as_ref()
                        .map(encode)
                        .transpose()?
                        .is_some_and(|value| value.len() > MAX_CHECKPOINT_BYTES)
                {
                    return Err(invalid("invalid background checkpoint or yield time"));
                }
            }
            JobSettlement::Retry {
                error_code,
                not_before_ms,
            } => {
                if !identifier(error_code)
                    || error_code.len() > 64
                    || *not_before_ms < request.now_ms
                {
                    return Err(invalid("invalid background failure retry"));
                }
            }
            JobSettlement::Fail { error_code } => {
                if !identifier(error_code) || error_code.len() > 64 {
                    return Err(invalid("invalid background failure code"));
                }
            }
            JobSettlement::Cancel => {}
        }
        let rows = self
            .queue_sql(
                format!("{SETTLE_SQL} RETURNING {JOB_JSON} AS result_json"),
                encode(&request)?,
                true,
                true,
            )
            .await?;
        rows.first().map(|row| decode(row)).transpose()
    }

    async fn cancel_job(&self, request: CancelJob) -> Result<Option<BackgroundJob>, StoreError> {
        if uuid::Uuid::parse_str(&request.job_id).is_err() || request.now_ms < 0 {
            return Err(invalid("invalid background cancellation"));
        }
        let rows = self.queue_sql(format!(
            "UPDATE background_jobs SET state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
                revision = revision + 1, updated_at_ms = json_extract($1, '$.now_ms')
              WHERE id = json_extract($1, '$.job_id') AND state IN ('queued','running')
                AND revision < 9223372036854775807
              RETURNING {JOB_JSON} AS result_json"
        ), encode(&request)?, true, true).await?;
        rows.first().map(|row| decode(row)).transpose()
    }

    async fn enqueue_job(&self, request: EnqueueJob) -> Result<EnqueueOutcome, StoreError> {
        request.validate()?;
        let rows = self
            .queue_sql(ENQUEUE_SQL.to_owned(), encode(&request)?, true, true)
            .await?;
        let row = rows
            .first()
            .ok_or_else(|| invalid("background admission returned no verdict"))?;
        decode(row)
    }

    async fn claim_job(&self, request: ClaimJob) -> Result<ClaimOutcome, StoreError> {
        request.validate()?;
        // Replaying an acknowledged or ambiguous claim must return its current
        // identity, not compete for a fresh fence. Never adopt another boot.
        let previous = self.queue_sql(
            format!("SELECT {JOB_JSON} AS result_json FROM background_jobs WHERE id = json_extract($1, '$.job_id')"),
            encode(&request)?, false, true,
        ).await?;
        let Some(previous) = previous.first() else {
            return Ok(ClaimOutcome::ExpiredOrPruned);
        };
        let previous: BackgroundJob = decode(previous)?;
        if let Some(token) = &previous.token {
            if token.claim_id == request.claim_id {
                return if previous.state == JobState::Running
                    && token.node_id == request.node_id
                    && token.boot_id == request.boot_id
                    && token.lease_expires_ms > request.now_ms
                {
                    Ok(ClaimOutcome::Claimed {
                        job: Box::new(previous),
                    })
                } else {
                    Ok(ClaimOutcome::ExpiredOrPruned)
                };
            }
        }
        if matches!(previous.state, JobState::Cancelling | JobState::Cancelled) {
            return Ok(ClaimOutcome::Cancelled);
        }
        if previous.payload_version != request.payload_version
            || previous.payload.kind() != request.kind
        {
            return Ok(ClaimOutcome::Unsupported);
        }
        let rows = self
            .queue_sql(
                format!("{CLAIM_SQL} RETURNING {JOB_JSON} AS result_json"),
                encode(&request)?,
                true,
                true,
            )
            .await?;
        match rows.first() {
            Some(row) => Ok(ClaimOutcome::Claimed { job: decode(row)? }),
            None => Ok(ClaimOutcome::Contended),
        }
    }

    async fn resolve_claim(&self, request: ResolveClaim) -> Result<ClaimResolution, StoreError> {
        if uuid::Uuid::parse_str(&request.job_id).is_err()
            || uuid::Uuid::parse_str(&request.claim_id).is_err()
            || uuid::Uuid::parse_str(&request.boot_id).is_err()
            || !identifier(&request.node_id)
            || request.now_ms < 0
            || request.fence.is_some_and(|fence| fence <= 0)
        {
            return Err(invalid("invalid background claim resolution"));
        }
        if request.fence.is_none()
            && (request.dispatched_at_ms < 0
                || request.now_ms < request.dispatched_at_ms
                || request.now_ms.saturating_sub(request.dispatched_at_ms)
                    >= CLAIM_RESOLUTION_WINDOW_MS)
        {
            return Ok(ClaimResolution::ExpiredOrPruned);
        }
        let rows = self.queue_sql(format!(
            "SELECT {JOB_JSON} AS result_json FROM background_jobs WHERE id = json_extract($1, '$.job_id')
             AND EXISTS (SELECT 1 FROM background_job_attempts a WHERE a.job_id = background_jobs.id
               AND a.claim_id = json_extract($1, '$.claim_id')
               AND a.owner_node_id = json_extract($1, '$.node_id')
               AND a.owner_boot_id = json_extract($1, '$.boot_id')
               AND (json_extract($1, '$.fence') IS NULL OR a.fence = json_extract($1, '$.fence')))"
        ), encode(&request)?, false, true).await?;
        let Some(row) = rows.first() else {
            return Ok(ClaimResolution::ExpiredOrPruned);
        };
        let job: BackgroundJob = decode(row)?;
        if let Some(token) = job.token {
            if token.claim_id != request.claim_id
                || token.node_id != request.node_id
                || token.boot_id != request.boot_id
                || request.fence.is_some_and(|fence| fence != token.fence)
            {
                return Ok(ClaimResolution::LostOwnership);
            }
            if token.lease_expires_ms <= request.now_ms {
                return Ok(ClaimResolution::ExpiredOrPruned);
            }
            return match job.state {
                JobState::Running => Ok(ClaimResolution::Running { token }),
                JobState::Cancelling => Ok(ClaimResolution::CancelRequested {
                    cleanup_token: token,
                }),
                _ => Err(invalid("non-running job retained ownership")),
            };
        }
        // A terminal outcome belongs to this attempt only if no successor
        // fence has existed, including a successor that has already settled.
        let rows = self.queue_sql(
            "SELECT json_object('fence', fence) AS result_json FROM background_job_attempts
             WHERE job_id = json_extract($1, '$.job_id') AND claim_id = json_extract($1, '$.claim_id')".to_owned(),
            encode(&request)?, false, true,
        ).await?;
        let attempt_fence = rows
            .first()
            .map(|row| decode::<serde_json::Value>(row))
            .transpose()?
            .and_then(|value| value.get("fence").and_then(serde_json::Value::as_i64));
        if attempt_fence != Some(job.fence) || job.state == JobState::Queued {
            return Ok(ClaimResolution::LostOwnership);
        }
        Ok(ClaimResolution::Settled {
            state: job.state,
            result_ref: job.result_ref,
        })
    }

    async fn list_jobs(&self, query: JobQuery) -> Result<JobPage, StoreError> {
        if query.limit == 0 || query.limit > MAX_PAGE_SIZE {
            return Err(invalid("background job page limit must be 1..=128"));
        }
        let rows = self
            .queue_sql(
                format!(
                    "SELECT {JOB_JSON} AS result_json FROM background_jobs
              WHERE (json_extract($1, '$.state') IS NULL OR state = json_extract($1, '$.state'))
                AND (json_extract($1, '$.kind') IS NULL OR kind = json_extract($1, '$.kind'))
                AND (json_extract($1, '$.after_id') IS NULL OR id > json_extract($1, '$.after_id'))
              ORDER BY id LIMIT json_extract($1, '$.limit') + 1"
                ),
                encode(&query)?,
                false,
                false,
            )
            .await?;
        let mut jobs: Vec<BackgroundJob> = rows
            .iter()
            .map(|row| decode(row))
            .collect::<Result<_, _>>()?;
        let more = jobs.len() > query.limit;
        jobs.truncate(query.limit);
        let next_after_id = more
            .then(|| jobs.last().map(|job| job.id.clone()))
            .flatten();
        Ok(JobPage {
            jobs,
            next_after_id,
        })
    }
}
