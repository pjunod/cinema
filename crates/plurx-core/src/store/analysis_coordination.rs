//! Metadata-only fragment-index coordination under the existing Analysis lease.
//! Physical phases reuse existing custody UUIDs, activation and settlement.
use super::background_jobs::{decode, encode, identifier, QueueSql};
use super::background_jobs_resources::{BackgroundSourceCustodyToken, BackgroundSourceOwner};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = concat!(
    include_str!("analysis_coordination.sql"),
    "\n-- next statement\n",
    include_str!("copy_index_v3.sql"),
);
pub const SOURCE_COPY_COORDINATION_CAPABILITY: &str = "source_copy_coordination_v1";
pub const PIPELINE_PREFIX: &str = "coordination-contract:";
pub const VIDEO_PREFIX: &str = "coordination-video:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisCoordinationSource {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub origin_node_id: String,
    /// Exact held-object witness in the origin node's namespace, not mtime.
    pub object_version: String,
    pub video_identity: String,
    pub contract_version: u32,
    /// Stable across ingresses; a forced successor uses a new generation.
    pub requested_generation: String,
}
impl AnalysisCoordinationSource {
    pub fn pipeline_marker(&self) -> String {
        format!("{PIPELINE_PREFIX}{}", self.contract_version)
    }
    pub fn video_marker(&self) -> String {
        format!(
            "{VIDEO_PREFIX}{}:{}",
            self.contract_version, self.video_identity
        )
    }
    pub(super) fn valid(&self) -> bool {
        self.file_id > 0
            && self.source_size >= 0
            && identifier(&self.origin_node_id)
            && !self.object_version.is_empty()
            && self.object_version.len() <= 4096
            && !self.video_identity.is_empty()
            && self.video_identity.len() <= 96
            && self
                .video_identity
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            && self.contract_version > 0
            && !self.requested_generation.is_empty()
            && self.requested_generation.len() <= 128
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisCoordinationClaim {
    pub request_id: String,
    pub node_id: String,
    pub fence: i64,
}
impl AnalysisCoordinationClaim {
    fn valid(&self) -> bool {
        super::fragment_index_cluster::analysis_request_id_is_valid(&self.request_id)
            && identifier(&self.node_id)
            && self.fence > 0
    }
    pub fn physical_owner(&self) -> BackgroundSourceOwner {
        BackgroundSourceOwner::Analysis {
            request_id: self.request_id.clone(),
            node_id: self.node_id.clone(),
            fence: self.fence,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisCoordination {
    pub request_id: String,
    pub source: AnalysisCoordinationSource,
    pub phase_sequence: i64,
    pub active_custody_id: Option<String>,
    pub physical_node_id: Option<String>,
    pub physical_fence: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisPhysicalPhase {
    pub claim: AnalysisCoordinationClaim,
    pub source: AnalysisCoordinationSource,
    pub phase_sequence: i64,
    pub custody: BackgroundSourceCustodyToken,
}
impl AnalysisPhysicalPhase {
    pub(crate) fn valid(&self) -> bool {
        self.claim.valid()
            && self.source.valid()
            && self.phase_sequence > 0
            && uuid::Uuid::parse_str(&self.custody.custody_id).is_ok()
            && matches!(&self.custody.owner, BackgroundSourceOwner::Analysis { request_id, node_id, fence }
                if request_id == &self.claim.request_id && node_id == &self.claim.node_id && fence == &self.claim.fence)
    }
}

/// Only the existing Source factories can construct these authorities. A
/// preparation snapshot is replaced with fresh active-route authority after
/// activation; a persisted association alone grants no reads or serving.
#[derive(Clone)]
pub enum AnalysisSourceInterestAuthority {
    Preparing(Box<crate::sharing_source_sessions::SourceSessionWriteAuthority>),
    Active(Box<crate::sharing_source_sessions::SourceOwnedRouteAuthority>),
}

fn invalid() -> StoreError {
    StoreError::Task("invalid analysis coordination operation".into())
}
fn source_match(role: &str) -> String {
    [
        "file_id",
        "source_size",
        "source_mtime",
        "origin_node_id",
        "object_version",
        "video_identity",
        "contract_version",
        "requested_generation",
    ]
    .into_iter()
    .map(|field| format!("{role}.{field}=json_extract($1,'$.source.{field}')"))
    .collect::<Vec<_>>()
    .join(" AND ")
}
fn owned_request() -> &'static str {
    "request.request_id=json_extract($1,'$.claim.request_id') AND request.owner_node_id=json_extract($1,'$.claim.node_id') AND request.fence=json_extract($1,'$.claim.fence') AND request.state='running' AND request.cancel_requested=0 AND request.lease_expires_ms>json_extract($1,'$.now_ms') AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||request.owner_node_id)"
}
const OBSERVATION: &str = "SELECT json_object('request_id',request_id,'source',json_object('file_id',file_id,'source_size',source_size,'source_mtime',source_mtime,'origin_node_id',origin_node_id,'object_version',object_version,'video_identity',video_identity,'contract_version',contract_version,'requested_generation',requested_generation),'phase_sequence',phase_sequence,'active_custody_id',active_custody_id,'physical_node_id',physical_node_id,'physical_fence',physical_fence) AS result_json FROM analysis_coordination_roles";

pub(super) async fn observe<T: QueueSql>(
    store: &T,
    request_id: &str,
) -> Result<Option<AnalysisCoordination>, StoreError> {
    if !super::fragment_index_cluster::analysis_request_id_is_valid(request_id) {
        return Err(invalid());
    }
    let rows = store
        .queue_sql(
            format!("{OBSERVATION} WHERE request_id=json_extract($1,'$.request_id')"),
            encode(&serde_json::json!({"request_id":request_id}))?,
            false,
            true,
        )
        .await?;
    rows.first().map(|row| decode(row)).transpose()
}

/// Establish only an explicitly new queued protocol row. A running legacy
/// reader is never converted or uncharged. Exact source/recipe identities
/// rendezvous across target nodes; unproven cross-node objects stay distinct.
pub(super) async fn establish<T: QueueSql>(
    store: &T,
    request_id: &str,
    source: AnalysisCoordinationSource,
    now_ms: i64,
) -> Result<Option<AnalysisCoordination>, StoreError> {
    if !source.valid()
        || now_ms < 0
        || !super::fragment_index_cluster::analysis_request_id_is_valid(request_id)
    {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"request_id":request_id,"source":source,"pipeline":source.pipeline_marker(),"video":source.video_marker(),"now_ms":now_ms}),
    )?;
    let insert="INSERT INTO analysis_coordination_roles(request_id,file_id,source_size,source_mtime,origin_node_id,object_version,video_identity,contract_version,requested_generation)
      SELECT request.request_id,request.file_id,request.source_size,request.source_mtime,json_extract($1,'$.source.origin_node_id'),json_extract($1,'$.source.object_version'),json_extract($1,'$.source.video_identity'),json_extract($1,'$.source.contract_version'),request.requested_generation
      FROM analysis_requests request JOIN files file ON file.id=request.file_id AND file.size=request.source_size AND file.mtime=request.source_mtime
      WHERE request.request_id=json_extract($1,'$.request_id') AND request.state='queued' AND request.fence=0 AND request.cancel_requested=0
        AND request.component='fragment_index' AND request.pipeline_version=json_extract($1,'$.pipeline') AND request.video_identity=json_extract($1,'$.video')
        AND request.requested_generation=json_extract($1,'$.source.requested_generation') AND request.file_id=json_extract($1,'$.source.file_id') AND request.source_size=json_extract($1,'$.source.source_size') AND request.source_mtime=json_extract($1,'$.source.source_mtime')
      ON CONFLICT DO NOTHING";
    let retire=format!("UPDATE analysis_requests SET state='cancelled',fence=fence+1,cancel_requested=1,last_error_code='coordination_joined',updated_at_ms=json_extract($1,'$.now_ms') WHERE request_id=json_extract($1,'$.request_id') AND state='queued' AND fence=0 AND pipeline_version=json_extract($1,'$.pipeline') AND video_identity=json_extract($1,'$.video') AND requested_generation=json_extract($1,'$.source.requested_generation') AND file_id=json_extract($1,'$.source.file_id') AND source_size=json_extract($1,'$.source.source_size') AND source_mtime=json_extract($1,'$.source.source_mtime') AND EXISTS(SELECT 1 FROM analysis_coordination_roles role WHERE role.request_id<>analysis_requests.request_id AND {})",source_match("role"));
    // Rendezvous uniqueness belongs to live coordinators only. A role whose
    // request already ended (failed, cancelled, ready) and holds no physical
    // phase can never be claimed again, so it must not capture the source.
    let release = format!("DELETE FROM analysis_coordination_roles WHERE {} AND active_custody_id IS NULL AND EXISTS(SELECT 1 FROM analysis_requests request WHERE request.request_id=analysis_coordination_roles.request_id AND request.state NOT IN ('queued','running','submitted'))",source_match("analysis_coordination_roles"));
    let attempt = store
        .queue_transaction(vec![
            (release, body.clone()),
            (insert.into(), body.clone()),
            (retire, body.clone()),
        ])
        .await;
    let rows = store
        .queue_sql(
            format!("{OBSERVATION} AS role WHERE {}", source_match("role")),
            body,
            false,
            true,
        )
        .await?;
    if let Some(row) = rows.first() {
        return decode(row).map(Some);
    }
    attempt?;
    Ok(None)
}

/// The permit is installed, consumed by the guarded request CAS, and cleared
/// in one transaction. This is the existing Analysis fence/lease, not a second
/// metadata claim service. Metadata claiming reserves zero source readers.
pub(super) async fn claim<T: QueueSql>(
    store: &T,
    request_id: &str,
    source: AnalysisCoordinationSource,
    node_id: &str,
    expected_fence: i64,
    now_ms: i64,
    expires_ms: i64,
) -> Result<Option<AnalysisCoordinationClaim>, StoreError> {
    if !source.valid()
        || !identifier(node_id)
        || !super::fragment_index_cluster::analysis_request_id_is_valid(request_id)
        || expected_fence < 0
        || expected_fence == i64::MAX
        || now_ms < 0
        || expires_ms <= now_ms
    {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"request_id":request_id,"source":source,"node_id":node_id,"expected_fence":expected_fence,"now_ms":now_ms,"expires_ms":expires_ms}),
    )?;
    let permit=format!("UPDATE analysis_coordination_roles AS role SET claim_permit_node=json_extract($1,'$.node_id'),claim_permit_fence=json_extract($1,'$.expected_fence') WHERE role.request_id=json_extract($1,'$.request_id') AND {} AND EXISTS(SELECT 1 FROM analysis_requests request JOIN files file ON file.id=request.file_id AND file.size=role.source_size AND file.mtime=role.source_mtime WHERE request.request_id=role.request_id AND request.state='queued' AND request.fence=json_extract($1,'$.expected_fence') AND request.not_before_ms<=json_extract($1,'$.now_ms') AND request.cancel_requested=0 AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||json_extract($1,'$.node_id')))",source_match("role"));
    let acquire="UPDATE analysis_requests SET state='running',owner_node_id=json_extract($1,'$.node_id'),fence=fence+1,lease_expires_ms=json_extract($1,'$.expires_ms'),attempts=attempts+1,last_error_code=NULL,updated_at_ms=json_extract($1,'$.now_ms') WHERE request_id=json_extract($1,'$.request_id') AND state='queued' AND fence=json_extract($1,'$.expected_fence') AND EXISTS(SELECT 1 FROM analysis_coordination_roles role WHERE role.request_id=analysis_requests.request_id AND role.claim_permit_node=json_extract($1,'$.node_id') AND role.claim_permit_fence=json_extract($1,'$.expected_fence'))";
    let history="INSERT INTO analysis_attempts(request_id,attempt,claim_node_id,claim_epoch,claim_expires_at_ms,phase,started_at_ms,phase_updated_at_ms,terminal_code) SELECT request_id,attempts,owner_node_id,fence,lease_expires_ms,'claimed',json_extract($1,'$.now_ms'),json_extract($1,'$.now_ms'),NULL FROM analysis_requests WHERE request_id=json_extract($1,'$.request_id') AND owner_node_id=json_extract($1,'$.node_id') AND fence=json_extract($1,'$.expected_fence')+1 AND state='running' ON CONFLICT DO NOTHING";
    let clear="UPDATE analysis_coordination_roles SET claim_permit_node=NULL,claim_permit_fence=NULL WHERE request_id=json_extract($1,'$.request_id') AND claim_permit_node=json_extract($1,'$.node_id') AND claim_permit_fence=json_extract($1,'$.expected_fence')";
    let attempt = store
        .queue_transaction(vec![
            (permit, body.clone()),
            (acquire.into(), body.clone()),
            (history.into(), body.clone()),
            (clear.into(), body.clone()),
        ])
        .await;
    let rows=store.queue_sql(format!("SELECT json_object('request_id',request.request_id,'node_id',request.owner_node_id,'fence',request.fence) AS result_json FROM analysis_requests request JOIN analysis_coordination_roles role ON role.request_id=request.request_id WHERE request.request_id=json_extract($1,'$.request_id') AND request.owner_node_id=json_extract($1,'$.node_id') AND request.fence=json_extract($1,'$.expected_fence')+1 AND request.state='running' AND request.cancel_requested=0 AND request.lease_expires_ms>json_extract($1,'$.now_ms') AND {}",source_match("role")),body,false,true).await?;
    if let Some(row) = rows.first() {
        return decode(row).map(Some);
    }
    attempt?;
    Ok(None)
}

/// Reserve and register an exact no-reader custody UUID atomically. The
/// expected monotonic sequence fences delayed registration after phase close.
pub(super) async fn begin_phase<T: QueueSql>(
    store: &T,
    claim: AnalysisCoordinationClaim,
    source: AnalysisCoordinationSource,
    expected_sequence: i64,
    custody_id: String,
    now_ms: i64,
) -> Result<Option<AnalysisPhysicalPhase>, StoreError> {
    if !claim.valid()
        || !source.valid()
        || claim.node_id != source.origin_node_id
        || expected_sequence < 0
        || expected_sequence == i64::MAX
        || uuid::Uuid::parse_str(&custody_id).is_err()
        || now_ms < 0
    {
        return Err(invalid());
    }
    let owner = claim.physical_owner();
    let owner_key = super::background_jobs_resources::background_owner_key(&owner)?;
    let body = encode(
        &serde_json::json!({"claim":claim,"source":source,"expected_sequence":expected_sequence,"custody_id":custody_id,"now_ms":now_ms,"owner":owner,"owner_key":owner_key}),
    )?;
    let capacity = super::fragment_index_cluster::analysis_source_capacity_clause(
        "json_extract($1,'$.now_ms')",
    );
    let start=format!("UPDATE analysis_coordination_roles AS role SET phase_sequence=phase_sequence+1,phase_registered=0,active_custody_id=json_extract($1,'$.custody_id'),physical_node_id=json_extract($1,'$.claim.node_id'),physical_fence=json_extract($1,'$.claim.fence') WHERE role.request_id=json_extract($1,'$.claim.request_id') AND {} AND role.phase_sequence=json_extract($1,'$.expected_sequence') AND role.active_custody_id IS NULL AND NOT EXISTS(SELECT 1 FROM background_source_custody custody WHERE custody.owner_kind='analysis' AND custody.work_id=role.request_id) AND EXISTS(SELECT 1 FROM analysis_requests request WHERE {} AND EXISTS(SELECT 1 FROM files file WHERE file.id=role.file_id AND file.size=role.source_size AND file.mtime=role.source_mtime)) AND EXISTS(SELECT 1 FROM analysis_requests WHERE analysis_requests.request_id=role.request_id AND {capacity})",source_match("role"),owned_request());
    let current="role.request_id=json_extract($1,'$.claim.request_id') AND role.active_custody_id=json_extract($1,'$.custody_id') AND role.phase_sequence=json_extract($1,'$.expected_sequence')+1 AND role.physical_node_id=json_extract($1,'$.claim.node_id') AND role.physical_fence=json_extract($1,'$.claim.fence')";
    let reserve=format!("INSERT INTO analysis_source_reservations(request_id,resource_key,fence,expires_at_ms) SELECT role.request_id,required.resource_key,role.physical_fence,request.lease_expires_ms FROM analysis_coordination_roles role JOIN analysis_requests request ON request.request_id=role.request_id JOIN analysis_required_resources required ON required.request_id=role.request_id WHERE {current} AND {} ON CONFLICT(request_id,resource_key) DO UPDATE SET fence=excluded.fence,expires_at_ms=excluded.expires_at_ms",owned_request());
    let register=format!("INSERT INTO background_source_custody(custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,phase,started_at_ms,updated_at_ms,lease_expires_at_ms) SELECT role.active_custody_id,json_extract($1,'$.owner_key'),json_extract($1,'$.owner'),(SELECT json_group_array(resource_key) FROM analysis_source_reservations held WHERE held.request_id=role.request_id AND held.fence=role.physical_fence),'analysis',role.request_id,role.physical_fence,'active',json_extract($1,'$.now_ms'),json_extract($1,'$.now_ms'),request.lease_expires_ms FROM analysis_coordination_roles role JOIN analysis_requests request ON request.request_id=role.request_id WHERE {current} AND role.phase_registered=0 AND {} AND EXISTS(SELECT 1 FROM analysis_source_reservations held WHERE held.request_id=role.request_id AND held.fence=role.physical_fence) ON CONFLICT(owner_key) DO NOTHING",owned_request());
    let registered = format!("UPDATE analysis_coordination_roles AS role SET phase_registered=1 WHERE {current} AND EXISTS(SELECT 1 FROM background_source_custody custody WHERE custody.custody_id=role.active_custody_id)");
    let attempt = store
        .queue_transaction(vec![
            (start, body.clone()),
            (reserve, body.clone()),
            (register, body.clone()),
            (registered, body.clone()),
        ])
        .await;
    let rows=store.queue_sql(format!("SELECT json_object('custody_id',custody.custody_id,'owner',json(custody.owner_json)) AS result_json FROM analysis_coordination_roles role JOIN analysis_requests request ON request.request_id=role.request_id JOIN background_source_custody custody ON custody.custody_id=role.active_custody_id AND custody.owner_key=json_extract($1,'$.owner_key') WHERE {current} AND {} AND {}",source_match("role"),owned_request()),body,false,true).await?;
    if let Some(row) = rows.first() {
        return Ok(Some(AnalysisPhysicalPhase {
            claim,
            source,
            phase_sequence: expected_sequence + 1,
            custody: decode(row)?,
        }));
    }
    attempt?;
    Ok(None)
}

/// Existing joined/no-reader settlement must delete the exact receipt first.
/// Lease expiry never substitutes for that physical-stop proof. A successor
/// may finish this bookkeeping only once the old receipt is actually absent.
pub(super) async fn finish_phase<T: QueueSql>(
    store: &T,
    phase: AnalysisPhysicalPhase,
) -> Result<bool, StoreError> {
    if !phase.valid() {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"claim":phase.claim,"source":phase.source,"sequence":phase.phase_sequence,"custody_id":phase.custody.custody_id}),
    )?;
    let closed=format!("role.request_id=json_extract($1,'$.claim.request_id') AND role.active_custody_id=json_extract($1,'$.custody_id') AND role.phase_sequence=json_extract($1,'$.sequence') AND role.physical_node_id=json_extract($1,'$.claim.node_id') AND role.physical_fence=json_extract($1,'$.claim.fence') AND {} AND NOT EXISTS(SELECT 1 FROM background_source_custody custody WHERE custody.custody_id=role.active_custody_id)",source_match("role"));
    let release=format!("DELETE FROM analysis_source_reservations WHERE request_id=json_extract($1,'$.claim.request_id') AND fence=json_extract($1,'$.claim.fence') AND EXISTS(SELECT 1 FROM analysis_coordination_roles role WHERE {closed})");
    let clear=format!("UPDATE analysis_coordination_roles AS role SET active_custody_id=NULL,physical_node_id=NULL,physical_fence=NULL WHERE {closed}");
    let attempt = store
        .queue_transaction(vec![(release, body.clone()), (clear, body.clone())])
        .await;
    let rows=store.queue_sql(format!("SELECT CASE WHEN EXISTS(SELECT 1 FROM analysis_coordination_roles role WHERE role.request_id=json_extract($1,'$.claim.request_id') AND {} AND role.phase_sequence>=json_extract($1,'$.sequence') AND (role.active_custody_id IS NULL OR role.active_custody_id<>json_extract($1,'$.custody_id'))) THEN 'true' ELSE 'false' END AS result_json",source_match("role")),body,false,true).await?;
    let finished = rows
        .first()
        .map(|row| decode::<bool>(row))
        .transpose()?
        .unwrap_or(false);
    if finished {
        return Ok(true);
    }
    attempt?;
    Ok(false)
}

