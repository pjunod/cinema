//! Replicated pacing for library, artwork and genre provider calls.
use super::background_jobs::{decode, encode, QueueSql};
use crate::{cluster::coordination::Lease, error::StoreError, metadata::Provider};
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = include_str!("background_jobs_provider.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 75;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderBudgetAction {
    Charge,
    Observe {
        cooldown_ms: i64,
        interval_ms: Option<i64>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderBudgetRequest {
    pub provider: Provider,
    pub lease: Lease,
    pub now_ms: i64,
    pub action: ProviderBudgetAction,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProviderBudgetOutcome {
    Charged,
    Observed,
    Wait { until_ms: i64 },
    LostAuthority,
}

const UPDATE_SQL: &str = r#"
INSERT INTO background_job_commands(id, operation, request_json, result_json)
SELECT json_extract($1, '$.id'), 'provider_budget', $1,
    CASE WHEN NOT EXISTS (
        SELECT 1 FROM job_leases lease
        WHERE lease.resource = json_extract($1, '$.lease.resource')
          AND lease.owner_node_id = json_extract($1, '$.lease.owner_node_id')
          AND lease.fence = json_extract($1, '$.lease.fence')
          AND lease.revision = json_extract($1, '$.lease.revision')
          AND lease.expires_at_ms = json_extract($1, '$.lease.expires_at_unix_ms')
          AND lease.expires_at_ms > json_extract($1, '$.now_ms')
          AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || lease.owner_node_id)
          AND NOT EXISTS (SELECT 1 FROM background_job_domain_leases binding
            WHERE binding.resource = lease.resource AND NOT EXISTS (
              SELECT 1 FROM background_jobs job WHERE job.id = binding.job_id
                AND binding.domain_fence = lease.fence AND job.fence = binding.job_fence
                AND job.owner_node_id = binding.node_id AND job.owner_node_id = lease.owner_node_id
                AND job.owner_boot_id = binding.boot_id AND job.claim_id = binding.claim_id
                AND job.state = 'running' AND job.lease_expires_ms > json_extract($1, '$.now_ms')))
    ) THEN json_object('outcome', 'lost_authority')
    WHEN json_extract($1, '$.action.kind') = 'observe' THEN json_object('outcome', 'observed')
    WHEN budget.next_dispatch_ms > json_extract($1, '$.now_ms') THEN json_object('outcome', 'wait', 'until_ms', budget.next_dispatch_ms)
    ELSE json_object('outcome', 'charged') END
FROM (SELECT json_extract($1, '$.provider') AS provider) requested
LEFT JOIN background_provider_budgets budget ON budget.provider = requested.provider
RETURNING result_json
"#;

pub(super) async fn update<T: QueueSql>(
    store: &T,
    request: ProviderBudgetRequest,
) -> Result<ProviderBudgetOutcome, StoreError> {
    if request.now_ms < 0
        || !(request.lease.resource.starts_with("scan:library:")
            || matches!(
                request.lease.resource.as_str(),
                "provider:artwork" | "provider:genres"
            ))
        || matches!(request.action, ProviderBudgetAction::Observe { cooldown_ms, interval_ms }
            if !(0..=86_400_000).contains(&cooldown_ms) || interval_ms.is_some_and(|ms| !(100..=60_000).contains(&ms)))
    {
        return Err(StoreError::Task(
            "invalid provider allowance request".into(),
        ));
    }
    let mut body =
        serde_json::to_value(&request).map_err(|error| StoreError::Task(error.to_string()))?;
    body["id"] = uuid::Uuid::new_v4().to_string().into();
    let body = encode(&body)?;
    if matches!(request.action, ProviderBudgetAction::Charge) {
        // A local hint can only defer work. The due path always uses the
        // authoritative single-statement verdict below; stale reads cannot
        // manufacture credit or charge a refused owner.
        let rows = store.queue_sql("SELECT CAST(next_dispatch_ms AS TEXT) AS result_json FROM background_provider_budgets WHERE provider = json_extract($1, '$.provider')".into(), body.clone(), false, false).await?;
        if let Some(row) = rows.first() {
            let until_ms: i64 = decode(row)?;
            if until_ms > request.now_ms {
                return Ok(ProviderBudgetOutcome::Wait { until_ms });
            }
        }
    }
    let rows = store.queue_sql(UPDATE_SQL.into(), body, true, true).await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing provider allowance".into()))?,
    )
}
