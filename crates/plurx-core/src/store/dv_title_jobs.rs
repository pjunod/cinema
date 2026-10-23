//! Durable complete-title acquisition, independent of ordinary probe leases.
use super::background_jobs::{decode, digest, encode, JobPublishOutcome, JobToken, QueueSql};
use crate::{
    error::StoreError,
    scan::dv_title::{DvTitleStats, TitleEligibility},
};
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = include_str!("dv_title_jobs.sql");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleDemand {
    pub file_id: i64,
    /// Monotonic source/probe/scanner demand generation, not a content hash.
    pub probe_revision: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub probe_json: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleOutput {
    pub file_id: i64,
    pub probe_revision: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub scanner_digest: String,
    pub stats: DvTitleStats,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishTitleJob {
    pub token: JobToken,
    pub output: TitleOutput,
    pub now_ms: i64,
}

pub(super) async fn demands<T: QueueSql>(
    store: &T,
    scanner: &str,
    after: i64,
) -> Result<Vec<TitleDemand>, StoreError> {
    if !digest(scanner) || after < 0 {
        return Err(StoreError::Task("invalid title demand query".into()));
    }
    let rows = store.queue_sql(r#"
SELECT json_object('file_id',file.id,'probe_revision',d.revision,
 'source_size',file.size,'source_mtime',file.mtime,'probe_json',file.probe_json) AS result_json
FROM dv_title_jobs_demand d JOIN files file ON file.id=d.file_id
WHERE d.revision>json_extract($1,'$.after') AND (d.terminal=0 OR d.scanner_digest IS NOT json_extract($1,'$.scanner'))
ORDER BY d.revision LIMIT 128
"#.into(), encode(&serde_json::json!({"scanner":scanner,"after":after}))?, false, true).await?;
    rows.iter().map(|row| decode(row)).collect()
}

/// Bind a trusted installed scanner to this revision. The command atomically
/// invalidates old scanner work without publishing fresh ordinary probe facts.
pub(super) async fn bind<T: QueueSql>(
    store: &T,
    revision: i64,
    scanner: &str,
) -> Result<Option<i64>, StoreError> {
    if revision <= 0 || !digest(scanner) {
        return Err(StoreError::Task("invalid title scanner binding".into()));
    }
    let body = serde_json::json!({"id":uuid::Uuid::new_v4().to_string(),"revision":revision,"scanner":scanner});
    let rows=store.queue_sql(r#"
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract($1,'$.id'),'bind_dv_title',json($1),json_object(
 'file_id',file_id,'previous_revision',revision,
 'revision',CASE WHEN scanner_digest IS NOT NULL AND scanner_digest IS NOT json_extract($1,'$.scanner')
  THEN (SELECT seq+1 FROM sqlite_sequence WHERE name='dv_title_jobs_demand') ELSE revision END,
 'rotate',scanner_digest IS NOT NULL AND scanner_digest IS NOT json_extract($1,'$.scanner'),
 'changed',scanner_digest IS NOT json_extract($1,'$.scanner'),'bound',json('true'))
FROM dv_title_jobs_demand WHERE revision=json_extract($1,'$.revision')
 AND (scanner_digest IS NULL OR scanner_digest IS json_extract($1,'$.scanner')
  OR (SELECT seq FROM sqlite_sequence WHERE name='dv_title_jobs_demand')<9223372036854775807)
RETURNING result_json
"#.into(),encode(&body)?,true,true).await?;
    #[derive(Deserialize)]
    struct Bound {
        revision: i64,
    }
    rows.first()
        .map(|row| decode::<Bound>(row).map(|bound| bound.revision))
        .transpose()
}

pub(super) async fn publish<T: QueueSql>(
    store: &T,
    input: PublishTitleJob,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    let out = &input.output;
    if input.now_ms < 0
        || out.file_id <= 0
        || out.probe_revision <= 0
        || out.source_size < 0
        || out.source_mtime < 0
        || !digest(&out.scanner_digest)
        || !out.stats.complete
        || out.stats.eligibility() == TitleEligibility::InvalidAggregate
        || encode(out)?.len() > 64 * 1024
    {
        return Err(StoreError::Task(
            "invalid complete-title publication".into(),
        ));
    }
    let body = serde_json::to_value(input).map_err(|e| StoreError::Task(e.to_string()))?;
    let rows = store
        .queue_sql(PUBLISH_SQL.into(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("no title publication verdict".into()))?,
    )
}

/// Conservative scheduler handoff, never a claim or publication authority.
/// The worker also checks its exact reporter and readable physical source.
pub(super) async fn probe_can_preempt<T: QueueSql>(
    store: &T,
    probe_id: &str,
    title_id: &str,
    node: &str,
    now: i64,
) -> Result<bool, StoreError> {
    if uuid::Uuid::parse_str(probe_id).is_err()
        || uuid::Uuid::parse_str(title_id).is_err()
        || !super::background_jobs::identifier(node)
        || now < 0
    {
        return Err(StoreError::Task(
            "invalid title priority observation".into(),
        ));
    }
    let rows=store.queue_sql(r#"
SELECT 'true' AS result_json FROM background_jobs job
JOIN background_jobs title ON title.id=json_extract($1,'$.title') AND title.kind='dv_title_scan'
 AND title.state='running' AND title.owner_node_id=json_extract($1,'$.node')
 AND title.lease_expires_ms>json_extract($1,'$.now')
WHERE job.id=json_extract($1,'$.probe') AND job.kind='media_probe' AND job.payload_version=1
 AND job.priority>title.priority AND (job.target_node_id IS NULL OR job.target_node_id=json_extract($1,'$.node'))
 AND (job.state='queued' OR (job.state='running' AND job.lease_expires_ms<=json_extract($1,'$.now')))
 AND job.not_before_ms<=json_extract($1,'$.now')
 AND (job.retry_deadline_ms=0 OR job.retry_deadline_ms>json_extract($1,'$.now'))
 AND job.failed_attempts+CASE WHEN job.state='running' THEN 1 ELSE 0 END<job.attempt_limit
 AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||json_extract($1,'$.node'))
 AND EXISTS(SELECT 1 FROM background_job_waiters w WHERE w.job_id=job.id AND w.state='pending'
  AND (w.deadline_ms IS NULL OR w.deadline_ms>json_extract($1,'$.now')))
 AND EXISTS(SELECT 1 FROM files f JOIN items i ON i.id=f.item_id WHERE f.id=json_extract(job.payload_json,'$.file_id')
  AND f.size=json_extract(job.payload_json,'$.source_size') AND f.mtime=json_extract(job.payload_json,'$.source_mtime')
  AND (json_extract(job.payload_json,'$.coordinator.resource')='repair:probe'
   OR json_extract(job.payload_json,'$.coordinator.resource')='scan:library:'||i.library_id))
 AND EXISTS(SELECT 1 FROM job_leases lease WHERE lease.resource=json_extract(job.payload_json,'$.coordinator.resource')
  AND lease.owner_node_id=json_extract(job.payload_json,'$.coordinator.node_id') AND lease.fence=json_extract(job.payload_json,'$.coordinator.fence')
  AND lease.expires_at_ms>json_extract($1,'$.now')
  AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||lease.owner_node_id)
  AND NOT EXISTS(SELECT 1 FROM background_job_domain_leases b WHERE b.resource=lease.resource
   AND (b.domain_fence!=lease.fence OR NOT EXISTS(SELECT 1 FROM background_jobs parent WHERE parent.id=b.job_id
    AND parent.fence=b.job_fence AND parent.state='running' AND parent.owner_node_id=b.node_id
    AND parent.owner_boot_id=b.boot_id AND parent.claim_id=b.claim_id AND parent.lease_expires_ms>json_extract($1,'$.now')))))
 -- Do not interrupt for work blocked on another reader. This deliberately
 -- under-admits; the real claim retains the common resource authority.
 AND NOT EXISTS(SELECT 1 FROM background_job_required_resources r WHERE r.job_id=job.id
  AND (EXISTS(SELECT 1 FROM background_job_reservations h WHERE h.resource_key=r.resource_key
   AND h.job_id!=title.id AND h.expires_at_ms>json_extract($1,'$.now'))
  OR EXISTS(SELECT 1 FROM analysis_source_reservations h WHERE h.resource_key=r.resource_key
   AND h.expires_at_ms>json_extract($1,'$.now'))))
"#.into(),encode(&serde_json::json!({"probe":probe_id,"title":title_id,"node":node,"now":now}))?,false,true).await?;
    Ok(!rows.is_empty())
}

const PUBLISH_SQL: &str = r#"
WITH input AS (SELECT json($1) AS body), observation AS (
 SELECT body,job.id,job.state,job.result_ref,json_extract(body,'$.output') AS output,
 EXISTS(SELECT 1 FROM background_job_attempts a WHERE a.job_id=job.id
  AND a.fence=job.fence AND a.fence=json_extract(body,'$.token.fence') AND a.claim_id=json_extract(body,'$.token.claim_id')
  AND a.owner_node_id=json_extract(body,'$.token.node_id') AND a.owner_boot_id=json_extract(body,'$.token.boot_id')) AS same_attempt,
 job.state='running' AND job.kind='dv_title_scan' AND job.payload_version=1
 AND job.owner_node_id=json_extract(body,'$.token.node_id') AND job.owner_boot_id=json_extract(body,'$.token.boot_id')
 AND job.claim_id=json_extract(body,'$.token.claim_id') AND job.fence=json_extract(body,'$.token.fence')
 AND job.revision=json_extract(body,'$.token.revision') AND job.revision<9223372036854775807
 AND job.lease_expires_ms=json_extract(body,'$.token.lease_expires_ms') AND job.lease_expires_ms>json_extract(body,'$.now_ms')
 AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||job.owner_node_id)
 AND NOT EXISTS(SELECT 1 FROM background_job_required_resources r WHERE r.job_id=job.id
  AND NOT EXISTS(SELECT 1 FROM background_job_reservations h WHERE h.job_id=job.id AND h.fence=job.fence
   AND h.resource_key=r.resource_key AND h.expires_at_ms>=job.lease_expires_ms))
 AND EXISTS(SELECT 1 FROM background_job_waiters w WHERE w.job_id=job.id AND w.state='pending'
  AND (w.deadline_ms IS NULL OR w.deadline_ms>json_extract(body,'$.now_ms')))
 AND json_extract(job.payload_json,'$.file_id')=json_extract(body,'$.output.file_id')
 AND json_extract(job.payload_json,'$.probe_revision')=json_extract(body,'$.output.probe_revision')
 AND json_extract(job.payload_json,'$.source_size')=json_extract(body,'$.output.source_size')
 AND json_extract(job.payload_json,'$.source_mtime')=json_extract(body,'$.output.source_mtime')
 AND json_extract(job.payload_json,'$.scanner_digest')=json_extract(body,'$.output.scanner_digest')
 AND EXISTS(SELECT 1 FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id
  WHERE d.revision=json_extract(body,'$.output.probe_revision') AND d.file_id=json_extract(body,'$.output.file_id')
  AND d.scanner_digest=json_extract(body,'$.output.scanner_digest') AND d.terminal=0
  AND f.size=json_extract(body,'$.output.source_size') AND f.mtime=json_extract(body,'$.output.source_mtime')
  AND f.video_codec='hevc' AND f.dv_profile IN (5,7,8) AND f.dv_rpu_present=1 AND json_valid(f.probe_json)) AS owns
 FROM input LEFT JOIN background_jobs job ON job.id=json_extract(body,'$.token.job_id')
), verdict AS (
 SELECT *,CASE WHEN state='succeeded' AND same_attempt AND result_ref=output THEN 'already_published'
 WHEN NOT COALESCE(owns,0) THEN 'lost_ownership' ELSE 'published' END AS outcome FROM observation
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract(body,'$.token.claim_id'),'publish_dv_title',body,
 CASE WHEN outcome IN ('published','already_published') THEN json_object('outcome',outcome,'job_id',id,
 'file_id',json_extract(body,'$.output.file_id'),'result_ref',output||'') ELSE json_object('outcome',outcome) END
FROM verdict RETURNING result_json
"#;