/// Why a queued coordinator nobody will claim was retired. Enqueue does not
/// count a request retired with this code against its generation, so the
/// source is free to be coordinated again the next time it is observed. The
/// replacement inherits the retired request's attempt count, so retiring
/// never resets the attempt limit.
pub const STRANDED_CODE: &str = "coordination_stranded";

/// Retire the queued coordination requests targeted at `node_id` that were
/// created before `created_before_ms`. Only the copy-analysis worker of the
/// target node joins such a request, and only for a source it holds
/// observations of in memory, so the ones a restarted process finds were left
/// by its predecessor and nothing would ever claim them. A request whose role
/// still names a physical custody is left for the claimant that must settle
/// it. The role of a retired request stays until the same source is next
/// established, which releases it, or the request is pruned.
pub(super) async fn retire_stranded<T: QueueSql>(
    store: &T,
    node_id: &str,
    created_before_ms: i64,
    now_ms: i64,
    keep: &[String],
) -> Result<u64, StoreError> {
    if !identifier(node_id)
        || created_before_ms < 0
        || now_ms < 0
        || keep.len() > MAX_LISTED_COORDINATIONS
    {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"node_id":node_id,"created_before_ms":created_before_ms,"now_ms":now_ms,"code":STRANDED_CODE,"pipeline":format!("{PIPELINE_PREFIX}%"),"keep":keep}),
    )?;
    let rows = store.queue_sql("UPDATE analysis_requests SET state='cancelled',fence=fence+1,cancel_requested=1,last_error_code=json_extract($1,'$.code'),updated_at_ms=json_extract($1,'$.now_ms') WHERE state='queued' AND component='fragment_index' AND pipeline_version LIKE json_extract($1,'$.pipeline') AND target_node_id=json_extract($1,'$.node_id') AND created_at_ms<json_extract($1,'$.created_before_ms') AND request_id NOT IN (SELECT value FROM json_each($1,'$.keep')) AND NOT EXISTS(SELECT 1 FROM analysis_coordination_roles role WHERE role.request_id=analysis_requests.request_id AND role.active_custody_id IS NOT NULL) RETURNING 'true' AS result_json".into(), body, true, true).await?;
    Ok(rows.len() as u64)
}

