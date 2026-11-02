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
/// A complete-title aggregate the content-analysis pass read while it indexed
/// the file, offered for the title's current demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisTitleOutput {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub scanner_digest: String,
    pub stats: DvTitleStats,
}
/// What became of an analysis pass's aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisTitleOutcome {
    /// Stored for the demand's current revision and scanner. The title's job
    /// reuses it instead of reading the file, and qualifies a FEL title.
    Published,
    /// That revision already holds this scanner's completed answer.
    Current,
    /// Another scanner's job is working on the title: its generation is not
    /// rotated away from under it. The job publishes its own answer.
    Busy,
    /// The catalogue row no longer describes the source the pass read, or the
    /// title is not one the scanner covers.
    NoDemand,
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

/// Offer an analysis pass's aggregate to the title's demand. The demand is
/// first bound to the pass's scanner exactly as the title worker binds it, so
/// a different scanner's generation is rotated away and its aggregate dropped;
/// the aggregate is then stored through the same `publish_dv_title` command a
/// job uses, with no job attached. The demand stays open: the title worker
/// finds this revision's aggregate already stored and settles the job without
/// a second read of the file (and qualifies a FEL title while it holds it).
pub(super) async fn publish_from_analysis<T: QueueSql>(
    store: &T,
    output: AnalysisTitleOutput,
) -> Result<AnalysisTitleOutcome, StoreError> {
    if output.file_id <= 0
        || output.source_size < 0
        || output.source_mtime < 0
        || !digest(&output.scanner_digest)
        || !output.stats.complete
        || output.stats.eligibility() == TitleEligibility::InvalidAggregate
        || encode(&output)?.len() > 64 * 1024
    {
        return Err(StoreError::Task(
            "invalid complete-title analysis publication".into(),
        ));
    }
    #[derive(Deserialize)]
    struct Verdict {
        outcome: String,
        revision: Option<i64>,
    }
    for _ in 0..2 {
        let body = serde_json::json!({"id": uuid::Uuid::new_v4().to_string(), "output": output});
        let rows = store
            .queue_sql(ANALYSIS_PUBLISH_SQL.into(), encode(&body)?, true, true)
            .await?;
        let verdict =
            decode::<Verdict>(rows.first().ok_or_else(|| {
                StoreError::Task("no analysis title publication verdict".into())
            })?)?;
        match (verdict.outcome.as_str(), verdict.revision) {
            ("published", _) => return Ok(AnalysisTitleOutcome::Published),
            ("current", _) => return Ok(AnalysisTitleOutcome::Current),
            ("busy", _) => return Ok(AnalysisTitleOutcome::Busy),
            ("unbound", Some(revision)) => {
                if bind(store, revision, &output.scanner_digest)
                    .await?
                    .is_none()
                {
                    return Ok(AnalysisTitleOutcome::NoDemand);
                }
            }
            _ => return Ok(AnalysisTitleOutcome::NoDemand),
        }
    }
    Ok(AnalysisTitleOutcome::NoDemand)
}

