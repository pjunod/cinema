//! Shared storage identities and all-or-none claim reservations.
use super::background_jobs::{decode, encode, identifier, QueueSql};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = include_str!("background_jobs_resources.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 74;
pub const SOURCE_ARBITRATION_SQLITE_SCHEMA: i64 = 122;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageDomainMapping {
    pub library_id: i64,
    pub root_path: String,
    pub domain_id: String,
}

pub fn validate(mappings: &[StorageDomainMapping]) -> Result<(), StoreError> {
    let mut roots = std::collections::BTreeSet::new();
    if mappings.len() > 256
        || mappings.iter().any(|mapping| {
            mapping.library_id <= 0
                || mapping.root_path.is_empty()
                || mapping.root_path.len() > 4096
                || mapping.root_path.contains('\0')
                || !identifier(&mapping.domain_id)
                || mapping.domain_id.len() > 64
                || !roots.insert((mapping.library_id, &mapping.root_path))
        })
    {
        return Err(StoreError::Task("invalid storage domains: at most 256 unique library roots and 64-byte domain identifiers".into()));
    }
    Ok(())
}

pub(super) async fn list<T: QueueSql>(store: &T) -> Result<Vec<StorageDomainMapping>, StoreError> {
    store.queue_sql("SELECT json_object('library_id', library_id, 'root_path', root_path, 'domain_id', domain_id) AS result_json FROM background_storage_domains ORDER BY library_id, root_path LIMIT json_extract($1, '$.limit')".into(), "{\"limit\":256}".into(), false, true).await?
        .iter().map(|row| decode(row)).collect()
}

pub(super) async fn replace<T: QueueSql>(
    store: &T,
    mappings: Vec<StorageDomainMapping>,
    now_ms: i64,
) -> Result<bool, StoreError> {
    validate(&mappings)?;
    if now_ms < 0 {
        return Err(StoreError::Task("invalid storage domain clock".into()));
    }
    let body = encode(
        &serde_json::json!({"mappings":mappings,"now_ms":now_ms,"id":uuid::Uuid::new_v4().to_string()}),
    )?;
    if body.len() > 64 * 1024 {
        return Err(StoreError::Task(
            "storage domain configuration exceeds 64 KiB".into(),
        ));
    }
    let rows = store
        .queue_sql(
            r#"
INSERT INTO background_job_commands(id, operation, request_json, result_json)
SELECT json_extract($1, '$.id'), 'storage_domains', $1,
    CASE WHEN NOT EXISTS (SELECT 1 FROM background_jobs
        WHERE state IN ('running','cancelling') AND lease_expires_ms > json_extract($1, '$.now_ms'))
      AND NOT EXISTS (SELECT 1 FROM analysis_source_reservations
        WHERE expires_at_ms > json_extract($1, '$.now_ms'))
      AND NOT EXISTS (SELECT 1 FROM background_source_custody)
      AND NOT EXISTS (SELECT 1 FROM foreground_source_owners WHERE lease_expires_at_ms>json_extract($1,'$.now_ms') OR json_extract(projection_json,'$.source_quiescent')!=1)
      AND NOT EXISTS (SELECT 1 FROM background_source_pauses paused JOIN background_jobs job ON job.id=paused.job_id
        AND job.fence=paused.fence WHERE job.state IN ('running','cancelling') AND job.lease_expires_ms>json_extract($1,'$.now_ms'))
      AND NOT EXISTS (SELECT 1 FROM json_each($1, '$.mappings') mapping
        WHERE NOT EXISTS (SELECT 1 FROM libraries library, json_each(library.paths) root
            WHERE library.id = json_extract(mapping.value, '$.library_id')
            AND root.value = json_extract(mapping.value, '$.root_path')))
      THEN 'true' ELSE 'false' END
RETURNING result_json
"#
            .into(),
            body,
            true,
            true,
        )
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing domain replacement verdict".into()))?,
    )
}