/// The coordination requests a previous process of `node_id` left, created
/// before `created_before_ms` under ids beginning with `id_prefix`, with
/// the exact source and phase state of each: the queued ones targeted at the
/// node, and the running ones it owns (its predecessor is dead, so nothing
/// holds those claims). A restarted worker reads them before it retires
/// what was left, to take up again the ones it can still work.
pub(super) async fn predecessor_with_prefix<T: QueueSql>(
    store: &T,
    node_id: &str,
    created_before_ms: i64,
    id_prefix: &str,
) -> Result<Vec<AnalysisCoordination>, StoreError> {
    if !identifier(node_id) || created_before_ms < 0 || id_prefix.is_empty() || id_prefix.len() > 64
    {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"node_id":node_id,"created_before_ms":created_before_ms,"prefix":id_prefix,"pipeline":format!("{PIPELINE_PREFIX}%")}),
    )?;
    let rows = store.queue_sql(format!("{OBSERVATION} WHERE request_id IN (SELECT request.request_id FROM analysis_requests request WHERE request.component='fragment_index' AND request.pipeline_version LIKE json_extract($1,'$.pipeline') AND request.created_at_ms<json_extract($1,'$.created_before_ms') AND substr(request.request_id,1,length(json_extract($1,'$.prefix')))=json_extract($1,'$.prefix') AND ((request.state='queued' AND request.target_node_id=json_extract($1,'$.node_id')) OR (request.state='running' AND request.owner_node_id=json_extract($1,'$.node_id')))) ORDER BY request_id LIMIT {MAX_LISTED_COORDINATIONS}"), body, false, true).await?;
    rows.iter().map(|row| decode(row)).collect()
}

