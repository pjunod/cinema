//! Bind a catalogue publication lease to one durable execution attempt.
//!
//! The two leases have independent renewal revisions. The binding names their
//! stable fences; every catalogue mutation checks the current queue expiry in
//! the same transaction as the existing exact domain-token check.
use serde::{Deserialize, Serialize};

use super::background_jobs::{encode, JobToken, QueueSql};
use crate::cluster::coordination::Lease;
use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("background_jobs_domain.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 72;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindLibraryJob {
    pub token: JobToken,
    pub lease: Lease,
    pub now_ms: i64,
}

const BIND_SQL: &str = r#"
INSERT INTO background_job_domain_leases
    (resource, domain_fence, job_id, job_fence, node_id, boot_id, claim_id)
SELECT lease.resource, lease.fence, job.id, job.fence,
    job.owner_node_id, job.owner_boot_id, job.claim_id
FROM background_jobs job JOIN job_leases lease
    ON lease.resource = json_extract($1, '$.lease.resource')
WHERE job.id = json_extract($1, '$.token.job_id')
    AND job.kind IN ('library_scan', 'metadata_refresh') AND job.payload_version = 1
    AND lease.resource = 'scan:library:' || json_extract(job.payload_json, '$.library_id')
    AND job.state = 'running'
    AND job.owner_node_id = json_extract($1, '$.token.node_id')
    AND job.owner_boot_id = json_extract($1, '$.token.boot_id')
    AND job.claim_id = json_extract($1, '$.token.claim_id')
    AND job.fence = json_extract($1, '$.token.fence')
    AND job.revision = json_extract($1, '$.token.revision')
    AND job.lease_expires_ms = json_extract($1, '$.token.lease_expires_ms')
    AND job.lease_expires_ms > json_extract($1, '$.now_ms')
            AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
                WHERE required.job_id = job.id AND NOT EXISTS (
                    SELECT 1 FROM background_job_reservations held
                    WHERE held.job_id = job.id AND held.fence = job.fence
                        AND held.resource_key = required.resource_key
                        AND held.expires_at_ms >= job.lease_expires_ms))
    AND lease.owner_node_id = job.owner_node_id
    AND lease.owner_node_id = json_extract($1, '$.lease.owner_node_id')
    AND lease.fence = json_extract($1, '$.lease.fence')
    AND lease.revision = json_extract($1, '$.lease.revision')
    AND lease.expires_at_ms = json_extract($1, '$.lease.expires_at_unix_ms')
    AND lease.expires_at_ms > json_extract($1, '$.now_ms')
    AND NOT EXISTS (SELECT 1 FROM settings WHERE key =
        'internal.cluster_job_owner_removed.' || job.owner_node_id)
ON CONFLICT(resource) DO UPDATE SET
    domain_fence = excluded.domain_fence, job_id = excluded.job_id,
    job_fence = excluded.job_fence, node_id = excluded.node_id,
    boot_id = excluded.boot_id, claim_id = excluded.claim_id
WHERE background_job_domain_leases.domain_fence < excluded.domain_fence
    OR (background_job_domain_leases.domain_fence = excluded.domain_fence
        AND background_job_domain_leases.job_id = excluded.job_id
        AND background_job_domain_leases.job_fence = excluded.job_fence
        AND background_job_domain_leases.node_id = excluded.node_id
        AND background_job_domain_leases.boot_id = excluded.boot_id
        AND background_job_domain_leases.claim_id = excluded.claim_id)
RETURNING 'true' AS result_json
"#;

pub(super) async fn bind<T: QueueSql>(
    store: &T,
    request: BindLibraryJob,
) -> Result<bool, StoreError> {
    request.token.validate()?;
    if request.now_ms < 0 || !request.lease.resource.starts_with("scan:library:") {
        return Err(StoreError::Task("invalid library job binding".into()));
    }
    Ok(!store
        .queue_sql(BIND_SQL.into(), encode(&request)?, true, true)
        .await?
        .is_empty())
}