/// `job_id` is null: the command's job and waiter settlements match nothing,
/// and only the probe document receives the aggregate.
const ANALYSIS_PUBLISH_SQL: &str = r#"
WITH input AS (SELECT json($1) AS body), verdict AS (
 SELECT body,d.revision AS revision,CASE
  WHEN d.revision IS NULL OR f.id IS NULL
   OR f.size IS NOT json_extract(body,'$.output.source_size')
   OR f.mtime IS NOT json_extract(body,'$.output.source_mtime')
   OR NOT (f.video_codec='hevc' AND f.dv_profile IN (5,7,8) AND f.dv_rpu_present=1
    AND json_valid(f.probe_json)) THEN 'no_demand'
  WHEN d.scanner_digest IS NOT json_extract(body,'$.output.scanner_digest')
   AND EXISTS(SELECT 1 FROM background_jobs job WHERE job.kind='dv_title_scan'
    AND job.state IN ('queued','running','cancelling')
    AND json_extract(job.payload_json,'$.file_id')=d.file_id) THEN 'busy'
  WHEN d.scanner_digest IS NOT json_extract(body,'$.output.scanner_digest') THEN 'unbound'
  WHEN d.terminal=1 AND json_type(f.probe_json,'$.plurx_dv_title_v1') IS NOT NULL THEN 'current'
  ELSE 'published' END AS outcome
 FROM input LEFT JOIN dv_title_jobs_demand d ON d.file_id=json_extract(body,'$.output.file_id')
 LEFT JOIN files f ON f.id=d.file_id
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract(body,'$.id'),'publish_dv_title',body,
 json_object('outcome',outcome,'job_id',NULL,'revision',revision,
  'file_id',json_extract(body,'$.output.file_id'))
FROM verdict RETURNING result_json
"#;

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

/// Compact advisory catalogue reading. Only bounded title aggregates leave
/// SQLite, never the potentially large ordinary probe document. The same
/// parser/eligibility rule used by rendering owns the eligible count.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TitleReadiness {
    pub scanned: u64,
    pub eligible: u64,
}

pub(super) async fn readiness<T: QueueSql>(store: &T) -> Result<TitleReadiness, StoreError> {
    let rows = store
        .queue_sql(
            r#"
SELECT json_extract(probe_json,'$.plurx_dv_title_v1') AS result_json
FROM files WHERE json_valid(probe_json)
AND json_type(probe_json,'$.plurx_dv_title_v1')='object'
AND json_extract(probe_json,'$.plurx_dv_title_v1.schema')=json_extract($1,'$.schema')
"#
            .into(),
            encode(&serde_json::json!({"schema": crate::scan::dv_title::SCHEMA}))?,
            false,
            false,
        )
        .await?;
    let mut result = TitleReadiness::default();
    for row in rows {
        if let Ok(stats) = decode::<DvTitleStats>(&row) {
            if stats.schema == crate::scan::dv_title::SCHEMA {
                result.scanned += 1;
                result.eligible += u64::from(stats.eligibility() == TitleEligibility::Eligible);
            }
        }
    }
    Ok(result)
}

/// Store a title's FEL qualification verdict beside its Dolby Vision facts in
/// its probe document, only while the measured source is still the row's.
/// A reprobe or source change rewrites the document and drops it.
pub(super) async fn put_fel_verdict<T: QueueSql>(
    store: &T,
    file_id: i64,
    source_size: i64,
    source_mtime: i64,
    verdict: &serde_json::Value,
) -> Result<bool, StoreError> {
    if file_id <= 0 || source_size < 0 || source_mtime < 0 || !verdict.is_object() {
        return Err(StoreError::Task("invalid FEL verdict".into()));
    }
    let body = serde_json::json!({"file_id":file_id,"size":source_size,"mtime":source_mtime,"verdict":verdict});
    if encode(&body)?.len() > 64 * 1024 {
        return Err(StoreError::Task("FEL verdict exceeds its bound".into()));
    }
    let rows = store
        .queue_sql(
            format!(
                "UPDATE files SET probe_json=json_set(probe_json,'$.{}',json(json_extract($1,'$.verdict')))
WHERE id=json_extract($1,'$.file_id') AND size=json_extract($1,'$.size')
 AND mtime=json_extract($1,'$.mtime') AND json_valid(probe_json)
 AND dv_profile=7 AND dv_el_present=1
RETURNING 'true' AS result_json",
                crate::transcode::dv_fel_qualification::PROBE_KEY
            ),
            encode(&body)?,
            true,
            true,
        )
        .await?;
    Ok(!rows.is_empty())
}