/// Retire one queued coordination request by id, as a stranded coordinator:
/// the worker that made it for a demand has dropped the demand. A request
/// that is not queued (claimed, published, already ended) is left as it is.
pub(super) async fn retire_queued<T: QueueSql>(
    store: &T,
    request_id: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !super::fragment_index_cluster::analysis_request_id_is_valid(request_id) || now_ms < 0 {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"request_id":request_id,"now_ms":now_ms,"code":STRANDED_CODE,"pipeline":format!("{PIPELINE_PREFIX}%")}),
    )?;
    let rows = store.queue_sql("UPDATE analysis_requests SET state='cancelled',fence=fence+1,cancel_requested=1,last_error_code=json_extract($1,'$.code'),updated_at_ms=json_extract($1,'$.now_ms') WHERE request_id=json_extract($1,'$.request_id') AND state='queued' AND component='fragment_index' AND pipeline_version LIKE json_extract($1,'$.pipeline') AND NOT EXISTS(SELECT 1 FROM analysis_coordination_roles role WHERE role.request_id=analysis_requests.request_id AND role.active_custody_id IS NOT NULL) RETURNING 'true' AS result_json".into(), body, true, true).await?;
    Ok(!rows.is_empty())
}

/// Queued coordinations one listing returns. A worker holds at most this
/// many sources; the rest are retired and asked for again when played.
const MAX_LISTED_COORDINATIONS: usize = 64;

