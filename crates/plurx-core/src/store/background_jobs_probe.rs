//! Pure leaf probes. Workers publish bounded facts; only the owning coordinator
//! can apply them to the exact catalogue file generation.
use super::background_jobs::{
    decode, encode, identifier, JobPayload, JobPublishOutcome, JobToken, QueueSql,
};
use crate::{cluster::coordination::Lease, domain::ProbeResult, error::StoreError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const SCHEMA: &str = include_str!("background_jobs_probe.sql");
pub const MAX_PROBE_RESULT_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeCoordinator {
    pub resource: String,
    pub node_id: String,
    pub fence: u64,
}
impl ProbeCoordinator {
    pub fn from_lease(lease: &Lease) -> Self {
        Self {
            resource: lease.resource.clone(),
            node_id: lease.owner_node_id.clone(),
            fence: lease.fence,
        }
    }
    pub fn validate(&self) -> bool {
        (self.resource == "repair:probe"
            || self
                .resource
                .strip_prefix("scan:library:")
                .is_some_and(|id| id.parse::<i64>().is_ok_and(|id| id > 0)))
            && identifier(&self.node_id)
            && self.fence > 0
            && self.fence <= i64::MAX as u64
    }
}
pub fn generation(
    file_id: i64,
    size: i64,
    mtime: i64,
    coordinator: &ProbeCoordinator,
    pipeline: &str,
) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(file_id, size, mtime, coordinator, pipeline)).expect("probe identity"),
    ))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeOutput {
    pub source: JobPayload,
    pub probe: ProbeResult,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishProbeJob {
    pub token: JobToken,
    pub output: ProbeOutput,
    pub now_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyProbeJob {
    pub job_id: String,
    pub lease: Lease,
    pub now_ms: i64,
}

pub(super) async fn publish<T: QueueSql>(
    store: &T,
    input: PublishProbeJob,
) -> Result<JobPublishOutcome, StoreError> {
    input.token.validate()?;
    input.output.source.validate()?;
    if !matches!(input.output.source, JobPayload::MediaProbe { .. })
        || input.now_ms < 0
        || encode(&input.output)?.len() > MAX_PROBE_RESULT_BYTES
    {
        return Err(StoreError::Task("invalid or oversized probe output".into()));
    }
    // Match enqueue's canonical object-key ordering for exact payload identity.
    let body = serde_json::to_value(&input).map_err(|error| StoreError::Task(error.to_string()))?;
    let rows = store
        .queue_sql(PUBLISH_SQL.into(), encode(&body)?, true, true)
        .await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Task("missing probe publication verdict".into()))?,
    )
}
pub(super) async fn apply<T: QueueSql>(
    store: &T,
    input: ApplyProbeJob,
) -> Result<bool, StoreError> {
    if uuid::Uuid::parse_str(&input.job_id).is_err()
        || !ProbeCoordinator::from_lease(&input.lease).validate()
        || input.now_ms < 0
    {
        return Err(StoreError::Task("invalid probe application".into()));
    }
    Ok(!store
        .queue_sql(APPLY_SQL.into(), encode(&input)?, true, false)
        .await?
        .is_empty())
}

