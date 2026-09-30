//! Durable library intent and per-request results. Paths and identity hints
//! belong to this domain record, never to the generic executable job payload.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::background_jobs::{
    decode, encode, enqueue_body, EnqueueJob, EnqueueOutcome, JobPayload, JobRequest, JobState,
    JobToken, QueueSql, ENQUEUE_SQL,
};
use crate::cluster::coordination::Lease;
use crate::domain::ItemKind;
use crate::error::StoreError;
use crate::scan::TargetedScan;

pub(crate) const SCHEMA: &str = include_str!("background_jobs_library.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 73;
pub const MAX_LIBRARY_REQUESTS: usize = 256;
pub const MAX_LIBRARY_RESULT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryIdHints {
    pub tmdb: Option<i64>,
    pub imdb: Option<String>,
    pub series_tmdb: Option<i64>,
    pub episodeish: bool,
}

impl LibraryIdHints {
    pub fn is_empty(&self) -> bool {
        self.tmdb.is_none() && self.imdb.is_none() && self.series_tmdb.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryBookHints {
    pub title: Option<String>,
    pub author: Option<String>,
    pub medium: ItemKind,
    pub work_id: String,
    pub edition_id: String,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryTrigger {
    Manual,
    Scheduled,
    Startup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LibraryWorkInput {
    Full {
        refresh: bool,
        trigger: LibraryTrigger,
    },
    Targeted {
        path: PathBuf,
        ids: Option<LibraryIdHints>,
        book: Option<Box<LibraryBookHints>>,
        correlation_id: Option<String>,
        source: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewLibraryWork {
    pub request_id: String,
    pub library_id: i64,
    pub input: LibraryWorkInput,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum LibraryWorkResult {
    Completed { scan: TargetedScan },
    Failed { error: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryWorkRecord {
    pub request_id: String,
    pub library_id: i64,
    pub job_id: String,
    pub job_state: Option<JobState>,
    pub job_lease_expires_ms: Option<i64>,
    pub input: LibraryWorkInput,
    pub state: String,
    pub result: Option<LibraryWorkResult>,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryWorkQuery {
    pub job_id: Option<String>,
    pub request_id: Option<String>,
    pub pending_only: bool,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteLibraryWork {
    pub token: JobToken,
    pub lease: Lease,
    pub request_id: String,
    pub result: LibraryWorkResult,
    pub now_ms: i64,
}

pub(super) async fn enqueue<T: QueueSql>(
    store: &T,
    input: NewLibraryWork,
) -> Result<EnqueueOutcome, StoreError> {
    let refresh = matches!(input.input, LibraryWorkInput::Full { refresh: true, .. });
    let priority = match input.input {
        LibraryWorkInput::Full {
            trigger: LibraryTrigger::Scheduled,
            ..
        } => 0,
        LibraryWorkInput::Full {
            trigger: LibraryTrigger::Startup,
            ..
        } => 1,
        _ => 2,
    };
    let input_json = encode(&input.input)?;
    if input_json.len() > 16 * 1024 {
        return Err(StoreError::Task("library request exceeds 16 KiB".into()));
    }
    let digest = hex::encode(Sha256::digest(format!("{}:{input_json}", input.library_id)));
    let generation = "library-work-v1".to_owned();
    let payload = if refresh {
        JobPayload::MetadataRefresh {
            library_id: input.library_id,
            generation,
        }
    } else {
        JobPayload::LibraryScan {
            library_id: input.library_id,
            generation,
        }
    };
    let request = EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload,
        dedupe_key: format!(
            "library:{}:{}",
            if refresh { "metadata" } else { "scan" },
            input.library_id
        ),
        priority,
        not_before_ms: input.now_ms,
        now_ms: input.now_ms,
        request: JobRequest {
            scope: "library".into(),
            request_id: input.request_id,
            request_digest: digest,
            consumer_kind: "library_work".into(),
            consumer_ref: input.library_id.to_string(),
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
    };
    request.validate()?;
    let mut body = enqueue_body(store, &request).await?;
    body["library_request"] =
        serde_json::from_str(&input_json).map_err(|error| StoreError::Task(error.to_string()))?;
    let rows = store
        .queue_sql(ENQUEUE_SQL.into(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing library admission verdict".into()))?,
    )
}

pub(super) async fn list<T: QueueSql>(
    store: &T,
    query: LibraryWorkQuery,
) -> Result<Vec<LibraryWorkRecord>, StoreError> {
    if query.limit == 0
        || query.limit > MAX_LIBRARY_REQUESTS
        || query
            .job_id
            .as_ref()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
        || query
            .request_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 256)
    {
        return Err(StoreError::Task("invalid library request query".into()));
    }
    store.queue_sql(r#"
SELECT json_object('request_id', request.request_id, 'library_id', request.library_id,
    'job_id', request.job_id, 'job_state', job.state, 'job_lease_expires_ms', job.lease_expires_ms,
    'input', json(request.input_json), 'state', waiter.state,
    'result', json(request.result_json), 'error_code', waiter.last_error_code,
    'created_at_ms', waiter.created_at_ms, 'updated_at_ms', waiter.updated_at_ms) AS result_json
FROM background_library_requests request JOIN background_job_waiters waiter
    ON waiter.request_scope = 'library' AND waiter.request_id = request.request_id
LEFT JOIN background_jobs job ON job.id = request.job_id
WHERE (json_extract($1, '$.job_id') IS NULL OR request.job_id = json_extract($1, '$.job_id'))
    AND (json_extract($1, '$.request_id') IS NULL OR request.request_id = json_extract($1, '$.request_id'))
    AND (json_extract($1, '$.pending_only') = 0 OR waiter.state = 'pending')
ORDER BY CASE WHEN json_extract($1, '$.pending_only') = 1 THEN waiter.created_at_ms
    ELSE -waiter.created_at_ms END, request.request_id LIMIT json_extract($1, '$.limit')
"#.into(), encode(&query)?, false, true).await?.iter().map(|row| decode(row)).collect()
}

pub(super) async fn complete<T: QueueSql>(
    store: &T,
    request: CompleteLibraryWork,
) -> Result<bool, StoreError> {
    request.token.validate()?;
    if request.now_ms < 0
        || request.request_id.is_empty()
        || request.request_id.len() > 256
        || encode(&request.result)?.len() > MAX_LIBRARY_RESULT_BYTES
    {
        return Err(StoreError::Task(
            "invalid or oversized library result".into(),
        ));
    }
    let rows = store
        .queue_sql(COMPLETE_SQL.into(), encode(&request)?, true, true)
        .await?;
    Ok(!rows.is_empty())
}

const COMPLETE_SQL: &str = r#"
UPDATE background_library_requests SET result_json = json_extract($1, '$.result'),
    completed_claim_id = json_extract($1, '$.token.claim_id'), completed_at_ms = json_extract($1, '$.now_ms')
WHERE request_id = json_extract($1, '$.request_id') AND job_id = json_extract($1, '$.token.job_id')
    AND result_json IS NULL AND EXISTS (
        SELECT 1 FROM background_job_waiters waiter WHERE waiter.request_scope = 'library'
            AND waiter.request_id = background_library_requests.request_id AND waiter.state = 'pending')
    AND EXISTS (SELECT 1 FROM background_jobs job
        JOIN background_job_domain_leases binding ON binding.job_id = job.id AND binding.job_fence = job.fence
            AND binding.node_id = job.owner_node_id AND binding.boot_id = job.owner_boot_id AND binding.claim_id = job.claim_id
        JOIN job_leases lease ON lease.resource = binding.resource AND lease.fence = binding.domain_fence
        WHERE job.id = background_library_requests.job_id AND job.state = 'running'
            AND job.revision < 9223372036854775807
            AND job.owner_node_id = json_extract($1, '$.token.node_id')
            AND job.owner_boot_id = json_extract($1, '$.token.boot_id')
            AND job.claim_id = json_extract($1, '$.token.claim_id')
            AND job.fence = json_extract($1, '$.token.fence') AND job.revision = json_extract($1, '$.token.revision')
            AND job.lease_expires_ms = json_extract($1, '$.token.lease_expires_ms')
            AND job.lease_expires_ms > json_extract($1, '$.now_ms')
            AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
                WHERE required.job_id = job.id AND NOT EXISTS (
                    SELECT 1 FROM background_job_reservations held
                    WHERE held.job_id = job.id AND held.fence = job.fence
                        AND held.resource_key = required.resource_key
                        AND held.expires_at_ms >= job.lease_expires_ms))
            AND lease.resource = json_extract($1, '$.lease.resource')
            AND lease.owner_node_id = job.owner_node_id
            AND lease.owner_node_id = json_extract($1, '$.lease.owner_node_id')
            AND lease.fence = json_extract($1, '$.lease.fence')
            AND lease.revision = json_extract($1, '$.lease.revision')
            AND lease.expires_at_ms = json_extract($1, '$.lease.expires_at_unix_ms')
            AND lease.expires_at_ms > json_extract($1, '$.now_ms')
            AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || job.owner_node_id))
RETURNING 'true' AS result_json
"#;