/// End a coordinator this claim borrowed only to write metadata (a reader's
/// quarantine), as `cancelled` with [`STRANDED_CODE`] and without charging
/// the attempt. The claimant holds no observations and will not rebuild; the
/// node that does enqueues its own request for the same generation, which
/// the stranded code leaves free. A claim holding a physical custody is not
/// ended here.
pub(super) async fn retire_claim<T: QueueSql>(
    store: &T,
    claim: &AnalysisCoordinationClaim,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !claim.valid() || now_ms < 0 {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"claim":claim,"now_ms":now_ms,"code":STRANDED_CODE,"pipeline":format!("{PIPELINE_PREFIX}%")}),
    )?;
    let owned = "request_id=json_extract($1,'$.claim.request_id') AND owner_node_id=json_extract($1,'$.claim.node_id') AND fence=json_extract($1,'$.claim.fence') AND state='running' AND pipeline_version LIKE json_extract($1,'$.pipeline') AND NOT EXISTS(SELECT 1 FROM analysis_coordination_roles role WHERE role.request_id=analysis_requests.request_id AND role.active_custody_id IS NOT NULL)";
    let history = format!("UPDATE analysis_attempts SET phase='canceled',phase_updated_at_ms=json_extract($1,'$.now_ms'),terminal_code=json_extract($1,'$.code') WHERE request_id=json_extract($1,'$.claim.request_id') AND claim_epoch=json_extract($1,'$.claim.fence') AND EXISTS(SELECT 1 FROM analysis_requests WHERE {owned})");
    let retire = format!("UPDATE analysis_requests SET state='cancelled',owner_node_id=NULL,lease_expires_ms=NULL,fence=fence+1,cancel_requested=1,attempts=CASE WHEN attempts>0 THEN attempts-1 ELSE 0 END,last_error_code=json_extract($1,'$.code'),updated_at_ms=json_extract($1,'$.now_ms') WHERE {owned}");
    let attempt = store
        .queue_transaction(vec![(history, body.clone()), (retire, body.clone())])
        .await;
    let rows = store.queue_sql("SELECT 'true' AS result_json FROM analysis_requests WHERE request_id=json_extract($1,'$.claim.request_id') AND state='cancelled' AND fence=json_extract($1,'$.claim.fence')+1 AND last_error_code=json_extract($1,'$.code')".into(), body, false, true).await?;
    if !rows.is_empty() {
        return Ok(true);
    }
    attempt?;
    Ok(false)
}