const PUBLISH_SQL: &str = r#"
WITH input AS (SELECT json($1) AS body), observation AS (
 SELECT body, job.id, job.state, job.result_ref, json_extract(body,'$.output') AS output,
 EXISTS (SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id = job.id
  AND attempt.fence = job.fence AND attempt.fence = json_extract(body,'$.token.fence')
  AND attempt.claim_id = json_extract(body,'$.token.claim_id')
  AND attempt.owner_node_id = json_extract(body,'$.token.node_id')
  AND attempt.owner_boot_id = json_extract(body,'$.token.boot_id')) AS same_attempt,
 job.state = 'running' AND job.kind = 'media_probe' AND job.payload_version = 1
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
  AND job.payload_json = json_extract(body,'$.output.source')
  AND EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = job.id
    AND interest.state = 'pending' AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract(body,'$.now_ms')))
  AND EXISTS (SELECT 1 FROM job_leases lease
 WHERE lease.resource = json_extract(job.payload_json,'$.coordinator.resource')
 AND lease.owner_node_id = json_extract(job.payload_json,'$.coordinator.node_id')
 AND lease.fence = json_extract(job.payload_json,'$.coordinator.fence')
 AND lease.expires_at_ms > json_extract(body,'$.now_ms')
 AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || lease.owner_node_id)
 AND NOT EXISTS (SELECT 1 FROM background_job_domain_leases binding WHERE binding.resource = lease.resource
   AND (binding.domain_fence != lease.fence OR NOT EXISTS (SELECT 1 FROM background_jobs parent
    WHERE parent.id = binding.job_id AND parent.fence = binding.job_fence AND parent.state = 'running'
     AND parent.owner_node_id = binding.node_id AND parent.owner_boot_id = binding.boot_id AND parent.claim_id = binding.claim_id
     AND parent.lease_expires_ms > json_extract(body,'$.now_ms')))))
  AND EXISTS (SELECT 1 FROM files file JOIN items item ON item.id = file.item_id
 WHERE file.id = json_extract(job.payload_json,'$.file_id')
 AND file.size = json_extract(job.payload_json,'$.source_size') AND file.mtime = json_extract(job.payload_json,'$.source_mtime')
 AND (json_extract(job.payload_json,'$.coordinator.resource') = 'repair:probe'
   OR json_extract(job.payload_json,'$.coordinator.resource') = 'scan:library:' || item.library_id)) AS owns
 FROM input LEFT JOIN background_jobs job ON job.id = json_extract(body,'$.token.job_id')
), verdict AS (
 SELECT *, CASE WHEN state = 'succeeded' AND same_attempt AND result_ref = output THEN 'already_published'
  WHEN NOT COALESCE(owns,0) THEN 'lost_ownership' ELSE 'published' END AS outcome FROM observation
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract(body,'$.token.claim_id'), 'publish_probe', body,
 CASE WHEN outcome IN ('published','already_published') THEN json_object('outcome',outcome,'job_id',id,'result_ref',output || '')
 ELSE json_object('outcome',outcome) END FROM verdict RETURNING result_json
"#;

const APPLY_SQL: &str = r#"
WITH input AS (SELECT json($1) AS body), admitted AS (
 SELECT body, job.payload_json, json_extract(job.result_ref,'$.probe') AS facts FROM input JOIN background_jobs job
 ON job.id = json_extract(body,'$.job_id') AND job.kind = 'media_probe' AND job.payload_version = 1 AND job.state = 'succeeded'
 WHERE json_valid(job.result_ref) AND json_extract(job.result_ref,'$.source') = job.payload_json
 AND EXISTS (SELECT 1 FROM job_leases lease
 WHERE lease.resource = json_extract(job.payload_json,'$.coordinator.resource')
 AND lease.owner_node_id = json_extract(job.payload_json,'$.coordinator.node_id')
 AND lease.fence = json_extract(job.payload_json,'$.coordinator.fence')
 AND lease.expires_at_ms > json_extract(body,'$.now_ms')
 AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || lease.owner_node_id)
 AND NOT EXISTS (SELECT 1 FROM background_job_domain_leases binding WHERE binding.resource = lease.resource
   AND (binding.domain_fence != lease.fence OR NOT EXISTS (SELECT 1 FROM background_jobs parent
    WHERE parent.id = binding.job_id AND parent.fence = binding.job_fence AND parent.state = 'running'
     AND parent.owner_node_id = binding.node_id AND parent.owner_boot_id = binding.boot_id AND parent.claim_id = binding.claim_id
     AND parent.lease_expires_ms > json_extract(body,'$.now_ms'))))) AND EXISTS (SELECT 1 FROM files file JOIN items item ON item.id = file.item_id
 WHERE file.id = json_extract(job.payload_json,'$.file_id')
 AND file.size = json_extract(job.payload_json,'$.source_size') AND file.mtime = json_extract(job.payload_json,'$.source_mtime')
 AND (json_extract(job.payload_json,'$.coordinator.resource') = 'repair:probe'
   OR json_extract(job.payload_json,'$.coordinator.resource') = 'scan:library:' || item.library_id))
 AND EXISTS (SELECT 1 FROM job_leases lease WHERE lease.resource = json_extract(job.payload_json,'$.coordinator.resource')
   AND lease.resource = json_extract(body,'$.lease.resource') AND lease.owner_node_id = json_extract(body,'$.lease.owner_node_id')
   AND lease.fence = json_extract(body,'$.lease.fence') AND lease.revision = json_extract(body,'$.lease.revision')
   AND lease.expires_at_ms = json_extract(body,'$.lease.expires_at_unix_ms'))
)
UPDATE files SET (duration_ms,container,video_codec,video_profile,width,height,bit_depth,hdr,bitrate,hdr_format,video_codec_tag,field_order,max_cll,max_fall,mastering_max_luminance,luminance_source,audio_streams,subtitle_streams,probe_json,scanned_at,dv_profile,dv_level,dv_bl_compat_id,dv_el_present,dv_rpu_present) =
 (SELECT json_extract(facts,'$.duration_ms'),json_extract(facts,'$.container'),json_extract(facts,'$.video_codec'),json_extract(facts,'$.video_profile'),json_extract(facts,'$.width'),json_extract(facts,'$.height'),json_extract(facts,'$.bit_depth'),json_extract(facts,'$.hdr'),json_extract(facts,'$.bitrate'),json_extract(facts,'$.hdr_format'),json_extract(facts,'$.video_codec_tag'),COALESCE(json_extract(facts,'$.field_order'),CASE WHEN json_extract(facts,'$.raw_json') IS NOT NULL THEN 'unknown' END),json_extract(facts,'$.max_cll'),json_extract(facts,'$.max_fall'),json_extract(facts,'$.mastering_max_luminance'),json_extract(facts,'$.luminance_source'),json_extract(facts,'$.audio_streams'),json_extract(facts,'$.subtitle_streams'),json_extract(facts,'$.raw_json'),json_extract(body,'$.now_ms') / 1000,json_extract(facts,'$.dolby_vision.profile'),json_extract(facts,'$.dolby_vision.level'),json_extract(facts,'$.dolby_vision.bl_compat_id'),json_extract(facts,'$.dolby_vision.el_present'),json_extract(facts,'$.dolby_vision.rpu_present') FROM admitted)
WHERE id = (SELECT json_extract(payload_json,'$.file_id') FROM admitted)
RETURNING 'true' AS result_json
"#;