/// Both backends install the same foreground custody and continuation schema.
pub(crate) const SOURCE_ARBITRATION_SCHEMA: &str = concat!(
    include_str!("background_jobs_foreground.sql"),
    "\n-- next statement\n",
    include_str!("source_checkpoints.sql"),
    "\n-- next statement\n",
    include_str!("background_jobs_source_interest.sql"),
    "\n-- next statement\n",
    include_str!("background_jobs_source_service.sql")
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForegroundSourcePolicy {
    Rolling,
    Transfer,
    RollingOrTransfer,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForegroundSourcePhase {
    Startup,
    Refill,
    Sharing,
    TransferService,
    SourceIdle,
    CleanupPending,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForegroundSourceOwner {
    pub owner_id: String,
    pub lifetime_id: String,
    pub session_incarnation: String,
    pub producer_attempt: String,
    pub node_id: String,
    pub boot_id: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_generation: String,
    pub policy: ForegroundSourcePolicy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForegroundSourceToken {
    pub owner: ForegroundSourceOwner,
    pub fence: i64,
    pub lease_expires_at_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceServiceClass {
    Measured,
    WeightedUnqualified,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceServiceGrant {
    pub epoch_id: u64,
    pub starts_at_ms: i64,
    pub expires_at_ms: i64,
    pub foreground_bytes: u64,
    pub background_bytes: u64,
    pub foreground_floor_bps: Option<u64>,
    pub supply_floor_bps: Option<u64>,
    pub service_class: SourceServiceClass,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForegroundSourceProjection {
    pub phase: ForegroundSourcePhase,
    pub observation_version: u64,
    pub observed_at_ms: i64,
    pub observation_expires_at_ms: i64,
    pub source_quiescent: bool,
    pub contiguous_lead_ms: Option<u64>,
    pub low_ms: Option<u64>,
    pub high_ms: Option<u64>,
    pub transfer_grant: Option<SourceServiceGrant>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackgroundSourceOwner {
    Job {
        token: super::background_jobs::JobToken,
    },
    Analysis {
        request_id: String,
        node_id: String,
        fence: i64,
    },
    ReadAttestation {
        lease: crate::cluster::coordination::Lease,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
    },
    Offline {
        package_id: String,
        node_id: String,
        claim_generation: i64,
    },
}
impl ForegroundSourceOwner {
    fn valid(&self) -> bool {
        let bounded = |value: &str| {
            !value.is_empty() && value.len() <= 128 && !value.bytes().any(|c| c.is_ascii_control())
        };
        uuid::Uuid::parse_str(&self.owner_id).is_ok()
            && bounded(&self.lifetime_id)
            && uuid::Uuid::parse_str(&self.session_incarnation).is_ok()
            && bounded(&self.producer_attempt)
            && identifier(&self.node_id)
            && uuid::Uuid::parse_str(&self.boot_id).is_ok()
            && self.file_id > 0
            && self.source_size >= 0
            && self.source_mtime >= 0
            && bounded(&self.source_generation)
    }
}
impl ForegroundSourceProjection {
    pub fn startup(now_ms: i64) -> Self {
        Self {
            phase: ForegroundSourcePhase::Startup,
            observation_version: 0,
            observed_at_ms: now_ms,
            observation_expires_at_ms: now_ms,
            source_quiescent: false,
            contiguous_lead_ms: None,
            low_ms: None,
            high_ms: None,
            transfer_grant: None,
        }
    }
    fn valid(&self, policy: &ForegroundSourcePolicy, now_ms: i64) -> bool {
        const JS_MAX: u64 = 9_007_199_254_740_991;
        if self.observation_version > JS_MAX
            || self.observed_at_ms < 0
            || self.observed_at_ms > now_ms
            || self.observation_expires_at_ms < self.observed_at_ms
            || self.observation_expires_at_ms > now_ms.saturating_add(30_000)
        {
            return false;
        }
        match self.phase {
            ForegroundSourcePhase::Sharing => matches!(policy,ForegroundSourcePolicy::Rolling|ForegroundSourcePolicy::RollingOrTransfer) && self.source_quiescent
                && self.observation_expires_at_ms>now_ms && self.transfer_grant.is_none()
                && matches!((self.contiguous_lead_ms,self.low_ms,self.high_ms), (Some(lead),Some(low),Some(high)) if lead<=JS_MAX && low>0 && high>low && high<=JS_MAX),
            ForegroundSourcePhase::TransferService => matches!(policy,ForegroundSourcePolicy::Transfer|ForegroundSourcePolicy::RollingOrTransfer) && self.transfer_grant.as_ref().is_some_and(|grant| {
                let duration=grant.expires_at_ms.saturating_sub(grant.starts_at_ms);
                if grant.epoch_id>JS_MAX || grant.starts_at_ms<0 || grant.starts_at_ms%5000!=0 || grant.epoch_id!=grant.starts_at_ms as u64/5000 || grant.starts_at_ms>now_ms || grant.expires_at_ms<=now_ms || duration!=5000
                    || grant.background_bytes<1_048_576 || grant.background_bytes>JS_MAX || grant.foreground_bytes>JS_MAX { return false; }
                match grant.service_class {
                    SourceServiceClass::Measured => matches!((grant.foreground_floor_bps,grant.supply_floor_bps),(Some(floor),Some(supply))
                        if floor>0 && supply>=floor && supply<=JS_MAX
                        && u128::from(grant.foreground_bytes)>=u128::from(floor)*duration as u128/8000
                        && u128::from(grant.foreground_bytes)+u128::from(grant.background_bytes)<=u128::from(supply)*duration as u128/8000),
                    SourceServiceClass::WeightedUnqualified => grant.foreground_floor_bps.is_none() && grant.supply_floor_bps.is_none()
                        && grant.foreground_bytes==grant.background_bytes.saturating_mul(2),
                }
            }),
            ForegroundSourcePhase::SourceIdle => self.source_quiescent && self.transfer_grant.is_none(),
            ForegroundSourcePhase::Startup|ForegroundSourcePhase::Refill|ForegroundSourcePhase::CleanupPending => self.transfer_grant.is_none(),
        }
    }
}

/// Complete conjunction over the same resource identity as background claims.
/// Missing/stale rolling evidence is refill. Idle means settled input custody,
/// never merely socket backpressure or an open descriptor without observations.
pub(crate) fn foreground_blocks(resource: &str, now: &str) -> String {
    format!("EXISTS(SELECT 1 FROM foreground_source_permissions fg WHERE ({resource} LIKE 'source_io%' AND (fg.resource_key={resource} OR fg.resource_key='source_io' OR {resource}='source_io')) AND (fg.lease_expires_at_ms>{now} OR fg.custody_held=1) AND (fg.lease_expires_at_ms<={now} OR COALESCE(fg.permits_through_ms,0)<={now}))")
}
fn token_json() -> &'static str {
    "json_object('owner',json(identity_json),'fence',fence,'lease_expires_at_ms',lease_expires_at_ms)"
}

pub(super) async fn register_foreground<T: QueueSql>(
    store: &T,
    owner: ForegroundSourceOwner,
    now_ms: i64,
    lease_expires_at_ms: i64,
) -> Result<Option<ForegroundSourceToken>, StoreError> {
    if !owner.valid()
        || now_ms < 0
        || lease_expires_at_ms <= now_ms
        || lease_expires_at_ms > now_ms.saturating_add(30_000)
    {
        return Err(StoreError::Task(
            "invalid foreground source registration".into(),
        ));
    }
    let body = encode(
        &serde_json::json!({"owner":owner,"now_ms":now_ms,"lease_expires_at_ms":lease_expires_at_ms,
        "projection":ForegroundSourceProjection::startup(now_ms)}),
    )?;
    let rows=store.queue_sql(format!(r#"INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms)
      SELECT json_extract($1,'$.owner.owner_id'),json_extract($1,'$.owner'),
        (SELECT json_group_array(resource_key) FROM (SELECT DISTINCT COALESCE('source_io:'||mapping.domain_id,'source_io') AS resource_key
          FROM libraries library LEFT JOIN json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
          LEFT JOIN background_storage_domains mapping ON mapping.library_id=library.id AND mapping.root_path=root.value WHERE library.id=item.library_id)),
        1,'startup',json_extract($1,'$.projection'),json_extract($1,'$.now_ms'),json_extract($1,'$.now_ms'),json_extract($1,'$.lease_expires_at_ms')
      FROM files file JOIN items item ON item.id=file.item_id
      WHERE file.id=json_extract($1,'$.owner.file_id') AND file.size=json_extract($1,'$.owner.source_size') AND file.mtime=json_extract($1,'$.owner.source_mtime')
        AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||json_extract($1,'$.owner.node_id'))
      ON CONFLICT(owner_id) DO UPDATE SET updated_at_ms=foreground_source_owners.updated_at_ms
        WHERE identity_json=json_extract($1,'$.owner') AND fence=1 AND phase='startup' AND lease_expires_at_ms>json_extract($1,'$.now_ms')
      RETURNING {} AS result_json"#,token_json()),body,true,true).await?;
    rows.first().map(|row| decode(row)).transpose()
}

pub(super) async fn project_foreground<T: QueueSql>(
    store: &T,
    token: ForegroundSourceToken,
    projection: ForegroundSourceProjection,
    now_ms: i64,
    lease_expires_at_ms: i64,
) -> Result<Option<ForegroundSourceToken>, StoreError> {
    if !token.owner.valid()
        || token.fence <= 0
        || !projection.valid(&token.owner.policy, now_ms)
        || now_ms < 0
        || lease_expires_at_ms <= now_ms
        || lease_expires_at_ms > now_ms.saturating_add(30_000)
    {
        return Err(StoreError::Task(
            "invalid foreground source projection".into(),
        ));
    }
    let body = encode(
        &serde_json::json!({"token":token,"projection":projection,"now_ms":now_ms,"lease_expires_at_ms":lease_expires_at_ms}),
    )?;
    let rows=store.queue_sql(format!(r#"UPDATE foreground_source_owners SET fence=fence+1,
        phase=CASE WHEN json_extract($1,'$.projection.phase')='sharing' THEN
          CASE WHEN json_extract($1,'$.projection.contiguous_lead_ms')>=json_extract($1,'$.projection.high_ms')
            OR (phase='sharing' AND json_extract($1,'$.projection.contiguous_lead_ms')>json_extract($1,'$.projection.low_ms'))
            THEN 'sharing' ELSE 'refill' END ELSE json_extract($1,'$.projection.phase') END,
        projection_json=json_extract($1,'$.projection'),updated_at_ms=json_extract($1,'$.now_ms'),lease_expires_at_ms=json_extract($1,'$.lease_expires_at_ms')
      WHERE owner_id=json_extract($1,'$.token.owner.owner_id') AND identity_json=json_extract($1,'$.token.owner')
        AND fence=json_extract($1,'$.token.fence') AND fence<9223372036854775807
        AND (lease_expires_at_ms>json_extract($1,'$.now_ms') OR json_extract($1,'$.projection.phase')='cleanup_pending')
        AND json_extract($1,'$.projection.observation_version')>json_extract(projection_json,'$.observation_version')
        AND json_extract($1,'$.projection.observed_at_ms')>=json_extract(projection_json,'$.observed_at_ms')
        AND (json_extract($1,'$.projection.transfer_grant.service_class') IS NOT 'measured' OR NOT EXISTS(
          SELECT 1 FROM foreground_source_required_resources required WHERE required.owner_id=foreground_source_owners.owner_id
            AND (COALESCE((SELECT SUM(json_extract(peer.projection_json,'$.transfer_grant.foreground_bytes'))
                FROM foreground_source_owners peer JOIN foreground_source_required_resources peer_resource ON peer_resource.owner_id=peer.owner_id
                WHERE (peer_resource.resource_key=required.resource_key OR (peer_resource.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (peer_resource.resource_key='source_io' OR required.resource_key='source_io'))) AND peer.owner_id!=foreground_source_owners.owner_id
                  AND peer.phase='transfer_service' AND peer.lease_expires_at_ms>json_extract($1,'$.now_ms')
                  AND json_extract(peer.projection_json,'$.transfer_grant.expires_at_ms')>json_extract($1,'$.now_ms')),0)
              +json_extract($1,'$.projection.transfer_grant.foreground_bytes'))+json_extract($1,'$.projection.transfer_grant.background_bytes')
              >MIN(json_extract($1,'$.projection.transfer_grant.supply_floor_bps'),COALESCE(
                (SELECT MIN(json_extract(peer.projection_json,'$.transfer_grant.supply_floor_bps'))
                FROM foreground_source_owners peer JOIN foreground_source_required_resources peer_resource ON peer_resource.owner_id=peer.owner_id
                WHERE (peer_resource.resource_key=required.resource_key OR (peer_resource.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (peer_resource.resource_key='source_io' OR required.resource_key='source_io'))) AND peer.owner_id!=foreground_source_owners.owner_id
                  AND peer.phase='transfer_service' AND peer.lease_expires_at_ms>json_extract($1,'$.now_ms')
                  AND json_extract(peer.projection_json,'$.transfer_grant.expires_at_ms')>json_extract($1,'$.now_ms')),
                json_extract($1,'$.projection.transfer_grant.supply_floor_bps')))*5/8))
        AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||json_extract(identity_json,'$.node_id'))
      RETURNING {} AS result_json"#,token_json()),body,true,true).await?;
    rows.first().map(|row| decode(row)).transpose()
}
pub(super) async fn release_foreground<T: QueueSql>(
    store: &T,
    token: ForegroundSourceToken,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !token.owner.valid() || token.fence <= 0 || now_ms < 0 {
        return Err(StoreError::Task("invalid foreground source release".into()));
    }
    let rows=store.queue_sql("DELETE FROM foreground_source_owners WHERE owner_id=json_extract($1,'$.owner.owner_id') AND identity_json=json_extract($1,'$.owner') AND fence=json_extract($1,'$.fence') RETURNING 'true' AS result_json".into(),encode(&token)?,true,true).await?;
    Ok(!rows.is_empty())
}

fn background_identity_sql() -> &'static str {
    r#"job.id=json_extract($1,'$.token.job_id') AND job.fence=json_extract($1,'$.token.fence')
    AND job.owner_node_id=json_extract($1,'$.token.node_id') AND job.owner_boot_id=json_extract($1,'$.token.boot_id')
    AND job.claim_id=json_extract($1,'$.token.claim_id') AND job.state='running'
    AND job.lease_expires_ms>json_extract($1,'$.now_ms')
    AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||job.owner_node_id)"#
}
pub(super) async fn pause_background<T: QueueSql>(
    store: &T,
    token: super::background_jobs::JobToken,
    now_ms: i64,
) -> Result<bool, StoreError> {
    token.validate()?;
    if now_ms < 0 {
        return Err(StoreError::Task("invalid source pause time".into()));
    }
    let body = encode(
        &serde_json::json!({"id":uuid::Uuid::new_v4().to_string(),"token":token,"now_ms":now_ms}),
    )?;
    let rows=store.queue_sql(format!(r#"INSERT INTO background_job_commands(id,operation,request_json,result_json)
      SELECT json_extract($1,'$.id'),'source_pause',$1,CASE WHEN EXISTS(SELECT 1 FROM background_jobs job
        WHERE {} AND (EXISTS(SELECT 1 FROM background_job_reservations held WHERE held.job_id=job.id AND held.fence=job.fence AND held.resource_key LIKE 'source_io%')
          OR EXISTS(SELECT 1 FROM background_source_pauses paused WHERE paused.job_id=job.id AND paused.fence=job.fence))) THEN 'true' ELSE 'false' END
      RETURNING result_json"#,background_identity_sql()),body,true,true).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(|value| value.unwrap_or(false))
}
pub(super) async fn resume_background<T: QueueSql>(
    store: &T,
    token: super::background_jobs::JobToken,
    now_ms: i64,
) -> Result<bool, StoreError> {
    token.validate()?;
    if now_ms < 0 {
        return Err(StoreError::Task("invalid source resume time".into()));
    }
    let body = encode(
        &serde_json::json!({"id":uuid::Uuid::new_v4().to_string(),"token":token,"now_ms":now_ms}),
    )?;
    let blocked = foreground_blocks("resource.value", "json_extract($1,'$.now_ms')");
    let rows=store.queue_sql(format!(r#"INSERT INTO background_job_commands(id,operation,request_json,result_json)
      SELECT json_extract($1,'$.id'),'source_resume',$1,CASE WHEN EXISTS(SELECT 1 FROM background_jobs job
        JOIN background_source_pauses paused ON paused.job_id=job.id AND paused.fence=job.fence
          AND paused.node_id=job.owner_node_id AND paused.boot_id=job.owner_boot_id AND paused.claim_id=job.claim_id
        WHERE {identity} AND json_array_length(paused.resource_keys_json)>0
          AND NOT EXISTS(SELECT 1 FROM json_each(paused.resource_keys_json) resource WHERE {blocked}
            OR (SELECT count(DISTINCT json_array(held.owner_kind,held.work_id,held.fence)) FROM source_io_occupancy held WHERE (held.resource_key=resource.value OR (held.resource_key LIKE 'source_io%' AND resource.value LIKE 'source_io%' AND (held.resource_key='source_io' OR resource.value='source_io')))
                AND held.active_occupancy=1 AND (held.expires_at_ms>json_extract($1,'$.now_ms') OR held.custody_id IS NOT NULL)
                AND NOT(held.owner_kind='job' AND held.work_id=job.id AND held.fence=job.fence))>=2)) THEN 'true' ELSE 'false' END
      RETURNING result_json"#,identity=background_identity_sql()),body,true,true).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(|value| value.unwrap_or(false))
}
pub(super) async fn background_may_continue<T: QueueSql>(
    store: &T,
    owner: BackgroundSourceOwner,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if now_ms < 0 {
        return Err(StoreError::Task("invalid source observation time".into()));
    }
    background_owner_key(&owner)?;
    let identity = background_owner_identity(&owner);
    let resources = background_owner_resources(&owner);
    let mut body =
        serde_json::to_value(owner).map_err(|error| StoreError::Task(error.to_string()))?;
    body["now_ms"] = now_ms.into();
    let blocked = foreground_blocks("resource.resource_key", "json_extract($1,'$.now_ms')");
    let rows=store.queue_sql(format!("SELECT CASE WHEN {identity} AND NOT EXISTS(SELECT 1 FROM ({resources}) resource WHERE resource.resource_key LIKE 'source_io%' AND {blocked}) THEN 'true' ELSE 'false' END AS result_json"),encode(&body)?,false,true).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(|value| value.unwrap_or(false))
}

pub(super) async fn foreground_pressure<T: QueueSql>(
    store: &T,
    file_id: i64,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if file_id <= 0 || now_ms < 0 {
        return Err(StoreError::Task("invalid source pressure query".into()));
    }
    let blocked = foreground_blocks("required.resource_key", "json_extract($1,'$.now_ms')");
    let sql = format!(
        r#"SELECT CASE WHEN EXISTS(SELECT 1 FROM (
      SELECT DISTINCT COALESCE('source_io:'||mapping.domain_id,'source_io') AS resource_key
      FROM files file JOIN items item ON item.id=file.item_id JOIN libraries library ON library.id=item.library_id
      LEFT JOIN json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
      LEFT JOIN background_storage_domains mapping ON mapping.library_id=library.id AND mapping.root_path=root.value
      WHERE file.id=json_extract($1,'$.file_id')) required WHERE {blocked}) THEN 'true' ELSE 'false' END AS result_json"#
    );
    let rows = store
        .queue_sql(
            sql,
            encode(&serde_json::json!({"file_id":file_id,"now_ms":now_ms}))?,
            false,
            true,
        )
        .await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(|value| value.unwrap_or(false))
}

/// Canonical SQLite definitions, used by the replicated extension installer.
/// A marker alone cannot attest tables, views or physical-owner triggers.
pub(crate) fn source_arbitration_objects() -> Result<Vec<(String, String)>, StoreError> {
    let conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch("CREATE TABLE background_jobs(id TEXT PRIMARY KEY,state TEXT,fence INTEGER); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); CREATE TABLE background_job_waiters(request_scope TEXT,request_id TEXT,state TEXT,updated_at_ms INTEGER);")?;
    conn.execute_batch(SOURCE_ARBITRATION_SCHEMA)?;
    let mut statement=conn.prepare("SELECT name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND name NOT IN ('background_jobs','background_job_commands','background_job_waiters') ORDER BY name")?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) async fn foreground_owners<T: QueueSql>(
    store: &T,
    node_id: &str,
) -> Result<Vec<ForegroundSourceToken>, StoreError> {
    if !identifier(node_id) {
        return Err(StoreError::Task("invalid source custody owner".into()));
    }
    store.queue_sql(format!("SELECT {} AS result_json FROM foreground_source_owners WHERE json_extract(identity_json,'$.node_id')=json_extract($1,'$.node_id') ORDER BY started_at_ms LIMIT 1024",token_json()),encode(&serde_json::json!({"node_id":node_id}))?,false,true).await?
        .iter().map(|row|decode(row)).collect()
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    struct SourceSql(Mutex<rusqlite::Connection>);
    #[async_trait]
    impl QueueSql for SourceSql {
        async fn queue_transaction(
            &self,
            statements: Vec<(String, String)>,
        ) -> Result<(), StoreError> {
            let mut connection = self.0.lock().expect("connection");
            let transaction = connection.transaction()?;
            for (sql, body) in statements {
                transaction.execute(&sql, [body])?;
            }
            transaction.commit()?;
            Ok(())
        }
        async fn queue_sql(
            &self,
            sql: String,
            body: String,
            _mutation: bool,
            _authoritative: bool,
        ) -> Result<Vec<String>, StoreError> {
            let connection = self.0.lock().expect("connection");
            let mut statement = connection.prepare(&sql)?;
            let rows = statement
                .query_map([body], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(rows)
        }
    }
    fn fixture() -> SourceSql {
        let connection = rusqlite::Connection::open_in_memory().expect("database");
        connection.execute_batch("CREATE TABLE libraries(id INTEGER PRIMARY KEY,paths TEXT); CREATE TABLE items(id INTEGER PRIMARY KEY,library_id INTEGER);
          CREATE TABLE files(id INTEGER PRIMARY KEY,item_id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE settings(key TEXT,value TEXT);
          CREATE TABLE job_leases(resource TEXT PRIMARY KEY,owner_node_id TEXT,fence INTEGER,revision INTEGER,expires_at_ms INTEGER,updated_at_ms INTEGER);
          CREATE TABLE offline_packages(id TEXT PRIMARY KEY,node_id TEXT,file_id INTEGER,source_size INTEGER,source_mtime INTEGER,claim_generation INTEGER,state TEXT,expires_at INTEGER);
          CREATE TABLE background_storage_domains(library_id INTEGER,root_path TEXT,domain_id TEXT);
          CREATE TABLE background_jobs(id TEXT PRIMARY KEY,kind TEXT,state TEXT,fence INTEGER,owner_node_id TEXT,owner_boot_id TEXT,claim_id TEXT,lease_expires_ms INTEGER,priority INTEGER DEFAULT 0);
          CREATE TABLE background_job_commands(id TEXT PRIMARY KEY,operation TEXT,request_json TEXT,result_json TEXT);
          CREATE TABLE background_job_waiters(request_scope TEXT,request_id TEXT,state TEXT,updated_at_ms INTEGER,request_digest TEXT,job_id TEXT,consumer_kind TEXT,consumer_ref TEXT,priority INTEGER,deadline_ms INTEGER,PRIMARY KEY(request_scope,request_id));
          CREATE TABLE media_session_requests(user_id INTEGER,playback_id TEXT,incarnation_id TEXT,state TEXT,claim_expires_at_ms INTEGER);
          CREATE TABLE media_sessions(user_id INTEGER,playback_id TEXT,incarnation_id TEXT,state TEXT,lease_expires_at_ms INTEGER,recipe_json TEXT);
          CREATE TABLE media_playback_pointers(user_id INTEGER,playback_id TEXT,current_incarnation_id TEXT);
          CREATE TABLE background_job_reservations(resource_key TEXT,slot INTEGER,job_id TEXT,fence INTEGER,expires_at_ms INTEGER,PRIMARY KEY(resource_key,slot));
          CREATE TABLE analysis_requests(request_id TEXT PRIMARY KEY,owner_node_id TEXT,fence INTEGER,state TEXT,cancel_requested INTEGER,lease_expires_ms INTEGER,
            requested_generation TEXT,file_id INTEGER,source_size INTEGER,source_mtime INTEGER,priority TEXT,trigger TEXT,updated_at_ms INTEGER,force_rebuild INTEGER,component TEXT);
          CREATE TABLE analysis_source_reservations(resource_key TEXT,request_id TEXT,fence INTEGER,expires_at_ms INTEGER);
          CREATE VIEW analysis_required_resources AS SELECT request_id,'source_io' AS resource_key FROM analysis_requests;
          INSERT INTO libraries VALUES(1,'[\"/media\"]'); INSERT INTO items VALUES(1,1);INSERT INTO files VALUES(1,1,100,1);").expect("existing arbiter");
        connection
            .execute_batch(SOURCE_ARBITRATION_SCHEMA)
            .expect("same new schema as both backends");
        SourceSql(Mutex::new(connection))
    }
    fn owner(policy: ForegroundSourcePolicy) -> ForegroundSourceOwner {
        ForegroundSourceOwner {
            owner_id: uuid::Uuid::new_v4().to_string(),
            lifetime_id: "original-gesture".into(),
            session_incarnation: uuid::Uuid::new_v4().to_string(),
            producer_attempt: "producer-1".into(),
            node_id: "worker".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            file_id: 1,
            source_size: 100,
            source_mtime: 1,
            source_generation: "source-generation".into(),
            policy,
        }
    }
    fn sharing(version: u64, lead: u64, now_ms: i64) -> ForegroundSourceProjection {
        ForegroundSourceProjection {
            phase: ForegroundSourcePhase::Sharing,
            observation_version: version,
            observed_at_ms: now_ms,
            observation_expires_at_ms: now_ms + 1000,
            source_quiescent: true,
            contiguous_lead_ms: Some(lead),
            low_ms: Some(40),
            high_ms: Some(100),
            transfer_grant: None,
        }
    }
    fn seed_job(store: &SourceSql) -> super::super::background_jobs::JobToken {
        let token = super::super::background_jobs::JobToken {
            job_id: uuid::Uuid::new_v4().to_string(),
            node_id: "worker".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            fence: 1,
            revision: 1,
            lease_expires_ms: 30_000,
        };
        let connection = store.0.lock().expect("connection");
        connection
            .execute(
                "INSERT INTO background_jobs(id,kind,state,fence,owner_node_id,owner_boot_id,claim_id,lease_expires_ms) VALUES(?1,'library_scan','running',1,?2,?3,?4,30000)",
                rusqlite::params![token.job_id, token.node_id, token.boot_id, token.claim_id],
            )
            .expect("job");
        connection
            .execute(
                "INSERT INTO background_job_reservations SELECT 'source_io',COALESCE(MAX(slot),-1)+1,?1,1,30000 FROM background_job_reservations",
                [&token.job_id],
            )
            .expect("charged source");
        token
    }
    #[tokio::test]
    async fn foreground_source_hysteresis_requires_all_owners_and_stale_custody_stays_charged() {
        let store = fixture();
        let job = seed_job(&store);
        let background = BackgroundSourceOwner::Job { token: job };
        let first = register_foreground(&store, owner(ForegroundSourcePolicy::Rolling), 1000, 2000)
            .await
            .expect("register")
            .expect("token");
        assert!(!background_may_continue(&store, background.clone(), 1001)
            .await
            .expect("pressure"));
        let first = project_foreground(&store, first, sharing(1, 50, 1002), 1002, 2000)
            .await
            .expect("project")
            .expect("token");
        assert!(!background_may_continue(&store, background.clone(), 1003)
            .await
            .expect("below high"));
        let first = project_foreground(&store, first, sharing(2, 100, 1004), 1004, 2000)
            .await
            .expect("high")
            .expect("token");
        assert!(background_may_continue(&store, background.clone(), 1005)
            .await
            .expect("sharing"));
        let second =
            register_foreground(&store, owner(ForegroundSourcePolicy::Rolling), 1006, 1100)
                .await
                .expect("overlap")
                .expect("second owner");
        assert!(!background_may_continue(&store, background.clone(), 1007)
            .await
            .expect("domain conjunction"));
        assert!(!background_may_continue(&store, background.clone(), 1200)
            .await
            .expect("expired physical custody remains"));
        assert!(release_foreground(&store, second, 1201)
            .await
            .expect("exact settled custody"));
        assert!(background_may_continue(&store, background.clone(), 1202)
            .await
            .expect("remaining fresh sharing owner"));
        assert!(background_may_continue(&store, background, 2005)
            .await
            .expect("quiescent demand expires without reviving active reads"));
        assert!(release_foreground(&store, first, 2006)
            .await
            .expect("release"));
    }
    #[tokio::test]
    async fn background_source_pause_preserves_fence_and_keys_until_atomic_resume() {
        let store = fixture();
        let job = seed_job(&store);
        assert!(pause_background(&store, job.clone(), 1000)
            .await
            .expect("verified pause"));
        assert!(pause_background(&store, job.clone(), 1001)
            .await
            .expect("pause replay"));
        {
            let connection = store.0.lock().expect("connection");
            assert_eq!(
                connection
                    .query_row(
                        "SELECT count(*) FROM background_job_reservations",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .expect("occupancy"),
                0
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT resource_keys_json FROM background_source_pauses",
                        [],
                        |row| row.get::<_, String>(0)
                    )
                    .expect("fixed keys"),
                "[\"source_io\"]"
            );
        }
        let foreground =
            register_foreground(&store, owner(ForegroundSourcePolicy::Transfer), 1002, 2000)
                .await
                .expect("immediate foreground overlap")
                .expect("foreground");
        assert!(!resume_background(&store, job.clone(), 1003)
            .await
            .expect("resume blocked"));
        let mut stale = job.clone();
        stale.fence += 1;
        assert!(!resume_background(&store, stale, 1003)
            .await
            .expect("wrong fence"));
        release_foreground(&store, foreground, 1004)
            .await
            .expect("foreground settled");
        assert!(resume_background(&store, job.clone(), 1005)
            .await
            .expect("readmit before SIGCONT"));
        assert!(resume_background(&store, job, 1006)
            .await
            .expect("lost acknowledgement replay"));
        let connection = store.0.lock().expect("connection");
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM background_job_reservations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("charged"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM background_job_commands", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("bounded commands"),
            0
        );
    }
    #[test]
    fn transfer_service_grants_are_finite_and_unknown_supply_is_explicitly_unqualified() {
        let mut projection = ForegroundSourceProjection::startup(1000);
        projection.phase = ForegroundSourcePhase::TransferService;
        projection.observation_version = 1;
        projection.observation_expires_at_ms = 6000;
        projection.transfer_grant = Some(SourceServiceGrant {
            epoch_id: 0,
            starts_at_ms: 0,
            expires_at_ms: 5000,
            foreground_bytes: 2 * 1_048_576,
            background_bytes: 1_048_576,
            foreground_floor_bps: None,
            supply_floor_bps: None,
            service_class: SourceServiceClass::WeightedUnqualified,
        });
        assert!(projection.valid(&ForegroundSourcePolicy::Transfer, 1001));
        projection
            .transfer_grant
            .as_mut()
            .expect("grant")
            .expires_at_ms = 6001;
        assert!(!projection.valid(&ForegroundSourcePolicy::Transfer, 1001));
        projection
            .transfer_grant
            .as_mut()
            .expect("grant")
            .expires_at_ms = 6000;
        projection
            .transfer_grant
            .as_mut()
            .expect("grant")
            .service_class = SourceServiceClass::Measured;
        assert!(
            !projection.valid(&ForegroundSourcePolicy::Transfer, 1001),
            "a measured supply claim needs actual floors"
        );
    }
    #[test]
    fn source_extension_shape_covers_owner_views_pause_triggers_and_checkpoint_boundary() {
        let objects = source_arbitration_objects().expect("canonical SQL");
        for name in [
            "foreground_source_owners",
            "foreground_source_permissions",
            "background_source_pauses",
            "background_source_resume_effects",
            "analysis_source_checkpoints",
        ] {
            assert!(
                objects.iter().any(|(candidate, _)| candidate == name),
                "missing {name}"
            );
        }
    }
    #[tokio::test]
    async fn background_physical_custody_survives_expiry_new_fence_and_history_removal() {
        let store = fixture();
        let job = seed_job(&store);
        let owner = BackgroundSourceOwner::Job { token: job.clone() };
        let receipt = register_background_custody(&store, owner.clone(), 1000)
            .await
            .expect("registration")
            .expect("physical owner");
        {
            let connection = store.0.lock().expect("connection");
            connection
                .execute("DELETE FROM background_job_reservations", [])
                .expect("ordinary TTL cleanup");
            connection
                .execute(
                    "UPDATE background_jobs SET fence=2,lease_expires_ms=50000 WHERE id=?1",
                    [&job.job_id],
                )
                .expect("later generation");
            assert_eq!(connection.query_row("SELECT count(*) FROM source_io_occupancy WHERE active_occupancy=1 AND custody_id IS NOT NULL",[],|row|row.get::<_,i64>(0)).expect("charge"),1);
        }
        let resolved = register_background_custody(&store, owner, 40000)
            .await
            .expect("lost ack resolves old exact owner")
            .expect("receipt");
        assert_eq!(resolved.custody_id, receipt.custody_id);
        assert!(!replace(&store, Vec::new(), 40001)
            .await
            .expect("mapping retains old physical identity"));
        let foreground = register_foreground(&store, owner_for_overlap(), 40002, 50000)
            .await
            .expect("foreground doesn't wait for failed reap")
            .expect("foreground owner");
        assert!(release_foreground(&store, foreground, 40003)
            .await
            .expect("foreground settled"));
        assert!(release_background_custody(&store, receipt.clone(), 40004)
            .await
            .expect("old exact read/group physically joined"));
        assert!(!release_background_custody(&store, receipt, 40005)
            .await
            .expect("idempotent settlement"));
        assert_eq!(
            store
                .0
                .lock()
                .expect("connection")
                .query_row("SELECT count(*) FROM source_io_occupancy", [], |row| row
                    .get::<_, i64>(0))
                .expect("no phantom occupancy"),
            0
        );
    }
    fn owner_for_overlap() -> ForegroundSourceOwner {
        owner(ForegroundSourcePolicy::Transfer)
    }
    #[tokio::test]
    async fn background_epoch_budget_is_shared_declared_headroom_not_a_per_reader_minimum() {
        let store = fixture();
        let a = BackgroundSourceOwner::Job {
            token: seed_job(&store),
        };
        let b = BackgroundSourceOwner::Job {
            token: seed_job(&store),
        };
        register_background_custody(&store, a.clone(), 1000)
            .await
            .expect("first custody")
            .expect("first");
        register_background_custody(&store, b.clone(), 1000)
            .await
            .expect("second custody")
            .expect("second");
        let foreground =
            register_foreground(&store, owner(ForegroundSourcePolicy::Transfer), 1000, 10000)
                .await
                .expect("foreground")
                .expect("token");
        let mut service = ForegroundSourceProjection::startup(1001);
        service.phase = ForegroundSourcePhase::TransferService;
        service.observation_version = 1;
        service.observation_expires_at_ms = 5000;
        service.transfer_grant = Some(SourceServiceGrant {
            epoch_id: 0,
            starts_at_ms: 0,
            expires_at_ms: 5000,
            foreground_bytes: 16 * 1048576,
            background_bytes: 8 * 1048576,
            foreground_floor_bps: None,
            supply_floor_bps: None,
            service_class: SourceServiceClass::WeightedUnqualified,
        });
        project_foreground(&store, foreground, service, 1001, 10000)
            .await
            .expect("finite service")
            .expect("token");
        let first = claim_background_service(&store, a.clone(), 1002)
            .await
            .expect("first grant")
            .expect("grant");
        let second = claim_background_service(&store, b, 1002)
            .await
            .expect("second grant")
            .expect("grant");
        assert_eq!(first.byte_allowance, Some(4 * 1048576));
        assert_eq!(second.byte_allowance, Some(4 * 1048576));
        let replay = claim_background_service(&store, a.clone(), 1003)
            .await
            .expect("lost ack replay")
            .expect("same receipt");
        assert_eq!(
            first.receipt_id, replay.receipt_id,
            "replay must not reset a physical owner's spend ledger"
        );
        assert_eq!(store.0.lock().expect("connection").query_row("SELECT allocated_bytes FROM background_source_service_epochs WHERE resource_key='source_io' AND epoch_id=0",[],|row|row.get::<_,i64>(0)).expect("aggregate"),8*1048576);
        register_foreground(&store, owner(ForegroundSourcePolicy::Rolling), 1004, 10000)
            .await
            .expect("second viewer starts immediately")
            .expect("startup");
        assert!(claim_background_service(&store, a, 1005)
            .await
            .expect("new foreground revokes opportunity")
            .is_none());
    }
    #[tokio::test]
    async fn rolling_fallback_interest_receipts_preserve_other_viewers_and_never_revive_a_successor(
    ) {
        let store = fixture();
        let analysis = uuid::Uuid::new_v4().to_string();
        let incarnation = uuid::Uuid::new_v4().to_string();
        {
            let connection = store.0.lock().expect("connection");
            let retired = include_str!("background_jobs_viewer_analysis.sql")
                .split("-- next statement\n")
                .find(|sql| {
                    sql.contains("CREATE TRIGGER IF NOT EXISTS background_analysis_viewer_retired")
                })
                .expect("actual reduction trigger");
            connection
                .execute_batch(retired)
                .expect("existing authority reduction");
            connection.execute("INSERT INTO analysis_requests VALUES(?1,'worker',0,'queued',0,0,'generation',1,100,1,'foreground','playback',1000,0,'fragment_index')",[&analysis]).expect("durable preparation");
            for (consumer, user) in [("viewer-a", 1), ("viewer-b", 2)] {
                connection.execute("INSERT INTO background_job_waiters VALUES('playback-analysis',?1,'pending',1000,'generation',?2,'playback_analysis',?3,3,10000)",rusqlite::params![consumer,analysis,serde_json::json!({"user_id":user,"playback_id":"movie","file_id":1,"analysis_request_id":analysis}).to_string()]).expect("separate viewer");
            }
            connection
                .execute(
                    "INSERT INTO media_session_requests VALUES(1,'movie',?1,'starting',10000)",
                    [&incarnation],
                )
                .expect("exact original intent");
        }
        let receipt = PlaybackAnalysisSuspension {
            receipt_id: uuid::Uuid::new_v4().to_string(),
            principal: crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 },
            playback_id: "movie".into(),
            incarnation_id: incarnation.clone(),
            file_id: 1,
            source_size: 100,
            source_mtime: 1,
        };
        let state = |consumer: &str| {
            store
                .0
                .lock()
                .expect("connection")
                .query_row(
                    "SELECT state FROM background_job_waiters WHERE request_id=?1",
                    [consumer],
                    |row| row.get::<_, String>(0),
                )
                .expect("waiter")
        };
        let first = suspend_analysis_interest(&store, receipt.clone(), 1001)
            .await
            .expect("suspend exact viewer")
            .expect("receipt");
        assert_eq!(state("viewer-a"), "cancelled");
        assert_eq!(state("viewer-b"), "pending");
        store.0.lock().expect("connection").execute("UPDATE background_job_waiters SET state='pending',updated_at_ms=1002 WHERE request_id='viewer-a'",[]).expect("join/refresh attempt");
        assert_eq!(
            state("viewer-a"),
            "cancelled",
            "active fallback cannot regain foreground preparation priority"
        );
        assert!(restore_analysis_interest(&store, first, 1003)
            .await
            .expect("failed fallback restores same starting demand"));
        assert_eq!(state("viewer-a"), "pending");
        let second = suspend_analysis_interest(
            &store,
            PlaybackAnalysisSuspension {
                receipt_id: uuid::Uuid::new_v4().to_string(),
                ..receipt.clone()
            },
            1004,
        )
        .await
        .expect("second fallback")
        .expect("second receipt");
        let third = suspend_analysis_interest(
            &store,
            PlaybackAnalysisSuspension {
                receipt_id: uuid::Uuid::new_v4().to_string(),
                ..receipt
            },
            1005,
        )
        .await
        .expect("overlapping fallback")
        .expect("separate receipt");
        restore_analysis_interest(&store, second, 1006)
            .await
            .expect("one fallback retires");
        assert_eq!(state("viewer-a"), "cancelled");
        store
            .0
            .lock()
            .expect("connection")
            .execute(
                "UPDATE media_session_requests SET state='failed' WHERE user_id=1",
                [],
            )
            .expect("original intent ended");
        restore_analysis_interest(&store, third, 1007)
            .await
            .expect("last fallback retires");
        assert_eq!(state("viewer-a"), "cancelled");
        assert_eq!(state("viewer-b"), "pending");
        assert_eq!(
            store
                .0
                .lock()
                .expect("connection")
                .query_row("SELECT count(*) FROM analysis_requests", [], |row| row
                    .get::<_, i64>(0))
                .expect("durable preparation survives"),
            1
        );
    }
    #[tokio::test]
    async fn mapped_and_global_source_capacity_conflict_in_both_directions() {
        for (held_keys, target_key) in [
            (["source_io", "source_io:disk-a"], "source_io"),
            (["source_io", "source_io"], "source_io:disk-a"),
        ] {
            let store = fixture();
            let first = seed_job(&store);
            let second = seed_job(&store);
            let target = seed_job(&store);
            {
                let connection = store.0.lock().expect("connection");
                connection
                    .execute(
                        "UPDATE background_job_reservations SET resource_key=?1 WHERE job_id=?2",
                        rusqlite::params![held_keys[0], first.job_id],
                    )
                    .expect("first domain");
                connection
                    .execute(
                        "UPDATE background_job_reservations SET resource_key=?1 WHERE job_id=?2",
                        rusqlite::params![held_keys[1], second.job_id],
                    )
                    .expect("second domain");
                connection
                    .execute(
                        "UPDATE background_job_reservations SET resource_key=?1 WHERE job_id=?2",
                        rusqlite::params![target_key, target.job_id],
                    )
                    .expect("target domain");
            }
            pause_background(&store, target.clone(), 1000)
                .await
                .expect("verified target pause");
            assert!(
                !resume_background(&store, target.clone(), 1001)
                    .await
                    .expect("shared source capacity"),
                "{target_key} must conflict with both {held_keys:?}"
            );
            pause_background(&store, first, 1002)
                .await
                .expect("one active reader stops");
            assert!(resume_background(&store, target, 1003)
                .await
                .expect("source credit reacquired after confirmed pause"));
        }
    }
    #[tokio::test]
    async fn same_epoch_unlimited_receipt_is_capped_after_foreground_pressure_without_resetting_identity(
    ) {
        let store = fixture();
        let background = BackgroundSourceOwner::Job {
            token: seed_job(&store),
        };
        register_background_custody(&store, background.clone(), 1000)
            .await
            .expect("physical source")
            .expect("receipt");
        let original = claim_background_service(&store, background.clone(), 1001)
            .await
            .expect("idle service")
            .expect("unlimited class");
        assert_eq!(original.byte_allowance, None);
        let foreground =
            register_foreground(&store, owner(ForegroundSourcePolicy::Transfer), 1002, 10000)
                .await
                .expect("foreground priority")
                .expect("token");
        assert!(claim_background_service(&store, background.clone(), 1003)
            .await
            .expect("startup revokes service")
            .is_none());
        let mut projection = ForegroundSourceProjection::startup(1004);
        projection.phase = ForegroundSourcePhase::TransferService;
        projection.observation_version = 1;
        projection.observation_expires_at_ms = 5000;
        projection.transfer_grant = Some(SourceServiceGrant {
            epoch_id: 0,
            starts_at_ms: 0,
            expires_at_ms: 5000,
            foreground_bytes: 16 * 1048576,
            background_bytes: 8 * 1048576,
            foreground_floor_bps: None,
            supply_floor_bps: None,
            service_class: SourceServiceClass::WeightedUnqualified,
        });
        project_foreground(&store, foreground, projection.clone(), 1004, 10000)
            .await
            .expect("finite transfer")
            .expect("token");
        let capped = claim_background_service(&store, background.clone(), 1005)
            .await
            .expect("resume revalidates old receipt")
            .expect("finite");
        assert_eq!(
            capped.receipt_id, original.receipt_id,
            "all bytes spent before pressure remain charged by the caller's shared receipt ledger"
        );
        assert_eq!(capped.byte_allowance, Some(8 * 1048576));
        let other =
            register_foreground(&store, owner(ForegroundSourcePolicy::Transfer), 1006, 10000)
                .await
                .expect("another viewer")
                .expect("token");
        projection.observed_at_ms = 1007;
        projection
            .transfer_grant
            .as_mut()
            .expect("grant")
            .foreground_bytes = 8 * 1048576;
        projection
            .transfer_grant
            .as_mut()
            .expect("grant")
            .background_bytes = 4 * 1048576;
        project_foreground(&store, other, projection, 1007, 10000)
            .await
            .expect("lower current shared allowance")
            .expect("token");
        let revoked = claim_background_service(&store, background, 1008)
            .await
            .expect("all-owner budget reduction")
            .expect("same receipt");
        assert_eq!(revoked.receipt_id, original.receipt_id);
        assert_eq!(revoked.byte_allowance, Some(0));
        assert_eq!(
            store
                .0
                .lock()
                .expect("connection")
                .query_row(
                    "SELECT allocated_bytes FROM background_source_service_grants",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("old promise remains charged"),
            8 * 1048576
        );
    }
    #[tokio::test]
    async fn read_attestation_lease_yields_to_foreground_and_close_fences_delayed_registration() {
        let store = fixture();
        let lease = crate::cluster::coordination::Lease {
            resource: "subtitle-read-attest:worker:0".into(),
            owner_node_id: "worker".into(),
            fence: 1,
            revision: 1,
            expires_at_unix_ms: 30000,
        };
        store
            .0
            .lock()
            .expect("connection")
            .execute(
                "INSERT INTO job_leases VALUES(?1,'worker',1,1,30000,1000)",
                [&lease.resource],
            )
            .expect("real bounded preparation lease");
        let read = BackgroundSourceOwner::ReadAttestation {
            lease: lease.clone(),
            file_id: 1,
            source_size: 100,
            source_mtime: 1,
        };
        let foreground =
            register_foreground(&store, owner(ForegroundSourcePolicy::Transfer), 1000, 10000)
                .await
                .expect("foreground")
                .expect("token");
        assert!(matches!(
            register_background_typed(&store, read.clone(), 1001)
                .await
                .expect("preparation admission"),
            BackgroundSourceRegistration::LostOwnership
        ));
        assert_eq!(
            store
                .0
                .lock()
                .expect("connection")
                .query_row(
                    "SELECT count(*) FROM background_source_custody",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("no competing hash"),
            0
        );
        close_read_attestation_lease(&store, read.clone(), 1002)
            .await
            .expect("joined/no-input close");
        assert!(
            register_background_custody(&store, read.clone(), 1001)
                .await
                .expect("delayed old clock registration")
                .is_none(),
            "closed revision must fence even an old now_ms"
        );
        release_foreground(&store, foreground, 1003)
            .await
            .expect("foreground settled");
        let resumed_lease = crate::cluster::coordination::Lease {
            fence: 2,
            revision: 3,
            ..lease
        };
        store
            .0
            .lock()
            .expect("connection")
            .execute(
                "UPDATE job_leases SET fence=2,revision=3,expires_at_ms=30000",
                [],
            )
            .expect("next bounded slot incarnation");
        let resumed = BackgroundSourceOwner::ReadAttestation {
            lease: resumed_lease,
            file_id: 1,
            source_size: 100,
            source_mtime: 1,
        };
        let receipt = match register_background_typed(&store, resumed.clone(), 1004)
            .await
            .expect("resume only after source admission")
        {
            BackgroundSourceRegistration::Registered(receipt) => receipt,
            other => panic!("expected read custody, got {other:?}"),
        };
        assert!(claim_background_service(&store, resumed.clone(), 1005)
            .await
            .expect("finite service owner")
            .is_some());
        store
            .0
            .lock()
            .expect("connection")
            .execute(
                "UPDATE job_leases SET fence=3,revision=4,expires_at_ms=40000",
                [],
            )
            .expect("later lease generation");
        assert!(!background_may_continue(&store, resumed.clone(), 1006)
            .await
            .expect("old hash cannot read"));
        assert!(!close_read_attestation_lease(&store, resumed, 1007)
            .await
            .expect("stale cleanup cannot close successor"));
        assert_eq!(store.0.lock().expect("connection").query_row("SELECT count(*) FROM source_io_occupancy WHERE custody_id IS NOT NULL AND active_occupancy=1",[],|row|row.get::<_,i64>(0)).expect("old syscall custody survives"),1);
        assert!(release_background_custody(&store, receipt, 1008)
            .await
            .expect("only after blocking join"));
    }
    #[tokio::test]
    async fn offline_source_owner_uses_exact_package_generation_and_keeps_old_physical_custody() {
        let store = fixture();
        let package = uuid::Uuid::new_v4().to_string();
        store
            .0
            .lock()
            .expect("connection")
            .execute(
                "INSERT INTO offline_packages VALUES(?1,'worker',1,100,1,1,'preparing',60)",
                [&package],
            )
            .expect("existing offline claim");
        let owner = BackgroundSourceOwner::Offline {
            package_id: package.clone(),
            node_id: "worker".into(),
            claim_generation: 1,
        };
        let invalid = BackgroundSourceOwner::Offline {
            package_id: package.clone(),
            node_id: "worker".into(),
            claim_generation: 2,
        };
        assert!(matches!(
            register_background_typed(&store, invalid, 1000)
                .await
                .expect("wrong generation"),
            BackgroundSourceRegistration::LostOwnership
        ));
        let receipt = match register_background_typed(&store, owner.clone(), 1001)
            .await
            .expect("offline original-source admission")
        {
            BackgroundSourceRegistration::Registered(receipt) => receipt,
            other => panic!("expected offline custody, got {other:?}"),
        };
        register_foreground(&store, owner_for_overlap(), 1002, 10000)
            .await
            .expect("playback never waits for offline reap")
            .expect("token");
        assert!(!background_may_continue(&store, owner.clone(), 1003)
            .await
            .expect("offline yields to startup"));
        store
            .0
            .lock()
            .expect("connection")
            .execute(
                "UPDATE offline_packages SET claim_generation=2,state='queued'",
                [],
            )
            .expect("ownership lost");
        assert_eq!(store.0.lock().expect("connection").query_row("SELECT count(*) FROM source_io_occupancy WHERE custody_id IS NOT NULL AND active_occupancy=1",[],|row|row.get::<_,i64>(0)).expect("no clock/requeue reap"),1);
        release_background_custody(&store, receipt, 1004)
            .await
            .expect("exact physical producer completion");
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackAnalysisSuspension {
    pub receipt_id: String,
    pub principal: crate::playback_principal::PlaybackPrincipal,
    pub playback_id: String,
    pub incarnation_id: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
}
impl PlaybackAnalysisSuspension {
    fn valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.receipt_id).is_ok()
            && self.principal.local_user_id().is_some_and(|id| id > 0)
            && !self.playback_id.is_empty()
            && self.playback_id.len() <= 128
            && uuid::Uuid::parse_str(&self.incarnation_id).is_ok()
            && self.file_id > 0
            && self.source_size >= 0
            && self.source_mtime >= 0
    }
}
pub(super) async fn suspend_analysis_interest<T: QueueSql>(
    store: &T,
    receipt: PlaybackAnalysisSuspension,
    now_ms: i64,
) -> Result<Option<PlaybackAnalysisSuspension>, StoreError> {
    if !receipt.valid() || now_ms < 0 {
        return Err(StoreError::Task("invalid exact fallback interest".into()));
    }
    let mut body = serde_json::to_value(&receipt).map_err(|e| StoreError::Task(e.to_string()))?;
    body["user_id"] = receipt.principal.local_user_id().into();
    body["now_ms"] = now_ms.into();
    body["id"] = uuid::Uuid::new_v4().to_string().into();
    let rows=store.queue_sql(r#"INSERT INTO background_job_commands(id,operation,request_json,result_json)
      SELECT json_extract($1,'$.id'),'source_suspend_interest',$1,CASE WHEN EXISTS(
        SELECT 1 FROM background_job_waiters waiter JOIN analysis_requests request ON request.request_id=json_extract(waiter.consumer_ref,'$.analysis_request_id')
        JOIN files file ON file.id=request.file_id AND file.size=request.source_size AND file.mtime=request.source_mtime
        WHERE waiter.request_scope='playback-analysis' AND (waiter.state IN ('pending','awaiting_hydration') OR (waiter.state='cancelled' AND EXISTS(SELECT 1 FROM playback_analysis_suspensions prior WHERE prior.consumer_id=waiter.request_id AND prior.phase='suspended' AND prior.expires_at_ms>json_extract($1,'$.now_ms'))))
          AND waiter.deadline_ms>json_extract($1,'$.now_ms') AND waiter.request_digest=request.requested_generation
          AND json_extract(waiter.consumer_ref,'$.user_id')=json_extract($1,'$.user_id')
          AND json_extract(waiter.consumer_ref,'$.playback_id')=json_extract($1,'$.playback_id')
          AND request.file_id=json_extract($1,'$.file_id') AND request.source_size=json_extract($1,'$.source_size')
          AND request.source_mtime=json_extract($1,'$.source_mtime') AND request.state IN ('queued','running','submitted'))
        THEN 'true' ELSE 'false' END RETURNING result_json"#.into(),encode(&body)?,true,true).await?;
    Ok(rows
        .first()
        .map(|row| decode::<bool>(row))
        .transpose()?
        .unwrap_or(false)
        .then_some(receipt))
}
pub(super) async fn restore_analysis_interest<T: QueueSql>(
    store: &T,
    receipt: PlaybackAnalysisSuspension,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !receipt.valid() || now_ms < 0 {
        return Err(StoreError::Task(
            "invalid exact fallback restoration".into(),
        ));
    }
    let mut body = serde_json::to_value(&receipt).map_err(|e| StoreError::Task(e.to_string()))?;
    body["user_id"] = receipt.principal.local_user_id().into();
    body["now_ms"] = now_ms.into();
    body["id"] = uuid::Uuid::new_v4().to_string().into();
    let rows=store.queue_sql(r#"INSERT INTO background_job_commands(id,operation,request_json,result_json)
      SELECT json_extract($1,'$.id'),'source_restore_interest',$1,CASE WHEN EXISTS(SELECT 1 FROM playback_analysis_suspensions held
        WHERE held.receipt_id=json_extract($1,'$.receipt_id') AND held.user_id=json_extract($1,'$.user_id')
          AND held.playback_id=json_extract($1,'$.playback_id') AND held.incarnation_id=json_extract($1,'$.incarnation_id')
          AND held.file_id=json_extract($1,'$.file_id') AND held.source_size=json_extract($1,'$.source_size') AND held.source_mtime=json_extract($1,'$.source_mtime'))
        THEN 'true' ELSE 'false' END RETURNING result_json"#.into(),encode(&body)?,true,true).await?;
    Ok(rows
        .first()
        .map(|row| decode::<bool>(row))
        .transpose()?
        .unwrap_or(false))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundSourceCustodyToken {
    pub custody_id: String,
    pub owner: BackgroundSourceOwner,
}
fn background_owner_key(owner: &BackgroundSourceOwner) -> Result<String, StoreError> {
    Ok(match owner {
        BackgroundSourceOwner::Job { token } => {
            token.validate()?;
            crate::segplan::argv_fingerprint(&[
                "job".into(),
                token.job_id.clone(),
                token.node_id.clone(),
                token.boot_id.clone(),
                token.claim_id.clone(),
                token.fence.to_string(),
            ])
        }
        BackgroundSourceOwner::Analysis {
            request_id,
            node_id,
            fence,
        } => {
            if uuid::Uuid::parse_str(request_id).is_err() || !identifier(node_id) || *fence <= 0 {
                return Err(StoreError::Task("invalid analysis source custody".into()));
            }
            crate::segplan::argv_fingerprint(&[
                "analysis".into(),
                request_id.clone(),
                node_id.clone(),
                fence.to_string(),
            ])
        }
        BackgroundSourceOwner::ReadAttestation {
            lease,
            file_id,
            source_size,
            source_mtime,
        } => {
            let prefix = format!("subtitle-read-attest:{}:", lease.owner_node_id);
            if !identifier(&lease.owner_node_id)
                || !lease
                    .resource
                    .strip_prefix(&prefix)
                    .and_then(|slot| slot.parse::<u8>().ok())
                    .is_some_and(|slot| slot < 64)
                || lease.fence == 0
                || lease.fence > i64::MAX as u64
                || lease.revision == 0
                || lease.revision > i64::MAX as u64
                || *file_id <= 0
                || *source_size < 0
                || *source_mtime < 0
            {
                return Err(StoreError::Task(
                    "invalid source read-preparation lease".into(),
                ));
            }
            crate::segplan::argv_fingerprint(&[
                "read_attestation".into(),
                lease.resource.clone(),
                lease.owner_node_id.clone(),
                lease.fence.to_string(),
                file_id.to_string(),
                source_size.to_string(),
                source_mtime.to_string(),
            ])
        }
        BackgroundSourceOwner::Offline {
            package_id,
            node_id,
            claim_generation,
        } => {
            if uuid::Uuid::parse_str(package_id).is_err()
                || !identifier(node_id)
                || *claim_generation <= 0
            {
                return Err(StoreError::Task("invalid offline source owner".into()));
            }
            crate::segplan::argv_fingerprint(&[
                "offline".into(),
                package_id.clone(),
                node_id.clone(),
                claim_generation.to_string(),
            ])
        }
    })
}
pub(super) async fn register_background_custody<T: QueueSql>(
    store: &T,
    owner: BackgroundSourceOwner,
    now_ms: i64,
) -> Result<Option<BackgroundSourceCustodyToken>, StoreError> {
    if now_ms < 0 {
        return Err(StoreError::Task("invalid source custody clock".into()));
    }
    let owner_key = background_owner_key(&owner)?;
    // Resolve a lost registration acknowledgement by exact physical owner even
    // after its demand lease expired. This receipt never authorizes new input;
    // the caller must revalidate authority or settle it without starting reads.
    let existing=store.queue_sql("SELECT json_object('custody_id',custody_id,'owner',json(owner_json)) AS result_json FROM background_source_custody WHERE owner_key=json_extract($1,'$.owner_key')".into(),encode(&serde_json::json!({"owner_key":owner_key}))?,false,true).await?;
    if let Some(row) = existing.first() {
        return decode(row).map(Some);
    }
    let mut identity = background_owner_identity(&owner);
    let resources = background_owner_resources(&owner);
    if matches!(owner, BackgroundSourceOwner::ReadAttestation { .. }) {
        identity=format!("{identity} AND EXISTS(SELECT 1 FROM job_leases exact WHERE exact.resource=json_extract($1,'$.lease.resource') AND exact.revision=json_extract($1,'$.lease.revision') AND exact.expires_at_ms=json_extract($1,'$.lease.expires_at_unix_ms'))");
    }
    let (kind,work,fence,lease)=match &owner {
        BackgroundSourceOwner::Job{..}=>("job","json_extract($1,'$.token.job_id')","json_extract($1,'$.token.fence')","json_extract($1,'$.token.lease_expires_ms')"),
        BackgroundSourceOwner::Analysis{..}=>("analysis","json_extract($1,'$.request_id')","json_extract($1,'$.fence')","(SELECT lease_expires_ms FROM analysis_requests WHERE request_id=json_extract($1,'$.request_id'))"),
        BackgroundSourceOwner::ReadAttestation{..}=>("lease","json_extract($1,'$.lease.resource')","json_extract($1,'$.lease.fence')","json_extract($1,'$.lease.expires_at_unix_ms')"),
        BackgroundSourceOwner::Offline{..}=>("offline","json_extract($1,'$.package_id')","json_extract($1,'$.claim_generation')","(SELECT expires_at*1000 FROM offline_packages WHERE id=json_extract($1,'$.package_id'))"),
    };
    if matches!(
        owner,
        BackgroundSourceOwner::ReadAttestation { .. } | BackgroundSourceOwner::Offline { .. }
    ) {
        let blocked = foreground_blocks("required.resource_key", "json_extract($1,'$.now_ms')");
        identity=format!("{identity} AND NOT EXISTS(SELECT 1 FROM ({resources}) required WHERE {blocked} OR (SELECT count(DISTINCT json_array(held.owner_kind,held.work_id,held.fence)) FROM source_io_occupancy held WHERE (held.resource_key=required.resource_key OR held.resource_key='source_io' OR required.resource_key='source_io') AND held.active_occupancy=1 AND (held.expires_at_ms>json_extract($1,'$.now_ms') OR held.custody_id IS NOT NULL))>=1)");
    }
    let mut body = serde_json::to_value(&owner).map_err(|e| StoreError::Task(e.to_string()))?;
    body["owner"] = serde_json::to_value(&owner).map_err(|e| StoreError::Task(e.to_string()))?;
    body["now_ms"] = now_ms.into();
    body["owner_key"] = owner_key.into();
    body["custody_id"] = uuid::Uuid::new_v4().to_string().into();
    let rows=store.queue_sql(format!(r#"INSERT INTO background_source_custody(custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,phase,started_at_ms,updated_at_ms,lease_expires_at_ms)
      SELECT json_extract($1,'$.custody_id'),json_extract($1,'$.owner_key'),json_extract($1,'$.owner'),
        (SELECT json_group_array(resource_key) FROM ({resources})),'{kind}',{work},{fence},'active',json_extract($1,'$.now_ms'),json_extract($1,'$.now_ms'),{lease}
      WHERE {identity} AND EXISTS(SELECT 1 FROM ({resources}))
      ON CONFLICT(owner_key) DO UPDATE SET updated_at_ms=background_source_custody.updated_at_ms
      RETURNING json_object('custody_id',custody_id,'owner',json(owner_json)) AS result_json"#),encode(&body)?,true,true).await?;
    rows.first().map(|row| decode(row)).transpose()
}
pub(super) async fn release_background_custody<T: QueueSql>(
    store: &T,
    token: BackgroundSourceCustodyToken,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if uuid::Uuid::parse_str(&token.custody_id).is_err() || now_ms < 0 {
        return Err(StoreError::Task("invalid source settlement receipt".into()));
    }
    let key = background_owner_key(&token.owner)?;
    let rows=store.queue_sql("DELETE FROM background_source_custody WHERE custody_id=json_extract($1,'$.custody_id') AND owner_key=json_extract($1,'$.owner_key') RETURNING 'true' AS result_json".into(),encode(&serde_json::json!({"custody_id":token.custody_id,"owner_key":key}))?,true,true).await?;
    Ok(!rows.is_empty())
}

/// One physical owner's shared budget. Replaying a receipt never replenishes
/// local bytes: all readers of this owner share its receipt-id spend ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundSourceServiceGrant {
    pub receipt_id: String,
    pub epoch_id: i64,
    pub expires_at_ms: i64,
    pub byte_allowance: Option<u64>,
    pub resource_keys: Vec<String>,
}
pub(super) async fn claim_background_service<T: QueueSql>(
    store: &T,
    owner: BackgroundSourceOwner,
    now_ms: i64,
) -> Result<Option<BackgroundSourceServiceGrant>, StoreError> {
    if !(0..=i64::MAX - 5000).contains(&now_ms) {
        return Err(StoreError::Task("invalid source service epoch".into()));
    }
    let owner_key = background_owner_key(&owner)?;
    let identity = background_owner_identity(&owner);
    let blocked = foreground_blocks("resource.value", "json_extract($1,'$.now_ms')");
    let mut body = serde_json::to_value(&owner).map_err(|e| StoreError::Task(e.to_string()))?;
    body["id"] = uuid::Uuid::new_v4().to_string().into();
    body["owner_key"] = owner_key.into();
    body["now_ms"] = now_ms.into();
    body["epoch_id"] = (now_ms / 5000).into();
    let eligibility = format!(
        r#"{identity} AND EXISTS(SELECT 1 FROM background_source_custody owned WHERE owned.owner_key=json_extract($1,'$.owner_key') AND owned.phase IN ('active','paused')) AND NOT EXISTS(SELECT 1 FROM background_source_custody custody,json_each(custody.resource_keys_json) resource
      WHERE custody.owner_key=json_extract($1,'$.owner_key') AND {blocked})"#
    );
    // Revalidate and reduce a replay in the same transaction as allocation.
    // A pre-pressure unlimited receipt becomes finite under the SAME identity;
    // its local spend ledger therefore cannot be reset by a resumed reader.
    let sql = format!(
        r#"INSERT INTO background_job_commands(id,operation,request_json,result_json)
      WITH custody AS (SELECT * FROM background_source_custody WHERE owner_key=json_extract($1,'$.owner_key')),
      prior AS (SELECT * FROM background_source_service_grants WHERE owner_key=json_extract($1,'$.owner_key') AND epoch_id=json_extract($1,'$.epoch_id')),
      resource_set AS (SELECT CASE WHEN EXISTS(SELECT 1 FROM foreground_source_permissions fg WHERE fg.resource_key='source_io'
          AND (fg.lease_expires_at_ms>json_extract($1,'$.now_ms') OR fg.custody_held=1))
          OR EXISTS(SELECT 1 FROM background_source_custody peer,json_each(peer.resource_keys_json) resource WHERE resource.value='source_io')
        THEN '["source_io"]' ELSE resource_keys_json END AS resources FROM custody),
      policy AS (SELECT EXISTS(SELECT 1 FROM custody,json_each(custody.resource_keys_json) resource
          JOIN foreground_source_required_resources fg_resource ON fg_resource.resource_key=resource.value OR fg_resource.resource_key='source_io' OR resource.value='source_io'
          JOIN foreground_source_owners fg ON fg.owner_id=fg_resource.owner_id WHERE fg.phase='transfer_service' AND fg.lease_expires_at_ms>json_extract($1,'$.now_ms')) AS finite,
        (SELECT MIN(json_extract(fg.projection_json,'$.transfer_grant.background_bytes')) FROM custody,json_each(custody.resource_keys_json) resource
          JOIN foreground_source_required_resources fg_resource ON fg_resource.resource_key=resource.value OR fg_resource.resource_key='source_io' OR resource.value='source_io'
          JOIN foreground_source_owners fg ON fg.owner_id=fg_resource.owner_id WHERE fg.phase='transfer_service'
            AND fg.lease_expires_at_ms>json_extract($1,'$.now_ms')
            AND json_extract(fg.projection_json,'$.transfer_grant.expires_at_ms')>json_extract($1,'$.now_ms')) AS quota,
        (SELECT MAX(1,COUNT(*)) FROM background_source_custody peer WHERE
          EXISTS(SELECT 1 FROM background_source_current_owners current WHERE current.custody_id=peer.custody_id AND current.authority_expires_at_ms>json_extract($1,'$.now_ms'))
          AND EXISTS(SELECT 1 FROM json_each(peer.resource_keys_json) a,custody,json_each(custody.resource_keys_json) b
            WHERE a.value=b.value OR a.value='source_io' OR b.value='source_io')) AS readers,
        COALESCE((SELECT SUM(grant_row.allocated_bytes) FROM background_source_service_grants grant_row
          WHERE grant_row.epoch_id=json_extract($1,'$.epoch_id') AND EXISTS(SELECT 1 FROM json_each(grant_row.grant_json,'$.resource_keys') held,
            resource_set,json_each(resource_set.resources) required WHERE held.value=required.value OR held.value='source_io' OR required.value='source_io')),0) AS allocated,
        COALESCE((SELECT allocated_bytes FROM prior),0) AS owned),
      allowance AS (SELECT CASE WHEN finite=1 THEN CASE WHEN owned>0 THEN
          CASE WHEN allocated>quota THEN 0 ELSE MIN(owned,quota/readers) END
        ELSE MIN(quota/readers,MAX(0,quota-allocated)) END
        WHEN owned>0 THEN owned ELSE NULL END AS bytes,
        CASE WHEN finite=1 AND owned=0 THEN MIN(quota/readers,MAX(0,quota-allocated)) ELSE 0 END AS charge FROM policy)
      SELECT json_extract($1,'$.id'),'source_service',json_set($1,'$.charge_bytes',COALESCE(allowance.charge,0)),CASE WHEN {eligibility}
          AND EXISTS(SELECT 1 FROM custody) AND (allowance.bytes IS NULL OR allowance.bytes>0 OR EXISTS(SELECT 1 FROM prior))
        THEN json_object('receipt_id',COALESCE((SELECT receipt_id FROM prior),json_extract($1,'$.id')),'epoch_id',json_extract($1,'$.epoch_id'),
          'expires_at_ms',MIN((json_extract($1,'$.epoch_id')+1)*5000,COALESCE((SELECT MIN(fg.permits_through_ms)
            FROM foreground_source_permissions fg,custody,json_each(custody.resource_keys_json) resource
            WHERE (fg.resource_key=resource.value OR fg.resource_key='source_io' OR resource.value='source_io')
              AND (fg.lease_expires_at_ms>json_extract($1,'$.now_ms') OR fg.custody_held=1)
              AND fg.permits_through_ms>json_extract($1,'$.now_ms')),(json_extract($1,'$.epoch_id')+1)*5000)),
          'byte_allowance',allowance.bytes,'resource_keys',json(resource_set.resources))
        ELSE 'false' END FROM allowance,resource_set RETURNING result_json"#
    );

    let rows = store.queue_sql(sql, encode(&body)?, true, true).await?;
    match rows.first() {
        Some(row) if row != "false" => decode(row).map(Some),
        _ => Ok(None),
    }
}

pub(super) async fn resolve_foreground<T: QueueSql>(
    store: &T,
    token: ForegroundSourceToken,
) -> Result<Option<ForegroundSourceToken>, StoreError> {
    if !token.owner.valid() {
        return Err(StoreError::Task(
            "invalid foreground custody resolution".into(),
        ));
    }
    let rows=store.queue_sql(format!("SELECT {} AS result_json FROM foreground_source_owners WHERE owner_id=json_extract($1,'$.owner.owner_id') AND identity_json=json_extract($1,'$.owner')",token_json()),encode(&token)?,false,true).await?;
    rows.first().map(|row| decode(row)).transpose()
}

fn background_owner_identity(owner: &BackgroundSourceOwner) -> String {
    match owner {
        BackgroundSourceOwner::Job{..}=>format!("EXISTS(SELECT 1 FROM background_jobs job WHERE {})",background_identity_sql()),
        BackgroundSourceOwner::Analysis{..}=>"EXISTS(SELECT 1 FROM analysis_requests request WHERE request.request_id=json_extract($1,'$.request_id') AND request.owner_node_id=json_extract($1,'$.node_id') AND request.fence=json_extract($1,'$.fence') AND request.state='running' AND request.cancel_requested=0 AND request.lease_expires_ms>json_extract($1,'$.now_ms') AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||request.owner_node_id))".to_owned(),
        BackgroundSourceOwner::ReadAttestation{..}=>"EXISTS(SELECT 1 FROM job_leases lease JOIN files file ON file.id=json_extract($1,'$.file_id') AND file.size=json_extract($1,'$.source_size') AND file.mtime=json_extract($1,'$.source_mtime') WHERE lease.resource=json_extract($1,'$.lease.resource') AND lease.owner_node_id=json_extract($1,'$.lease.owner_node_id') AND lease.fence=json_extract($1,'$.lease.fence') AND lease.expires_at_ms>json_extract($1,'$.now_ms') AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||lease.owner_node_id))".to_owned(),
        BackgroundSourceOwner::Offline{..}=>"EXISTS(SELECT 1 FROM offline_packages package JOIN files file ON file.id=package.file_id AND file.size=package.source_size AND file.mtime=package.source_mtime WHERE package.id=json_extract($1,'$.package_id') AND package.node_id=json_extract($1,'$.node_id') AND package.claim_generation=json_extract($1,'$.claim_generation') AND package.state='preparing' AND package.expires_at*1000>json_extract($1,'$.now_ms') AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||package.node_id))".to_owned(),
    }
}
fn background_owner_resources(owner: &BackgroundSourceOwner) -> String {
    match owner {
        BackgroundSourceOwner::Job{..}=>"SELECT resource_key FROM background_job_reservations WHERE job_id=json_extract($1,'$.token.job_id') AND fence=json_extract($1,'$.token.fence') AND resource_key LIKE 'source_io%' AND expires_at_ms>json_extract($1,'$.now_ms') UNION SELECT resource.value AS resource_key FROM background_source_pauses paused,json_each(paused.resource_keys_json) resource WHERE paused.job_id=json_extract($1,'$.token.job_id') AND paused.fence=json_extract($1,'$.token.fence')".to_owned(),
        BackgroundSourceOwner::Analysis{..}=>"SELECT resource_key FROM analysis_source_reservations WHERE request_id=json_extract($1,'$.request_id') AND fence=json_extract($1,'$.fence') AND expires_at_ms>json_extract($1,'$.now_ms')".to_owned(),
        BackgroundSourceOwner::ReadAttestation{..}|BackgroundSourceOwner::Offline{..}=>{
            let file=if matches!(owner,BackgroundSourceOwner::Offline{..}){"(SELECT file_id FROM offline_packages WHERE id=json_extract($1,'$.package_id'))"}else{"json_extract($1,'$.file_id')"};
            format!("SELECT DISTINCT COALESCE('source_io:'||mapping.domain_id,'source_io') AS resource_key FROM files file JOIN items item ON item.id=file.item_id LEFT JOIN libraries library ON library.id=item.library_id LEFT JOIN json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root LEFT JOIN background_storage_domains mapping ON mapping.library_id=library.id AND mapping.root_path=root.value WHERE file.id={file}")
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackgroundSourceRegistration {
    Registered(BackgroundSourceCustodyToken),
    NotRequired,
    LostOwnership,
}
pub(super) async fn register_background_typed<T: QueueSql>(
    store: &T,
    owner: BackgroundSourceOwner,
    now_ms: i64,
) -> Result<BackgroundSourceRegistration, StoreError> {
    if now_ms < 0 {
        return Err(StoreError::Task("invalid source registration clock".into()));
    }
    let owner_key = background_owner_key(&owner)?;
    let existing=store.queue_sql("SELECT json_object('custody_id',custody_id,'owner',json(owner_json)) AS result_json FROM background_source_custody WHERE owner_key=json_extract($1,'$.owner_key')".into(),encode(&serde_json::json!({"owner_key":owner_key}))?,false,true).await?;
    if let Some(row) = existing.first() {
        return decode(row).map(BackgroundSourceRegistration::Registered);
    }
    if matches!(owner, BackgroundSourceOwner::Job { .. }) {
        let identity = background_owner_identity(&owner);
        let mut body = serde_json::to_value(&owner).map_err(|e| StoreError::Task(e.to_string()))?;
        body["now_ms"] = now_ms.into();
        let rows=store.queue_sql(format!("SELECT CASE WHEN {identity} AND NOT EXISTS(SELECT 1 FROM background_job_required_resources WHERE job_id=json_extract($1,'$.token.job_id') AND resource_key LIKE 'source_io%') THEN 'true' ELSE 'false' END AS result_json"),encode(&body)?,false,true).await?;
        if rows
            .first()
            .map(|row| decode::<bool>(row))
            .transpose()?
            .unwrap_or(false)
        {
            return Ok(BackgroundSourceRegistration::NotRequired);
        }
    }
    if let Some(receipt) = register_background_custody(store, owner, now_ms).await? {
        return Ok(BackgroundSourceRegistration::Registered(receipt));
    }
    Ok(BackgroundSourceRegistration::LostOwnership)
}

/// Called only by the existing read-preparation owner after all blocking reads
/// joined. Closing its exact fence also fences a delayed registration write;
/// revision uncertainty from a lost renewal ack cannot revive input authority.
pub(super) async fn close_read_attestation_lease<T: QueueSql>(
    store: &T,
    owner: BackgroundSourceOwner,
    now_ms: i64,
) -> Result<bool, StoreError> {
    background_owner_key(&owner)?;
    if !matches!(owner, BackgroundSourceOwner::ReadAttestation { .. }) || now_ms < 0 {
        return Err(StoreError::Task(
            "invalid read-preparation settlement".into(),
        ));
    }
    let mut body = serde_json::to_value(owner).map_err(|e| StoreError::Task(e.to_string()))?;
    body["now_ms"] = now_ms.into();
    let rows=store.queue_sql("UPDATE job_leases SET expires_at_ms=MIN(expires_at_ms,json_extract($1,'$.now_ms')),revision=CASE WHEN revision<9223372036854775807 THEN revision+1 ELSE revision END,updated_at_ms=json_extract($1,'$.now_ms') WHERE resource=json_extract($1,'$.lease.resource') AND owner_node_id=json_extract($1,'$.lease.owner_node_id') AND fence=json_extract($1,'$.lease.fence') RETURNING 'true' AS result_json".into(),encode(&body)?,true,true).await?;
    Ok(!rows.is_empty())
}