/// A current metadata-only Analysis claim for the exact source generation.
/// This condition grants neither Source I/O nor public media authority.
pub(super) fn metadata_owned_condition() -> String {
    format!("EXISTS(SELECT 1 FROM analysis_coordination_roles role JOIN analysis_requests request ON request.request_id=role.request_id JOIN files file ON file.id=role.file_id AND file.size=role.source_size AND file.mtime=role.source_mtime WHERE {} AND {})",source_match("role"),owned_request())
}

/// Exact metadata ownership of a physical phase. Foreground pressure governs
/// further reads separately; it cannot invalidate a full hash already joined.
pub(super) fn physical_phase_owned_condition() -> String {
    format!("EXISTS(SELECT 1 FROM analysis_coordination_roles role JOIN analysis_requests request ON request.request_id=role.request_id JOIN background_source_custody custody ON custody.custody_id=role.active_custody_id AND custody.owner_kind='analysis' AND custody.work_id=role.request_id AND custody.fence=role.physical_fence WHERE {} AND {} AND role.active_custody_id=json_extract($1,'$.custody_id') AND role.phase_sequence=json_extract($1,'$.sequence') AND role.physical_node_id=request.owner_node_id AND role.physical_fence=request.fence AND custody.settlement_proof IS NULL AND custody.phase IN ('active','paused') AND EXISTS(SELECT 1 FROM files file WHERE file.id=role.file_id AND file.size=role.source_size AND file.mtime=role.source_mtime))",source_match("role"),owned_request())
}