/// Ask for one title to be rescanned and re-qualified: rebind its title
/// demand to `binding`, which no scanner digest equals, so the title worker
/// rebinds it to its scanner and runs the title job again. `false` when the
/// title has no demand (not a Dolby Vision title the scanner covers), or when
/// a title job for it is already queued, running or cancelling: rotating the
/// demand would cancel that attempt, and the attempt qualifies the title
/// anyway if it is due.
pub(super) async fn request_requalification<T: QueueSql>(
    store: &T,
    file_id: i64,
    binding: &str,
) -> Result<bool, StoreError> {
    if file_id <= 0 || !digest(binding) {
        return Err(StoreError::Task("invalid FEL re-qualification".into()));
    }
    #[derive(Deserialize)]
    struct Revision {
        revision: i64,
    }
    let rows = store
        .queue_sql(
            "SELECT json_object('revision',d.revision) AS result_json FROM dv_title_jobs_demand d
WHERE d.file_id=json_extract($1,'$.file_id')
 AND NOT EXISTS(SELECT 1 FROM background_jobs job WHERE job.kind='dv_title_scan'
  AND job.state IN ('queued','running','cancelling')
  AND json_extract(job.payload_json,'$.file_id')=d.file_id)"
                .into(),
            encode(&serde_json::json!({"file_id": file_id}))?,
            false,
            true,
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    let revision = decode::<Revision>(row)?.revision;
    Ok(bind(store, revision, binding).await?.is_some())
}

/// Settled Profile 7 FEL titles whose stored verdict was measured under
/// another FEL identity (renderer, recipe or encoder), with no title job in
/// flight: what a renderer change has to re-qualify. Bounded, oldest
/// generation first.
pub(super) async fn stale_fel_titles<T: QueueSql>(
    store: &T,
    identity: &str,
    limit: u32,
) -> Result<Vec<i64>, StoreError> {
    if !digest(identity) || limit == 0 || limit > 128 {
        return Err(StoreError::Task("invalid FEL staleness query".into()));
    }
    #[derive(Deserialize)]
    struct Title {
        file_id: i64,
    }
    let rows = store
        .queue_sql(
            format!(
                "SELECT json_object('file_id',f.id) AS result_json
FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id
WHERE d.terminal=1 AND f.dv_profile=7 AND f.dv_el_present=1 AND json_valid(f.probe_json)
 AND json_type(f.probe_json,'$.{key}')='object'
 AND json_extract(f.probe_json,'$.{key}.identity') IS NOT json_extract($1,'$.identity')
 AND NOT EXISTS(SELECT 1 FROM background_jobs job WHERE job.kind='dv_title_scan'
  AND job.state IN ('queued','running','cancelling')
  AND json_extract(job.payload_json,'$.file_id')=f.id)
ORDER BY d.revision LIMIT json_extract($1,'$.limit')",
                key = crate::transcode::dv_fel_qualification::PROBE_KEY
            ),
            encode(&serde_json::json!({"identity": identity, "limit": limit}))?,
            false,
            true,
        )
        .await?;
    rows.iter()
        .map(|row| decode::<Title>(row).map(|title| title.file_id))
        .collect()
}

/// How many titles hold a FEL verdict for `identity`, and how many of those
/// admit reconstruction. Advisory (Developer tab) only. Walks the Dolby
/// Vision title demand (one row per DV title) and reads each title's row by
/// its key, so it never scans the whole catalogue.
pub(super) async fn fel_verdict_counts<T: QueueSql>(
    store: &T,
    identity: &str,
) -> Result<(u64, u64), StoreError> {
    let rows = store
        .queue_sql(
            format!(
                "SELECT json_extract(f.probe_json,'$.{key}') AS result_json
FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id
WHERE f.dv_profile=7 AND f.dv_el_present=1 AND json_valid(f.probe_json)
 AND json_type(f.probe_json,'$.{key}')='object'
 AND json_extract(f.probe_json,'$.{key}.identity')=json_extract($1,'$.identity')",
                key = crate::transcode::dv_fel_qualification::PROBE_KEY
            ),
            encode(&serde_json::json!({"identity": identity}))?,
            false,
            false,
        )
        .await?;
    let mut counts = (0, 0);
    for row in rows {
        if let Ok(verdict) = decode::<crate::transcode::dv_fel_qualification::FelVerdict>(&row) {
            counts.0 += 1;
            counts.1 += u64::from(verdict.admitted);
        }
    }
    Ok(counts)
}