/// Exact UUID/sequence/source gate for background byte service and readers.
pub(super) async fn phase_current<T: QueueSql>(
    store: &T,
    phase: &AnalysisPhysicalPhase,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !phase.valid() || now_ms < 0 {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"claim":phase.claim,"source":phase.source,"custody_id":phase.custody.custody_id,"sequence":phase.phase_sequence,"now_ms":now_ms}),
    )?;
    let blocked = super::background_jobs_resources::foreground_blocks(
        "resource.value",
        "json_extract($1,'$.now_ms')",
    );
    let owned = physical_phase_owned_condition();
    let sql = format!("SELECT CASE WHEN {owned} AND NOT EXISTS(SELECT 1 FROM background_source_custody custody,json_each(custody.resource_keys_json) resource WHERE custody.custody_id=json_extract($1,'$.custody_id') AND {blocked}) THEN 'true' ELSE 'false' END AS result_json");
    let rows = store.queue_sql(sql, body, false, true).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(|value| value.unwrap_or(false))
}

pub(super) async fn is_coordinator_owner<T: QueueSql>(
    store: &T,
    owner: &BackgroundSourceOwner,
) -> Result<bool, StoreError> {
    let BackgroundSourceOwner::Analysis { request_id, .. } = owner else {
        return Ok(false);
    };
    let rows=store.queue_sql("SELECT CASE WHEN EXISTS(SELECT 1 FROM analysis_requests WHERE request_id=json_extract($1,'$.request_id') AND pipeline_version LIKE 'coordination-contract:%') THEN 'true' ELSE 'false' END AS result_json".into(),encode(&serde_json::json!({"request_id":request_id}))?,false,true).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(|value| value.unwrap_or(false))
}

/// Shape-check adjunct installation independently of frozen Source v81/v82.
pub(crate) fn schema_objects() -> Result<Vec<(String, String)>, StoreError> {
    let conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch("CREATE TABLE analysis_requests(request_id TEXT PRIMARY KEY,state TEXT,fence INTEGER,pipeline_version TEXT,video_identity TEXT,component TEXT,owner_node_id TEXT,updated_at_ms INTEGER,last_error_code TEXT); CREATE TABLE analysis_source_reservations(request_id TEXT,resource_key TEXT,fence INTEGER,expires_at_ms INTEGER); CREATE TABLE background_job_commands(id TEXT PRIMARY KEY,operation TEXT,request_json TEXT,result_json TEXT);")?;
    conn.execute_batch(SCHEMA)?;
    let mut query=conn.prepare("SELECT name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND name NOT IN ('analysis_requests','analysis_source_reservations','background_job_commands') ORDER BY name")?;
    let rows = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
