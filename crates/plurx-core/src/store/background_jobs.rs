//! Common durable-work identities and ownership contract.
//!
//! A request, a computation, an execution attempt and its immutable output
//! have different identities. In particular a retry must never manufacture
//! a new claim ID after a write acknowledgement is lost.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use super::background_jobs_delivery::{
    hydration_identity, DeliveryIntent, JobWaiter, WaiterCursor, WaiterPage, WaiterQuery,
};
use super::background_jobs_delivery::{DELIVERIES_SQL, WAITERS_SQL};
pub use super::background_jobs_domain::BindLibraryJob;
use super::background_jobs_fragment::PUBLISH_FRAGMENT_SQL;
pub use super::background_jobs_fragment::{FragmentJobFailure, PublishFragmentJob};
pub use super::background_jobs_fragment_admission::EnqueueFragmentJob;
pub use super::background_jobs_library::{
    CompleteLibraryWork, LibraryWorkQuery, LibraryWorkRecord, NewLibraryWork,
};
use super::background_jobs_maintenance::{CANCEL_WAITER_SQL, MAINTENANCE_NEEDED, MAINTENANCE_SQL};
pub use super::background_jobs_migration::JobMigrationStatus;
pub use super::background_jobs_observation::{
    BackgroundJobMetrics, JobAttemptObservation, JobCount, JobLabel, SourceIoHolder,
    JOB_METRIC_KINDS, JOB_METRIC_SLOTS, JOB_METRIC_STATES,
};
use super::background_jobs_observation::{
    ATTEMPTS_SQL, COUNTS_SQL, LABELS_SQL, SOURCE_IO_HOLDERS_SQL,
};
pub use super::background_jobs_offline::JoinOfflineJob;
use super::background_jobs_publication::PUBLISH_TRANSCODE_SQL;
pub use super::background_jobs_publication::{
    JobPublishOutcome, PublishTranscodeJob, TranscodeJobOutput,
};
use crate::error::StoreError;

/// First SQLite schema containing this queue; stable across later migrations.
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 71;

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

// Settled history is evicted under pressure rather than refusing new work;
// this migration replaces the enqueue and upkeep triggers. See the file's
// header for the failure it answers.
pub(crate) const RETENTION_SCHEMA: &str = include_str!("background_jobs_retention.sql");
pub(crate) const VIEWER_ANALYSIS_SCHEMA: &str = include_str!("background_jobs_viewer_analysis.sql");

// Both backends execute the same admission statement and schema trigger.
// The returned snapshot is the verdict that authorized the mutation, not a
// follow-up read which might observe a different concurrent request.
// These single-row stages must remain materialized: flattening the repeated
// verdict fields duplicates correlated subqueries into tens of thousands of
// planner operations, making migration and ordinary admission needlessly slow.
pub(super) const ENQUEUE_SQL: &str = r#"
WITH provided AS (SELECT json($1) AS body), input AS MATERIALIZED (
 SELECT CASE WHEN EXISTS (SELECT 1 FROM background_predictions WHERE state = 'pending'
   AND request_id = COALESCE(json_extract(body,'$.subtitle_request.request_id'),json_extract(body,'$.analysis_request.request_id')))
 THEN json_set(body, '$.request.deadline_ms', (SELECT expires_ms FROM background_predictions
   WHERE request_id = COALESCE(json_extract(body,'$.subtitle_request.request_id'),json_extract(body,'$.analysis_request.request_id'))),
   '$.request.retain_identity', json('false')) ELSE body END AS body FROM provided
), request AS MATERIALIZED (
 SELECT CASE WHEN json_extract(body, '$.artwork_demand') = 1 THEN json_set(body, '$.request.request_id',
   COALESCE((SELECT request_id FROM background_job_waiters
     WHERE request_scope = json_extract(body, '$.request.scope')
       AND consumer_ref = json_extract(body, '$.request.consumer_ref')
       AND (state IN ('pending','awaiting_hydration') OR updated_at_ms > json_extract(body, '$.now_ms') - 3600000)
     ORDER BY updated_at_ms DESC, request_id DESC LIMIT 1), json_extract(body, '$.request.request_id')))
 ELSE body END AS body FROM input
), snapshot AS MATERIALIZED (
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
      AND (state IN ('queued','running','cancelling') OR (json_type(body, '$.legacy_key') IS NOT NULL
        AND (id = json_extract(body, '$.id') OR id IN (SELECT mapped.job_id FROM background_job_legacy mapped
            WHERE mapped.state = 'materialized' AND mapped.kind = 'fragment_index_build'
              AND json_extract(mapped.snapshot_json, '$.cache_key') = json_extract(body, '$.payload.cache_key')))))
      ORDER BY CASE WHEN state IN ('queued','running','cancelling') THEN 0 ELSE 1 END, created_at_ms DESC, id LIMIT 1) AS active_job,
    (SELECT state FROM background_jobs WHERE dedupe_key = json_extract(body, '$.dedupe_key')
      AND (state IN ('queued','running','cancelling') OR (json_type(body, '$.legacy_key') IS NOT NULL
        AND (id = json_extract(body, '$.id') OR id IN (SELECT mapped.job_id FROM background_job_legacy mapped
            WHERE mapped.state = 'materialized' AND mapped.kind = 'fragment_index_build'
              AND json_extract(mapped.snapshot_json, '$.cache_key') = json_extract(body, '$.payload.cache_key')))))
      ORDER BY CASE WHEN state IN ('queued','running','cancelling') THEN 0 ELSE 1 END, created_at_ms DESC, id LIMIT 1) AS active_state,
    (SELECT payload_json FROM background_jobs WHERE dedupe_key = json_extract(body, '$.dedupe_key')
      AND (state IN ('queued','running','cancelling') OR (json_type(body, '$.legacy_key') IS NOT NULL
        AND (id = json_extract(body, '$.id') OR id IN (SELECT mapped.job_id FROM background_job_legacy mapped
            WHERE mapped.state = 'materialized' AND mapped.kind = 'fragment_index_build'
              AND json_extract(mapped.snapshot_json, '$.cache_key') = json_extract(body, '$.payload.cache_key')))))
      ORDER BY CASE WHEN state IN ('queued','running','cancelling') THEN 0 ELSE 1 END, created_at_ms DESC, id LIMIT 1) AS active_payload
  FROM request
), classified AS MATERIALIZED (
  SELECT *, CASE
    WHEN json_type(body, '$.legacy_key') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM background_job_legacy WHERE legacy_key = json_extract(body, '$.legacy_key')
        AND state = 'awaiting_import'
    ) THEN 'legacy_settled'
    WHEN json_type(body, '$.legacy_key') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM files WHERE id = json_extract(body, '$.payload.file_id')
        AND size = json_extract(body, '$.payload.source_size') AND mtime = json_extract(body, '$.payload.source_mtime')
    ) THEN 'source_changed'
    WHEN json_type(body, '$.producer_lease') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM job_leases WHERE resource = json_extract(body, '$.producer_lease.resource')
      AND owner_node_id = json_extract(body, '$.producer_lease.owner_node_id')
      AND fence = json_extract(body, '$.producer_lease.fence')
      AND revision = json_extract(body, '$.producer_lease.revision')
      AND expires_at_ms = json_extract(body, '$.producer_lease.expires_at_unix_ms')
      AND expires_at_ms > json_extract(body, '$.now_ms')
      AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || owner_node_id)
    ) THEN 'producer_fenced'
    WHEN json_extract(body, '$.payload.kind') = 'subtitle_extract' AND (
      json_type(body, '$.subtitle_request') IS NULL OR NOT EXISTS (
        SELECT 1 FROM analysis_requests request JOIN files file ON file.id = request.file_id
          AND file.size = request.source_size AND file.mtime = request.source_mtime
        WHERE request.request_id = json_extract(body, '$.subtitle_request.request_id')
          AND request.request_id = json_extract(body, '$.payload.source_generation')
          AND request.file_id = json_extract(body, '$.payload.file_id')
          AND request.pipeline_version = json_extract(body, '$.subtitle_request.pipeline_version')
          AND request.component = 'subtitle_source' AND request.cancel_requested = 0
          AND request.state IN ('queued','running')
          AND request.video_identity = '' AND request.target_node_id = ''
      )) THEN 'request_fenced'
    WHEN json_type(body,'$.repair_id') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM background_artifact_repairs repair WHERE repair.id = json_extract(body,'$.repair_id')
        AND repair.phase = json_extract(body,'$.repair_phase') AND repair.expires_ms > json_extract(body,'$.now_ms')
        AND json_extract(body,'$.request.scope') = 'artifact-repair'
        AND json_extract(body,'$.request.request_id') = repair.id || ':' || repair.phase
        AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || repair.target_node_id)
        AND ((repair.phase IN ('copy','deliver') AND json_extract(body,'$.payload.kind') = 'artifact_hydrate'
          AND json_extract(body,'$.payload.artifact_key') = repair.artifact_key
          AND json_extract(body,'$.payload.target_node_id') = repair.target_node_id)
          OR (repair.phase = 'build' AND json_extract(body,'$.payload') = repair.producer_payload
            AND (json_extract(body,'$.payload.kind') = 'artwork_derivative'
              OR (json_extract(body,'$.payload.kind') = 'transcode_prepare' AND EXISTS (SELECT 1 FROM files
                WHERE id = json_extract(body,'$.payload.file_id') AND size = json_extract(body,'$.payload.source_size')
                  AND mtime = json_extract(body,'$.payload.source_mtime'))))))) THEN 'no_demand'
    WHEN json_type(body,'$.repair_id') IS NOT NULL AND prior_job IS NULL AND active_job IS NULL
      AND (SELECT COUNT(*) FROM background_job_waiters WHERE request_scope = 'artifact-repair'
        AND state IN ('pending','awaiting_hydration')) >= 64 THEN 'queue_full'
    WHEN prior_job IS NOT NULL AND prior_digest != json_extract(body, '$.request.request_digest') THEN 'conflict'
    WHEN prior_job IS NOT NULL THEN 'existing'
    WHEN json_extract(body,'$.payload.kind') = 'media_probe' AND NOT EXISTS (SELECT 1 FROM job_leases lease
 WHERE lease.resource = json_extract(json_extract(body,'$.payload'),'$.coordinator.resource')
 AND lease.owner_node_id = json_extract(json_extract(body,'$.payload'),'$.coordinator.node_id')
 AND lease.fence = json_extract(json_extract(body,'$.payload'),'$.coordinator.fence')
 AND lease.expires_at_ms > json_extract(body,'$.now_ms')
 AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || lease.owner_node_id)
 AND NOT EXISTS (SELECT 1 FROM background_job_domain_leases binding WHERE binding.resource = lease.resource
   AND (binding.domain_fence != lease.fence OR NOT EXISTS (SELECT 1 FROM background_jobs parent
    WHERE parent.id = binding.job_id AND parent.fence = binding.job_fence AND parent.state = 'running'
     AND parent.owner_node_id = binding.node_id AND parent.owner_boot_id = binding.boot_id AND parent.claim_id = binding.claim_id
     AND parent.lease_expires_ms > json_extract(body,'$.now_ms'))))) THEN 'producer_fenced'
    WHEN json_extract(body,'$.payload.kind') = 'media_probe' AND NOT EXISTS (SELECT 1 FROM files file JOIN items item ON item.id = file.item_id
 WHERE file.id = json_extract(json_extract(body,'$.payload'),'$.file_id')
 AND file.size = json_extract(json_extract(body,'$.payload'),'$.source_size') AND file.mtime = json_extract(json_extract(body,'$.payload'),'$.source_mtime')
 AND (json_extract(json_extract(body,'$.payload'),'$.coordinator.resource') = 'repair:probe'
   OR json_extract(json_extract(body,'$.payload'),'$.coordinator.resource') = 'scan:library:' || item.library_id)) THEN 'source_changed'
    WHEN json_extract(body,'$.payload.kind') = 'media_probe' AND active_job IS NULL
      AND (SELECT COUNT(*) FROM background_jobs WHERE kind = 'media_probe' AND state IN ('queued','running','cancelling')
        AND json_extract(payload_json,'$.coordinator') = json_extract(body,'$.payload.coordinator')) >= 128 THEN 'queue_full'
    WHEN json_extract(body,'$.payload.kind') = 'semantic_embedding' AND NOT EXISTS (
      SELECT 1 FROM items WHERE id = json_extract(body,'$.payload.item_id')) THEN 'source_changed'
    WHEN json_extract(body,'$.payload.kind') = 'semantic_embedding' AND EXISTS (
      SELECT 1 FROM background_embeddings WHERE item_id = json_extract(body,'$.payload.item_id')
        AND content_digest = json_extract(body,'$.payload.content_digest')
        AND model_digest = json_extract(body,'$.payload.model_digest')) THEN 'no_demand'
    WHEN json_extract(body,'$.payload.kind') = 'semantic_embedding' AND active_job IS NULL
      AND (SELECT COUNT(*) FROM background_jobs WHERE kind = 'semantic_embedding'
        AND state IN ('queued','running','cancelling')) >= 64 THEN 'queue_full'
    WHEN json_type(body, '$.library_request') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM libraries WHERE id = json_extract(body, '$.payload.library_id')) THEN 'source_changed'
    WHEN json_type(body, '$.library_request') IS NOT NULL AND (SELECT COUNT(*) FROM background_library_requests request
      JOIN background_job_waiters waiter ON waiter.request_scope = 'library' AND waiter.request_id = request.request_id
      WHERE request.library_id = json_extract(body, '$.payload.library_id') AND waiter.state = 'pending') >= 256 THEN 'queue_full'
    WHEN json_type(body, '$.offline_join') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM offline_packages package JOIN background_jobs original
        ON original.id = json_extract(body, '$.offline_join.job_id')
      JOIN files file ON file.id = package.file_id AND file.size = package.source_size AND file.mtime = package.source_mtime
      WHERE package.id = json_extract(body, '$.offline_join.package_id') AND package.state = 'preparing'
        AND package.node_id = json_extract(body, '$.offline_join.node_id')
        AND package.claim_generation = json_extract(body, '$.offline_join.claim_generation')
        AND package.recipe_hash = json_extract(body, '$.offline_join.recipe_hash')
        AND package.decoder_recovery_state = 'primary' AND package.expires_at > json_extract(body, '$.now_ms') / 1000
        AND original.state = 'running' AND original.owner_node_id = package.node_id
        AND original.id = active_job AND original.lease_expires_ms > json_extract(body, '$.now_ms')
        AND json_extract(original.checkpoint_json, '$.recipe_node_id') = package.node_id
        AND json_extract(original.checkpoint_json, '$.effective_recipe_hash') = package.recipe_hash
        AND json_extract(original.payload_json, '$.file_id') = package.file_id
        AND json_extract(original.payload_json, '$.source_size') = package.source_size
        AND json_extract(original.payload_json, '$.source_mtime') = package.source_mtime
        AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || original.owner_node_id)
    ) THEN 'no_demand'
    WHEN json_type(body, '$.cache_repair_of') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM background_jobs original WHERE original.id = json_extract(body, '$.cache_repair_of')
        AND original.kind = 'transcode_prepare' AND original.state = 'succeeded' AND original.payload_version = 1
        AND original.dedupe_key = json_extract(body, '$.dedupe_key')
        AND original.id = (SELECT latest.id FROM background_jobs latest
            WHERE latest.dedupe_key = original.dedupe_key ORDER BY latest.created_at_ms DESC, latest.id DESC LIMIT 1)
        AND json_valid(original.result_ref) AND NOT EXISTS (
            SELECT 1 FROM transcode_cache_locations location
            WHERE location.recipe_hash = CASE WHEN json_valid(original.result_ref) THEN json_extract(original.result_ref, '$.recipe_hash') END
                AND location.complete = 1)
    ) THEN 'no_demand'
    WHEN json_type(body, '$.cache_repair_of') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM files WHERE id = json_extract(body, '$.payload.file_id')
        AND size = json_extract(body, '$.payload.source_size') AND mtime = json_extract(body, '$.payload.source_mtime')
    ) THEN 'source_changed'
    WHEN json_type(body, '$.retry_of') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM background_jobs original WHERE original.id = json_extract(body, '$.retry_of')
        AND original.state IN ('failed','cancelled') AND original.payload_version = 1
        AND original.kind IN ('transcode_prepare','artifact_hydrate')
        AND original.dedupe_key = json_extract(body, '$.dedupe_key')
    ) THEN 'conflict'
    WHEN json_type(body, '$.retry_of') IS NOT NULL
      AND json_extract(body, '$.payload.kind') = 'transcode_prepare' AND NOT EXISTS (
        SELECT 1 FROM files WHERE id = json_extract(body, '$.payload.file_id')
          AND size = json_extract(body, '$.payload.source_size')
          AND mtime = json_extract(body, '$.payload.source_mtime')
    ) THEN 'source_changed'
    WHEN json_type(body, '$.retry_of') IS NOT NULL
      AND json_extract(body, '$.payload.kind') = 'artifact_hydrate' AND NOT EXISTS (
        SELECT 1 FROM cluster_fragment_index_artifacts artifact JOIN files file ON file.id = artifact.file_id
          AND file.size = artifact.source_size AND file.mtime = artifact.source_mtime
        WHERE 'fragment:' || artifact.cache_key = json_extract(body, '$.payload.artifact_key')
      UNION ALL SELECT 1 FROM background_artwork_locations artifact
        WHERE 'artwork:' || artifact.artifact_key = json_extract(body, '$.payload.artifact_key')
      UNION ALL SELECT 1 FROM background_transcode_artifacts artifact JOIN files file ON file.id = artifact.file_id
          AND file.size = artifact.source_size AND file.mtime = artifact.source_mtime
        WHERE 'transcode:' || artifact.recipe_hash || ':' || artifact.manifest_digest = json_extract(body, '$.payload.artifact_key')
    ) THEN 'source_changed'
    WHEN json_type(body, '$.delivery_parent') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM background_job_waiters WHERE job_id = json_extract(body, '$.delivery_parent')
        AND target_node_id = json_extract(body, '$.payload.target_node_id') AND state = 'awaiting_hydration'
        AND (deadline_ms IS NULL OR deadline_ms > json_extract(body, '$.now_ms'))
        AND CASE WHEN json_valid(result_ref) THEN COALESCE(CASE json_extract(result_ref, '$.artifact_kind') WHEN 'fragment_index' THEN 'fragment:' WHEN 'artwork' THEN 'artwork:' END || json_extract(result_ref, '$.artifact_key'), 'transcode:' || json_extract(result_ref,'$.recipe_hash') || ':' || json_extract(result_ref,'$.manifest_digest')) END = json_extract(body, '$.payload.artifact_key')
    ) THEN 'no_demand'
    WHEN json_type(body, '$.fragment_domain') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM files WHERE id = json_extract(body, '$.fragment_domain.file_id')
        AND size = json_extract(body, '$.fragment_domain.source_size') AND mtime = json_extract(body, '$.fragment_domain.source_mtime')
    ) THEN 'source_changed'
    WHEN json_extract(body, '$.fragment_repair') = 1 AND NOT EXISTS (
      SELECT 1 FROM cluster_fragment_index_artifacts WHERE cache_key = json_extract(body, '$.fragment_domain.cache_key')
        AND file_id = json_extract(body, '$.fragment_domain.file_id')
        AND source_size = json_extract(body, '$.fragment_domain.source_size')
        AND source_mtime = json_extract(body, '$.fragment_domain.source_mtime')
        AND source_sha256 = json_extract(body, '$.fragment_domain.source_sha256')
        AND pipeline_sha256 = json_extract(body, '$.fragment_domain.pipeline_sha256')
    ) THEN 'conflict'
    WHEN json_type(body, '$.analysis_request') IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM analysis_requests WHERE request_id = json_extract(body, '$.analysis_request.request_id')
        AND state = 'running' AND component = 'fragment_index' AND cancel_requested = 0
        AND owner_node_id = json_extract(body, '$.analysis_request.owner_node_id')
        AND fence = json_extract(body, '$.analysis_request.fence') AND lease_expires_ms > json_extract(body, '$.now_ms')
        AND file_id = json_extract(body, '$.fragment_domain.file_id')
        AND source_size = json_extract(body, '$.fragment_domain.source_size')
        AND source_mtime = json_extract(body, '$.fragment_domain.source_mtime')
        AND target_node_id = json_extract(body, '$.fragment_domain.target_node_id')
        AND force_rebuild = json_extract(body, '$.analysis_request.force_rebuild')
        AND requested_generation = json_extract(body, '$.analysis_request.requested_generation')
        AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || owner_node_id)
    ) THEN 'request_fenced'
    WHEN json_type(body, '$.fragment_domain') IS NOT NULL
      AND COALESCE(json_extract(body, '$.analysis_request.force_rebuild'), 0) = 0
      AND json_extract(body, '$.fragment_domain.cache_key') != COALESCE((
        SELECT generation_cache_key FROM cluster_fragment_index_heads WHERE logical_cache_key = json_extract(body, '$.logical_key')
      ), json_extract(body, '$.logical_key')) THEN 'conflict'
    WHEN json_type(body, '$.fragment_domain') IS NOT NULL
      AND COALESCE(json_extract(body, '$.analysis_request.force_rebuild'), 0) = 0
      AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
        WHERE cache_key = json_extract(body, '$.fragment_domain.cache_key')
          AND target_node_id = json_extract(body, '$.fragment_domain.target_node_id')
          AND state IN ('failed','cancelled')
          AND (COALESCE(last_error_code, '') != 'queue_expired' OR index_retry_deadline_ms != 0)) THEN 'domain_terminal'
    WHEN active_state = 'cancelling' THEN 'job_cancelling'
    WHEN active_job IS NOT NULL AND json_remove(active_payload, '$.reason', '$.spec.source_name') != json_remove(json_extract(body, '$.payload'), '$.reason', '$.spec.source_name') THEN 'conflict'
    WHEN json_type(body, '$.legacy_key') IS NULL AND EXISTS (SELECT 1 FROM background_jobs WHERE id = json_extract(body, '$.id')) THEN 'conflict'
    WHEN json_type(body, '$.legacy_key') IS NULL AND json_extract(body, '$.priority') < 2
      AND EXISTS (SELECT 1 FROM background_job_legacy WHERE state = 'awaiting_import') THEN 'queue_full'
    WHEN json_extract(body, '$.request.scope') IN ('automatic:transcode','automatic:transcode-repair','automatic:hot-copy')
      AND (SELECT COUNT(*) FROM background_job_waiters WHERE request_scope IN ('automatic:transcode','automatic:transcode-repair','automatic:hot-copy')
        AND state IN ('pending','awaiting_hydration') AND (deadline_ms IS NULL OR deadline_ms > json_extract(body, '$.now_ms')))
        + (SELECT COUNT(*) FROM background_predictions WHERE state = 'pending' AND expires_ms > json_extract(body,'$.now_ms')) >= 64 THEN 'queue_full'
    WHEN (SELECT COUNT(*) FROM background_job_waiters) >= 16384 THEN 'queue_full'
    WHEN json_extract(body, '$.request.scope') LIKE 'user:%' AND (SELECT COUNT(*) FROM background_job_waiters
      WHERE request_scope = json_extract(body, '$.request.scope')
        AND state IN ('pending','awaiting_hydration')) >= 128 THEN 'queue_full'
    WHEN active_job IS NULL AND (SELECT COUNT(*) FROM background_jobs) >= 10000
      AND NOT EXISTS (SELECT 1 FROM background_jobs job WHERE job.state IN ('succeeded','failed','cancelled')
        AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
          AND remaining.kind = 'fragment_index_build'
          AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
        AND NOT EXISTS (SELECT 1 FROM background_job_waiters
          WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))) THEN 'queue_full'
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
    'attempt_limit', attempt_limit, 'retry_deadline_ms', retry_deadline_ms,
    'attempt_errors', attempt_errors, 'index_diagnostic_json', index_diagnostic_json,
    'yield_count', yield_count, 'abandoned_count', abandoned_count,
    'not_before_ms', not_before_ms, 'checkpoint', json(checkpoint_json),
    'result_ref', result_ref, 'last_error_code', last_error_code,
    'created_at_ms', created_at_ms, 'updated_at_ms', updated_at_ms
)"#;

pub(super) const CLAIM_SQL: &str = r#"
UPDATE background_jobs SET
  failed_attempts = failed_attempts + CASE WHEN state = 'running' THEN 1 ELSE 0 END,
  attempt_errors = CASE WHEN state = 'running' THEN
    CASE WHEN length(attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || 'lease_expired') <= 2048 THEN attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || 'lease_expired' ELSE 'history_compacted,' || substr(substr(attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || 'lease_expired', -1800), instr(substr(attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || 'lease_expired', -1800), ',') + 1) END ELSE attempt_errors END,
  state = 'running', owner_node_id = json_extract($1, '$.node_id'),
  owner_boot_id = json_extract($1, '$.boot_id'), claim_id = json_extract($1, '$.claim_id'),
  fence = fence + 1, revision = revision + 1,
  lease_expires_ms = json_extract($1, '$.now_ms') + 30000,
  updated_at_ms = json_extract($1, '$.now_ms')
WHERE id = json_extract($1, '$.job_id')
  AND revision = json_extract($1, '$.expected_revision')
  AND kind = json_extract($1, '$.kind') AND payload_version = json_extract($1, '$.payload_version')
  -- Match the subtitle projection's preconditions before its trigger runs. A
  -- settled request is an ordinary lost claim, never an ambiguous SQL failure.
  AND (kind != 'subtitle_extract' OR EXISTS (SELECT 1 FROM analysis_requests request JOIN files file ON file.id = request.file_id
        AND file.size = request.source_size AND file.mtime = request.source_mtime
      WHERE request.request_id = json_extract(background_jobs.payload_json, '$.source_generation')
        AND request.file_id = json_extract(background_jobs.payload_json, '$.file_id')
        AND request.component = 'subtitle_source' AND request.state IN ('queued','running')
        AND request.cancel_requested = 0))
  AND (kind != 'media_probe' OR (EXISTS (SELECT 1 FROM job_leases lease
 WHERE lease.resource = json_extract(background_jobs.payload_json,'$.coordinator.resource')
 AND lease.owner_node_id = json_extract(background_jobs.payload_json,'$.coordinator.node_id')
 AND lease.fence = json_extract(background_jobs.payload_json,'$.coordinator.fence')
 AND lease.expires_at_ms > json_extract($1,'$.now_ms')
 AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || lease.owner_node_id)
 AND NOT EXISTS (SELECT 1 FROM background_job_domain_leases binding WHERE binding.resource = lease.resource
   AND (binding.domain_fence != lease.fence OR NOT EXISTS (SELECT 1 FROM background_jobs parent
    WHERE parent.id = binding.job_id AND parent.fence = binding.job_fence AND parent.state = 'running'
     AND parent.owner_node_id = binding.node_id AND parent.owner_boot_id = binding.boot_id AND parent.claim_id = binding.claim_id
     AND parent.lease_expires_ms > json_extract($1,'$.now_ms')))))
  AND EXISTS (SELECT 1 FROM files file JOIN items item ON item.id = file.item_id
 WHERE file.id = json_extract(background_jobs.payload_json,'$.file_id')
 AND file.size = json_extract(background_jobs.payload_json,'$.source_size') AND file.mtime = json_extract(background_jobs.payload_json,'$.source_mtime')
 AND (json_extract(background_jobs.payload_json,'$.coordinator.resource') = 'repair:probe'
   OR json_extract(background_jobs.payload_json,'$.coordinator.resource') = 'scan:library:' || item.library_id))))
  AND (target_node_id IS NULL OR target_node_id = json_extract($1, '$.node_id'))
  AND (state = 'queued' OR (state = 'running' AND lease_expires_ms <= json_extract($1, '$.now_ms')))
  AND (retry_deadline_ms = 0 OR retry_deadline_ms > json_extract($1, '$.now_ms'))
  AND (kind = 'fragment_index_build' OR failed_attempts + CASE WHEN state = 'running' THEN 1 ELSE 0 END < attempt_limit)
  AND not_before_ms <= json_extract($1, '$.now_ms')
  AND EXISTS (SELECT 1 FROM background_job_waiters WHERE job_id = background_jobs.id
    AND state = 'pending' AND (deadline_ms IS NULL OR deadline_ms > json_extract($1, '$.now_ms'))
    AND (background_jobs.kind != 'fragment_index_build' OR (
      not_before_ms <= json_extract($1, '$.now_ms')
      AND (retry_deadline_ms = 0 OR retry_deadline_ms > json_extract($1, '$.now_ms'))
      AND failed_attempts + CASE WHEN background_jobs.state = 'running'
        AND participation_fence = background_jobs.fence THEN 1 ELSE 0 END < attempt_limit)))
  AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || json_extract($1, '$.node_id'))
  AND fence < 9223372036854775807 AND revision < 9223372036854775807
  AND NOT EXISTS (SELECT 1 FROM background_job_attempts WHERE claim_id = json_extract($1, '$.claim_id'))
  AND (SELECT COUNT(*) FROM background_job_attempts) < 40000
  AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
    WHERE required.job_id = background_jobs.id AND ((SELECT COUNT(*) FROM background_job_reservations held
      WHERE held.resource_key = required.resource_key AND held.expires_at_ms > json_extract($1, '$.now_ms')
        AND held.job_id != json_extract($1, '$.job_id'))
      + (SELECT COUNT(*) FROM analysis_source_reservations held
          WHERE held.resource_key = required.resource_key
            AND held.expires_at_ms > json_extract($1, '$.now_ms')))
      >= 2)
  -- A second source reader is reserved for live demand. Classification uses
  -- durable consumer ownership, never the job's caller-supplied priority.
  AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
    WHERE required.job_id = background_jobs.id
      AND required.resource_key LIKE 'source_io%'
      AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
          WHERE interest.job_id = background_jobs.id
            AND interest.state IN ('pending','awaiting_hydration')
            AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract($1, '$.now_ms'))
            AND (interest.consumer_kind = 'playback_fragment'
              OR (interest.consumer_kind = 'subtitle_source' AND EXISTS (
                  SELECT 1 FROM analysis_requests request
                  WHERE request.request_id = interest.consumer_ref
                    AND request.component = 'subtitle_source'
                    AND request.trigger = 'playback'
                    AND request.state IN ('queued','running')))))
      AND ((SELECT COUNT(*) FROM background_job_reservations held
          WHERE held.resource_key = required.resource_key
            AND held.expires_at_ms > json_extract($1, '$.now_ms')
            AND held.job_id != background_jobs.id
            AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
                WHERE interest.job_id = held.job_id
                  AND interest.state IN ('pending','awaiting_hydration')
                  AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract($1, '$.now_ms'))
                  AND (interest.consumer_kind = 'playback_fragment'
                    OR (interest.consumer_kind = 'subtitle_source' AND EXISTS (
                        SELECT 1 FROM analysis_requests request
                        WHERE request.request_id = interest.consumer_ref
                          AND request.component = 'subtitle_source'
                          AND request.trigger = 'playback'
                          AND request.state IN ('queued','running')))))
        + (SELECT COUNT(*) FROM analysis_source_reservations held
            WHERE held.resource_key = required.resource_key
              AND held.expires_at_ms > json_extract($1, '$.now_ms')
              AND NOT EXISTS (SELECT 1 FROM background_job_waiters viewer
                  WHERE viewer.request_scope = 'playback-analysis'
                    AND viewer.job_id = held.request_id
                    AND viewer.state = 'pending'
                    AND viewer.deadline_ms > json_extract($1, '$.now_ms')))) >= 1)
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
            AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
                WHERE required.job_id = job.id AND NOT EXISTS (
                    SELECT 1 FROM background_job_reservations held
                    WHERE held.job_id = job.id AND held.fence = job.fence
                        AND held.resource_key = required.resource_key
                        AND held.expires_at_ms >= job.lease_expires_ms))
    AND (job.retry_deadline_ms = 0 OR job.retry_deadline_ms > now_ms)
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
    WHEN 'retry' THEN CASE WHEN kind = 'fragment_index_build' THEN 'queued' WHEN failed_attempts + 1 >= attempt_limit THEN 'failed' ELSE 'queued' END
    WHEN 'fail' THEN CASE WHEN kind = 'fragment_index_build' THEN 'queued' ELSE 'failed' END
    WHEN 'stop' THEN 'cancelled' WHEN 'cancel' THEN 'cancelled' END,
  failed_attempts = failed_attempts + CASE WHEN json_extract($1, '$.settlement.disposition') IN ('retry','fail') THEN 1 ELSE 0 END,
  failure_policy = COALESCE(json_extract($1, '$.failure_policy'), CASE json_extract($1, '$.settlement.disposition')
    WHEN 'retry' THEN 'retry' WHEN 'fail' THEN 'terminal' END),
  not_before_ms = COALESCE(json_extract($1, '$.settlement.not_before_ms'), not_before_ms),
  checkpoint_json = CASE WHEN json_extract($1, '$.settlement.disposition') != 'yield' THEN checkpoint_json
    WHEN json_extract(checkpoint_json, '$.effective_recipe_hash') IS NOT NULL THEN json_set(
      CASE WHEN json_type($1, '$.settlement.checkpoint') = 'object' THEN json_extract($1, '$.settlement.checkpoint')
        ELSE checkpoint_json END,
      '$.effective_recipe_hash', json_extract(checkpoint_json, '$.effective_recipe_hash'),
      '$.recipe_node_id', json_extract(checkpoint_json, '$.recipe_node_id'))
    ELSE COALESCE(json_extract($1, '$.settlement.checkpoint'), checkpoint_json) END,
  retry_deadline_ms = COALESCE(json_extract($1, '$.retry_deadline_ms'), retry_deadline_ms),
  index_diagnostic_json = COALESCE(json_extract($1, '$.index_diagnostic_json'), index_diagnostic_json),
  attempt_errors = CASE WHEN json_extract($1, '$.settlement.disposition') IN ('retry','fail') THEN
    CASE WHEN length(attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || COALESCE(json_extract($1, '$.attempt_error'), json_extract($1, '$.settlement.error_code'))) <= 2048 THEN attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || COALESCE(json_extract($1, '$.attempt_error'), json_extract($1, '$.settlement.error_code')) ELSE 'history_compacted,' || substr(substr(attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || COALESCE(json_extract($1, '$.attempt_error'), json_extract($1, '$.settlement.error_code')), -1800), instr(substr(attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || COALESCE(json_extract($1, '$.attempt_error'), json_extract($1, '$.settlement.error_code')), -1800), ',') + 1) END ELSE attempt_errors END,
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

impl JobKind {
    /// Learners may compute immutable results, but never own catalogue scans
    /// or provider passes. Every publication still uses its typed transaction.
    pub const fn permits_artifact_execution(self) -> bool {
        match self {
            Self::TranscodePrepare
            | Self::FragmentIndexBuild
            | Self::ArtifactHydrate
            | Self::SubtitleExtract
            | Self::ArtifactVerify
            | Self::ArtworkDerivative
            | Self::SemanticEmbedding
            | Self::MediaProbe => true,
            Self::LibraryScan | Self::MetadataRefresh => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
/// Closed payloads contain identifiers, never executable text or caller-selected paths.
pub enum JobPayload {
    TranscodePrepare {
        file_id: i64,
        source_generation: String,
        source_size: i64,
        source_mtime: i64,
        recipe_key: String,
        target_height: u32,
        policy_generation: String,
        requirements: crate::domain::PretranscodeRequirements,
        reason: String,
    },
    FragmentIndexBuild {
        file_id: i64,
        source_generation: String,
        source_size: i64,
        source_mtime: i64,
        source_sha256: String,
        cache_key: String,
        pipeline_digest: String,
    },
    ArtifactHydrate {
        artifact_key: String,
        target_node_id: String,
    },
    SubtitleExtract {
        file_id: i64,
        source_generation: String,
        /// None prepares every eligible track in the existing one-pass extractor.
        track: Option<u32>,
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
        spec: super::background_jobs_artwork::ArtworkVariantSpec,
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
        source_size: i64,
        source_mtime: i64,
        coordinator: super::background_jobs_probe::ProbeCoordinator,
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
                source_size,
                recipe_key,
                target_height,
                policy_generation,
                requirements,
                reason,
                ..
            } => {
                *file_id > 0
                    && identifier(source_generation)
                    && *source_size >= 0
                    && identifier(recipe_key)
                    && (1..=8_640).contains(target_height)
                    && identifier(policy_generation)
                    && requirements.validate()
                    && matches!(
                        reason.as_str(),
                        "in_progress" | "next_up" | "recent" | "recent_demand" | "channel_next"
                    )
            }
            Self::FragmentIndexBuild {
                file_id,
                source_generation,
                source_size,
                source_sha256,
                cache_key,
                pipeline_digest,
                ..
            } => {
                *file_id > 0
                    && identifier(source_generation)
                    && *source_size >= 0
                    && digest(source_sha256)
                    && digest(cache_key)
                    && digest(pipeline_digest)
            }
            Self::SubtitleExtract {
                file_id,
                source_generation,
                pipeline_digest,
                ..
            } => *file_id > 0 && identifier(source_generation) && digest(pipeline_digest),
            Self::MediaProbe {
                file_id,
                source_generation,
                probe_digest,
                source_size,
                source_mtime,
                coordinator,
            } => {
                *file_id > 0
                    && *source_size >= 0
                    && digest(probe_digest)
                    && coordinator.validate()
                    && *source_generation
                        == super::background_jobs_probe::generation(
                            *file_id,
                            *source_size,
                            *source_mtime,
                            coordinator,
                            probe_digest,
                        )
            }
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
            Self::ArtworkDerivative { artifact_key, spec } => {
                spec.validate().is_ok() && *artifact_key == spec.artifact_key()
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

pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

pub(super) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    /// Preserve future payloads for listing and cancellation. Only
    /// `supported_payload` grants typed execution access to this binary.
    pub payload: serde_json::Value,
    pub dedupe_key: String,
    pub priority: u8,
    pub state: JobState,
    pub token: Option<JobToken>,
    pub fence: i64,
    pub revision: i64,
    pub failed_attempts: i64,
    pub attempt_limit: i64,
    pub retry_deadline_ms: i64,
    pub attempt_errors: String,
    pub index_diagnostic_json: String,
    /// Compacted resolved attempt counts; the latest detailed attempts remain.
    pub yield_count: i64,
    pub abandoned_count: i64,
    pub not_before_ms: i64,
    pub checkpoint: Option<serde_json::Value>,
    pub result_ref: Option<String>,
    pub last_error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl BackgroundJob {
    pub fn supported_payload(&self) -> Result<JobPayload, StoreError> {
        if self.payload_version != 1 {
            return Err(invalid("unsupported background payload version"));
        }
        let payload: JobPayload = serde_json::from_value(self.payload.clone())
            .map_err(|error| invalid(&format!("unsupported background payload: {error}")))?;
        payload.validate()?;
        Ok(payload)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub scope: String,
    pub request_id: String,
    pub request_digest: String,
    pub consumer_kind: String,
    pub consumer_ref: String,
    /// Delivery obligation, independent of the node that builds an artifact.
    pub target_node_id: Option<String>,
    pub deadline_ms: Option<i64>,
    /// Domain histories retain this compact identity until they release it.
    #[serde(default)]
    pub retain_identity: bool,
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
            || self
                .request
                .target_node_id
                .as_deref()
                .is_some_and(|node| !identifier(node))
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
    ProducerFenced,
    NoDemand,
    SourceChanged,
    RequestFenced,
    DomainTerminal,
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
pub struct CandidateCursor {
    /// Keep ordering inputs fixed across this bounded keyset walk.
    pub selected_at_ms: i64,
    pub fairness_floor: u8,
    pub scope_last_claim_ms: i64,
    pub priority: u8,
    pub created_at_ms: i64,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateQuery {
    pub node_id: String,
    pub kinds: Vec<JobKind>,
    pub after: Option<CandidateCursor>,
    pub now_ms: i64,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidatePage {
    pub jobs: Vec<BackgroundJob>,
    pub next: Option<CandidateCursor>,
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
    Stop {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelWaiter {
    pub scope: String,
    pub request_id: String,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelWaiterOutcome {
    pub job_id: String,
    pub cancelled: bool,
}

/// A server-owned playback identity joined to one exact fragment-analysis
/// generation. A retry of the same playback key renews the same waiter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisViewerInterest {
    pub analysis_request_id: String,
    pub requested_generation: String,
    pub pipeline_version: String,
    pub video_identity: String,
    pub target_node_id: String,
    pub user_id: i64,
    pub playback_id: String,
    pub now_ms: i64,
}

impl AnalysisViewerInterest {
    pub fn consumer_id(&self) -> String {
        crate::segplan::argv_fingerprint(&[
            "playback-analysis-v1".to_owned(),
            self.user_id.to_string(),
            self.playback_id.clone(),
            self.analysis_request_id.clone(),
        ])
    }
}

const JOIN_ANALYSIS_VIEWER_SQL: &str = r#"
INSERT INTO background_job_commands (id, operation, request_json, result_json)
SELECT json_extract($1, '$.command_id'), 'join_analysis_viewer', $1,
  json_object('joined', json('true'))
WHERE EXISTS (SELECT 1 FROM analysis_requests request JOIN files file
    ON file.id = request.file_id AND file.size = request.source_size
    AND file.mtime = request.source_mtime
  WHERE request.request_id = json_extract($1, '$.analysis_request_id')
    AND request.component = 'fragment_index'
    AND request.pipeline_version = json_extract($1, '$.pipeline_version')
    AND request.video_identity = json_extract($1, '$.video_identity')
    AND request.requested_generation = json_extract($1, '$.requested_generation')
    AND request.target_node_id = json_extract($1, '$.target_node_id')
    AND request.state IN ('queued','running','submitted')
    AND (request.state != 'submitted' OR EXISTS (
        SELECT 1 FROM background_job_waiters
        WHERE request_scope = 'analysis' AND request_id = request.request_id)))
RETURNING result_json
"#;

const REFRESH_ANALYSIS_VIEWERS_NEEDED: &str = r#"
SELECT json_object('needed', 1) AS result_json
WHERE EXISTS (SELECT 1 FROM background_job_waiters waiter
    JOIN media_sessions session ON session.user_id = json_extract(waiter.consumer_ref, '$.user_id')
      AND session.playback_id = json_extract(waiter.consumer_ref, '$.playback_id')
      AND json_extract(session.recipe_json, '$.request.file_id') = json_extract(waiter.consumer_ref, '$.file_id')
    JOIN media_playback_pointers pointer ON pointer.user_id = session.user_id
      AND pointer.playback_id = session.playback_id
      AND pointer.current_incarnation_id = session.incarnation_id
    WHERE waiter.request_scope = 'playback-analysis'
      AND waiter.state IN ('pending','awaiting_hydration')
      AND waiter.updated_at_ms <= json_extract($1, '$.now_ms') - 30000
      AND waiter.deadline_ms > json_extract($1, '$.now_ms')
      AND session.state = 'active' AND session.lease_expires_at_ms > json_extract($1, '$.now_ms'))
"#;

const REFRESH_ANALYSIS_VIEWERS_SQL: &str = r#"
INSERT INTO background_job_commands (id, operation, request_json, result_json)
VALUES (json_extract($1, '$.command_id'), 'refresh_analysis_viewers', $1, '{}')
RETURNING result_json
"#;

/// No generic public enqueue endpoint is implied by this internal boundary.
/// Domain producers authorize the request before attaching a waiter.
#[async_trait]
pub trait BackgroundJobStore: Send + Sync {
    /// Join or renew a viewer's bounded interest in an exact preparation.
    async fn join_analysis_viewer(
        &self,
        interest: AnalysisViewerInterest,
    ) -> Result<bool, StoreError>;
    async fn embedding_for(
        &self,
        item_id: i64,
        content_digest: &str,
        model_digest: &str,
    ) -> Result<super::background_jobs_embeddings::EmbeddingLookup, StoreError>;
    async fn publish_embedding_job(
        &self,
        request: super::background_jobs_embeddings::PublishEmbeddingJob,
    ) -> Result<JobPublishOutcome, StoreError>;
    async fn publish_probe_job(
        &self,
        request: super::background_jobs_probe::PublishProbeJob,
    ) -> Result<JobPublishOutcome, StoreError>;
    async fn apply_probe_job(
        &self,
        request: super::background_jobs_probe::ApplyProbeJob,
    ) -> Result<bool, StoreError>;
    async fn transcode_verification_candidates(
        &self,
        node_id: &str,
        artifact_key: Option<&str>,
    ) -> Result<Vec<super::background_jobs_integrity::TranscodeVerificationCandidate>, StoreError>;
    async fn artwork_verification_candidates(
        &self,
        node_id: &str,
    ) -> Result<Vec<super::background_jobs_artwork::ArtworkLocation>, StoreError>;
    async fn verify_artwork_job(
        &self,
        request: super::background_jobs_integrity::VerifyArtwork,
    ) -> Result<JobPublishOutcome, StoreError>;
    async fn verify_transcode_job(
        &self,
        request: super::background_jobs_integrity::VerifyTranscode,
    ) -> Result<JobPublishOutcome, StoreError>;
    async fn artifact_repairs(
        &self,
        pending_only: bool,
    ) -> Result<Vec<super::background_jobs_integrity::ArtifactRepair>, StoreError>;
    async fn enqueue_artifact_repair(
        &self,
        repair: super::background_jobs_integrity::ArtifactRepair,
        now_ms: i64,
    ) -> Result<EnqueueOutcome, StoreError>;
    async fn sync_predictions(
        &self,
        request: super::background_jobs_predictions::SyncPredictions,
    ) -> Result<(), StoreError>;
    async fn pending_predictions(
        &self,
        now_ms: i64,
    ) -> Result<Vec<super::NewAnalysisRequest>, StoreError>;
    async fn preparation_demands(
        &self,
        now_ms: i64,
    ) -> Result<Vec<super::background_jobs_preparation::PreparationDemand>, StoreError>;
    async fn hot_artifacts(
        &self,
        now_ms: i64,
    ) -> Result<Vec<super::background_jobs_preparation::HotArtifact>, StoreError>;
    async fn transcode_copy_sources(
        &self,
        artifact_key: &str,
    ) -> Result<Vec<super::background_jobs_transcode::TranscodeCopySource>, StoreError>;
    /// Immutable producer provenance, including when all advertised holders expired.
    async fn artwork_variant(
        &self,
        artifact_key: &str,
    ) -> Result<Option<super::background_jobs_artwork::ArtworkLocation>, StoreError>;
    async fn enqueue_artwork_demand(
        &self,
        spec: super::background_jobs_artwork::ArtworkVariantSpec,
        node_id: &str,
        now_ms: i64,
    ) -> Result<EnqueueOutcome, StoreError>;
    async fn artwork_locations(
        &self,
        artifact_key: &str,
        now_ms: i64,
    ) -> Result<Vec<super::background_jobs_artwork::ArtworkLocation>, StoreError>;
    async fn publish_artwork_job(
        &self,
        request: super::background_jobs_artwork::PublishArtworkJob,
    ) -> Result<JobPublishOutcome, StoreError>;

    /// Pending compatibility records are an outbox; only the common queue executes them.
    async fn subtitle_job_intents(&self, limit: usize) -> Result<Vec<String>, StoreError>;
    async fn enqueue_subtitle_job(
        &self,
        request: super::AnalysisRequest,
        now_ms: i64,
    ) -> Result<EnqueueOutcome, StoreError>;
    async fn write_subtitle_job(
        &self,
        request: super::background_jobs_subtitle::WriteSubtitleJob,
    ) -> Result<bool, StoreError>;

    async fn update_provider_budget(
        &self,
        request: super::background_jobs_provider::ProviderBudgetRequest,
    ) -> Result<super::background_jobs_provider::ProviderBudgetOutcome, StoreError>;
    async fn storage_domains(
        &self,
    ) -> Result<Vec<super::background_jobs_resources::StorageDomainMapping>, StoreError>;
    async fn replace_storage_domains(
        &self,
        mappings: Vec<super::background_jobs_resources::StorageDomainMapping>,
        now_ms: i64,
    ) -> Result<bool, StoreError>;
    async fn enqueue_library_work(
        &self,
        request: NewLibraryWork,
    ) -> Result<EnqueueOutcome, StoreError>;
    async fn library_work_requests(
        &self,
        query: LibraryWorkQuery,
    ) -> Result<Vec<LibraryWorkRecord>, StoreError>;
    async fn complete_library_work(&self, request: CompleteLibraryWork)
        -> Result<bool, StoreError>;

    async fn bind_library_job(&self, request: BindLibraryJob) -> Result<bool, StoreError>;
    async fn bind_transcode_job_recipe(
        &self,
        token: JobToken,
        recipe_hash: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;
    async fn join_offline_job(
        &self,
        request: JoinOfflineJob,
    ) -> Result<Option<EnqueueOutcome>, StoreError>;
    async fn import_legacy_jobs(&self, now_ms: i64) -> Result<bool, StoreError>;
    async fn job_migration_status(&self) -> Result<JobMigrationStatus, StoreError>;
    async fn enqueue_fragment_job(
        &self,
        request: EnqueueFragmentJob,
    ) -> Result<EnqueueOutcome, StoreError>;
    async fn job_waiters(&self, query: WaiterQuery) -> Result<WaiterPage, StoreError>;
    async fn delivery_intents(&self, now_ms: i64) -> Result<Vec<DeliveryIntent>, StoreError>;
    async fn enqueue_delivery(
        &self,
        intent: DeliveryIntent,
        now_ms: i64,
    ) -> Result<EnqueueOutcome, StoreError>;
    /// Authoritative single-job lookup for ownership-sensitive cleanup.
    async fn background_job(&self, id: &str) -> Result<Option<BackgroundJob>, StoreError>;
    async fn background_staging_jobs(&self, node_id: &str) -> Result<Vec<String>, StoreError>;
    async fn enqueue_job(&self, request: EnqueueJob) -> Result<EnqueueOutcome, StoreError>;
    /// A fresh administrator interest, never resurrection of old consumers.
    /// Fragment builds use the domain analysis generation path instead.
    async fn retry_background_job(
        &self,
        job_id: &str,
        request_id: &str,
        user_id: i64,
        now_ms: i64,
    ) -> Result<Option<EnqueueOutcome>, StoreError>;
    /// Atomically advance the discovery lease with the admission verdict.
    async fn enqueue_job_fenced(
        &self,
        request: EnqueueJob,
        lease: crate::cluster::coordination::Lease,
        replacement: crate::cluster::coordination::Lease,
    ) -> Result<EnqueueOutcome, StoreError>;
    async fn claim_job(&self, request: ClaimJob) -> Result<ClaimOutcome, StoreError>;
    /// Narrow entry point shared by voter and learner artifact workers.
    async fn claim_artifact_job(&self, request: ClaimJob) -> Result<ClaimOutcome, StoreError>;
    async fn resolve_claim(&self, request: ResolveClaim) -> Result<ClaimResolution, StoreError>;
    async fn job_labels(&self, ids: &[String]) -> Result<Vec<JobLabel>, StoreError>;
    async fn job_counts(&self, now_ms: i64) -> Result<Vec<JobCount>, StoreError>;
    async fn source_io_holders(&self, now_ms: i64) -> Result<Vec<SourceIoHolder>, StoreError>;
    async fn job_attempts(&self, job_id: &str) -> Result<Vec<JobAttemptObservation>, StoreError>;
    async fn list_jobs(&self, query: JobQuery) -> Result<JobPage, StoreError>;
    async fn job_candidates(&self, query: CandidateQuery) -> Result<CandidatePage, StoreError>;
    async fn renew_jobs(&self, request: RenewJobs) -> Result<Vec<RenewOutcome>, StoreError>;
    /// True acknowledges this token's transition. Read current job state
    /// separately: per-interest reduction and a new claim may advance it.
    async fn settle_job(&self, request: SettleJob) -> Result<bool, StoreError>;
    async fn fail_fragment_job(&self, request: FragmentJobFailure) -> Result<bool, StoreError>;
    async fn cancel_job(&self, request: CancelJob) -> Result<Option<BackgroundJob>, StoreError>;
    async fn cancel_waiter(
        &self,
        request: CancelWaiter,
    ) -> Result<Option<CancelWaiterOutcome>, StoreError>;
    /// At most 128 rows per upkeep category; no Raft write when nothing is due.
    async fn maintain_jobs(&self, now_ms: i64) -> Result<bool, StoreError>;
    async fn publish_transcode_job(
        &self,
        request: PublishTranscodeJob,
    ) -> Result<JobPublishOutcome, StoreError>;
    async fn publish_fragment_job(
        &self,
        request: PublishFragmentJob,
    ) -> Result<JobPublishOutcome, StoreError>;
}

/// Narrow internal SQL bridge: callers cannot submit SQL through Store.
/// Authority reads use the replicated leader barrier; listing is advisory.
#[async_trait]
pub(super) trait QueueSql: Send + Sync {
    async fn queue_transaction(&self, statements: Vec<(String, String)>) -> Result<(), StoreError>;
    async fn queue_sql(
        &self,
        sql: String,
        request: String,
        mutation: bool,
        authoritative: bool,
    ) -> Result<Vec<String>, StoreError>;
}

// Claim acknowledgement and renewal reconciliation must not revive a token
// whose library now requires different storage. Return the row and resource
// verdict from one authoritative snapshot, not two independently timed reads.
const CLAIM_RESOURCE_SNAPSHOT: &str = r#"NOT EXISTS (
    SELECT 1 FROM background_job_required_resources required
    WHERE required.job_id = background_jobs.id AND NOT EXISTS (
        SELECT 1 FROM background_job_reservations held
        WHERE held.job_id = background_jobs.id AND held.fence = background_jobs.fence
            AND held.resource_key = required.resource_key
            AND held.expires_at_ms >= background_jobs.lease_expires_ms))"#;

#[derive(Deserialize)]
struct ClaimSnapshot {
    job: BackgroundJob,
    resources_current: i64,
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value)
        .map_err(|error| StoreError::Database(format!("invalid background job row: {error}")))
}

pub(super) async fn enqueue_body<T: QueueSql>(
    store: &T,
    request: &EnqueueJob,
) -> Result<serde_json::Value, StoreError> {
    let fragment_policy = matches!(
        request.payload,
        JobPayload::FragmentIndexBuild { .. } | JobPayload::SubtitleExtract { .. }
    ) || matches!(&request.payload, JobPayload::ArtifactHydrate { artifact_key, .. } if artifact_key.starts_with("fragment:"));
    let limit = if fragment_policy {
        let rows = store.queue_sql("SELECT json_quote(value) AS result_json FROM settings WHERE key = json_extract($1, '$.key')".into(),
            encode(&serde_json::json!({"key": super::keys::ANALYSIS_MAX_ATTEMPTS}))?, false, true).await?;
        let configured: Option<String> = rows.first().map(|row| decode(row)).transpose()?;
        super::bounded_analysis_max_attempts(configured.as_deref())
    } else {
        5
    };
    let mut body = serde_json::to_value(request).map_err(|error| invalid(&error.to_string()))?;
    body["attempt_limit"] = limit.into();
    Ok(body)
}

#[async_trait]
impl<T: QueueSql> BackgroundJobStore for T {
    async fn join_analysis_viewer(
        &self,
        interest: AnalysisViewerInterest,
    ) -> Result<bool, StoreError> {
        if !identifier(&interest.analysis_request_id)
            || !identifier(&interest.target_node_id)
            || interest.requested_generation.is_empty()
            || interest.requested_generation.len() > 128
            || interest.pipeline_version.is_empty()
            || interest.pipeline_version.len() > 128
            || interest.video_identity.len() > 128
            || interest.user_id <= 0
            || interest.playback_id.is_empty()
            || interest.playback_id.len() > 128
            || interest.now_ms < 0
            || interest.now_ms > i64::MAX - 604_800_000
        {
            return Err(invalid("invalid analysis viewer interest"));
        }
        let mut body =
            serde_json::to_value(&interest).map_err(|error| invalid(&error.to_string()))?;
        body["consumer_id"] = interest.consumer_id().into();
        body["command_id"] = uuid::Uuid::new_v4().to_string().into();
        let rows = self
            .queue_sql(
                JOIN_ANALYSIS_VIEWER_SQL.to_owned(),
                encode(&body)?,
                true,
                true,
            )
            .await?;
        Ok(!rows.is_empty())
    }
    async fn embedding_for(
        &self,
        item_id: i64,
        content_digest: &str,
        model_digest: &str,
    ) -> Result<super::background_jobs_embeddings::EmbeddingLookup, StoreError> {
        super::background_jobs_embeddings::lookup(self, item_id, content_digest, model_digest).await
    }
    async fn publish_embedding_job(
        &self,
        request: super::background_jobs_embeddings::PublishEmbeddingJob,
    ) -> Result<JobPublishOutcome, StoreError> {
        super::background_jobs_embeddings::publish(self, request).await
    }
    async fn publish_probe_job(
        &self,
        request: super::background_jobs_probe::PublishProbeJob,
    ) -> Result<JobPublishOutcome, StoreError> {
        super::background_jobs_probe::publish(self, request).await
    }
    async fn apply_probe_job(
        &self,
        request: super::background_jobs_probe::ApplyProbeJob,
    ) -> Result<bool, StoreError> {
        super::background_jobs_probe::apply(self, request).await
    }
    async fn transcode_verification_candidates(
        &self,
        node_id: &str,
        artifact_key: Option<&str>,
    ) -> Result<Vec<super::background_jobs_integrity::TranscodeVerificationCandidate>, StoreError>
    {
        super::background_jobs_integrity::transcode_candidates(self, node_id, artifact_key).await
    }
    async fn artwork_verification_candidates(
        &self,
        node_id: &str,
    ) -> Result<Vec<super::background_jobs_artwork::ArtworkLocation>, StoreError> {
        super::background_jobs_integrity::artwork_candidates(self, node_id).await
    }
    async fn verify_artwork_job(
        &self,
        request: super::background_jobs_integrity::VerifyArtwork,
    ) -> Result<JobPublishOutcome, StoreError> {
        super::background_jobs_integrity::verify_artwork(self, request).await
    }
    async fn verify_transcode_job(
        &self,
        request: super::background_jobs_integrity::VerifyTranscode,
    ) -> Result<JobPublishOutcome, StoreError> {
        super::background_jobs_integrity::verify_transcode(self, request).await
    }
    async fn artifact_repairs(
        &self,
        pending_only: bool,
    ) -> Result<Vec<super::background_jobs_integrity::ArtifactRepair>, StoreError> {
        super::background_jobs_integrity::repairs(self, pending_only).await
    }
    async fn enqueue_artifact_repair(
        &self,
        repair: super::background_jobs_integrity::ArtifactRepair,
        now_ms: i64,
    ) -> Result<EnqueueOutcome, StoreError> {
        super::background_jobs_integrity::enqueue_repair(self, repair, now_ms).await
    }
    async fn sync_predictions(
        &self,
        request: super::background_jobs_predictions::SyncPredictions,
    ) -> Result<(), StoreError> {
        super::background_jobs_predictions::sync(self, request).await
    }
    async fn pending_predictions(
        &self,
        now_ms: i64,
    ) -> Result<Vec<super::NewAnalysisRequest>, StoreError> {
        super::background_jobs_predictions::pending(self, now_ms).await
    }
    async fn preparation_demands(
        &self,
        now_ms: i64,
    ) -> Result<Vec<super::background_jobs_preparation::PreparationDemand>, StoreError> {
        super::background_jobs_preparation::demands(self, now_ms).await
    }
    async fn hot_artifacts(
        &self,
        now_ms: i64,
    ) -> Result<Vec<super::background_jobs_preparation::HotArtifact>, StoreError> {
        super::background_jobs_preparation::hot_artifacts(self, now_ms).await
    }
    async fn transcode_copy_sources(
        &self,
        artifact_key: &str,
    ) -> Result<Vec<super::background_jobs_transcode::TranscodeCopySource>, StoreError> {
        super::background_jobs_transcode::sources(self, artifact_key).await
    }
    async fn artwork_variant(
        &self,
        artifact_key: &str,
    ) -> Result<Option<super::background_jobs_artwork::ArtworkLocation>, StoreError> {
        super::background_jobs_artwork::canonical(self, artifact_key).await
    }
    async fn enqueue_artwork_demand(
        &self,
        spec: super::background_jobs_artwork::ArtworkVariantSpec,
        node_id: &str,
        now_ms: i64,
    ) -> Result<EnqueueOutcome, StoreError> {
        super::background_jobs_artwork::enqueue(self, spec, node_id, now_ms).await
    }
    async fn artwork_locations(
        &self,
        artifact_key: &str,
        now_ms: i64,
    ) -> Result<Vec<super::background_jobs_artwork::ArtworkLocation>, StoreError> {
        super::background_jobs_artwork::locations(self, artifact_key, now_ms).await
    }
    async fn publish_artwork_job(
        &self,
        request: super::background_jobs_artwork::PublishArtworkJob,
    ) -> Result<JobPublishOutcome, StoreError> {
        super::background_jobs_artwork::publish(self, request).await
    }

    async fn subtitle_job_intents(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        super::background_jobs_subtitle::intents(self, limit).await
    }
    async fn enqueue_subtitle_job(
        &self,
        request: super::AnalysisRequest,
        now_ms: i64,
    ) -> Result<EnqueueOutcome, StoreError> {
        super::background_jobs_subtitle::enqueue(self, request, now_ms).await
    }
    async fn write_subtitle_job(
        &self,
        request: super::background_jobs_subtitle::WriteSubtitleJob,
    ) -> Result<bool, StoreError> {
        super::background_jobs_subtitle::write(self, request).await
    }

    async fn update_provider_budget(
        &self,
        request: super::background_jobs_provider::ProviderBudgetRequest,
    ) -> Result<super::background_jobs_provider::ProviderBudgetOutcome, StoreError> {
        super::background_jobs_provider::update(self, request).await
    }
    async fn storage_domains(
        &self,
    ) -> Result<Vec<super::background_jobs_resources::StorageDomainMapping>, StoreError> {
        super::background_jobs_resources::list(self).await
    }
    async fn replace_storage_domains(
        &self,
        mappings: Vec<super::background_jobs_resources::StorageDomainMapping>,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        super::background_jobs_resources::replace(self, mappings, now_ms).await
    }
    async fn enqueue_library_work(
        &self,
        request: NewLibraryWork,
    ) -> Result<EnqueueOutcome, StoreError> {
        super::background_jobs_library::enqueue(self, request).await
    }
    async fn library_work_requests(
        &self,
        query: LibraryWorkQuery,
    ) -> Result<Vec<LibraryWorkRecord>, StoreError> {
        super::background_jobs_library::list(self, query).await
    }
    async fn complete_library_work(
        &self,
        request: CompleteLibraryWork,
    ) -> Result<bool, StoreError> {
        super::background_jobs_library::complete(self, request).await
    }

    async fn bind_library_job(&self, request: BindLibraryJob) -> Result<bool, StoreError> {
        super::background_jobs_domain::bind(self, request).await
    }
    async fn bind_transcode_job_recipe(
        &self,
        token: JobToken,
        recipe_hash: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        super::background_jobs_offline::bind_recipe(self, token, recipe_hash, now_ms).await
    }
    async fn join_offline_job(
        &self,
        request: JoinOfflineJob,
    ) -> Result<Option<EnqueueOutcome>, StoreError> {
        super::background_jobs_offline::join(self, request).await
    }
    async fn import_legacy_jobs(&self, now_ms: i64) -> Result<bool, StoreError> {
        super::background_jobs_migration::import_page(self, now_ms).await
    }
    async fn job_migration_status(&self) -> Result<JobMigrationStatus, StoreError> {
        let rows = self
            .queue_sql(
                super::background_jobs_migration::STATUS_SQL.into(),
                "{}".into(),
                false,
                true,
            )
            .await?;
        decode(
            rows.first()
                .ok_or_else(|| invalid("missing queue migration status"))?,
        )
    }

    async fn enqueue_fragment_job(
        &self,
        input: EnqueueFragmentJob,
    ) -> Result<EnqueueOutcome, StoreError> {
        let (request, logical_key) = super::background_jobs_fragment_admission::prepare(&input)?;
        let mut body = enqueue_body(self, &request).await?;
        let mut domain = input.job;
        if input.repair {
            domain.priority = "normal".into();
            domain.trigger = "background".into();
        }
        body["fragment_domain"] =
            serde_json::to_value(domain).map_err(|error| invalid(&error.to_string()))?;
        body["logical_key"] = logical_key.into();
        body["fragment_repair"] = input.repair.into();
        if let Some(analysis) = input.analysis_request {
            body["analysis_request"] =
                serde_json::to_value(analysis).map_err(|error| invalid(&error.to_string()))?;
        }
        let rows = self
            .queue_sql(ENQUEUE_SQL.into(), encode(&body)?, true, true)
            .await?;
        decode(
            rows.first()
                .ok_or_else(|| invalid("fragment admission returned no verdict"))?,
        )
    }

    async fn job_waiters(&self, query: WaiterQuery) -> Result<WaiterPage, StoreError> {
        if uuid::Uuid::parse_str(&query.job_id).is_err()
            || query.limit == 0
            || query.limit > MAX_PAGE_SIZE
            || query
                .after
                .as_ref()
                .is_some_and(|after| !identifier(&after.scope) || !identifier(&after.request_id))
        {
            return Err(invalid("invalid job waiter page"));
        }
        let rows = self
            .queue_sql(WAITERS_SQL.into(), encode(&query)?, false, false)
            .await?;
        let mut waiters: Vec<JobWaiter> = rows
            .iter()
            .map(|row| decode(row))
            .collect::<Result<_, _>>()?;
        let more = waiters.len() > query.limit;
        waiters.truncate(query.limit);
        let next = if more {
            waiters.last().map(|waiter| WaiterCursor {
                scope: waiter.scope.clone(),
                request_id: waiter.request_id.clone(),
            })
        } else {
            None
        };
        Ok(WaiterPage { waiters, next })
    }

    async fn enqueue_delivery(
        &self,
        intent: DeliveryIntent,
        now_ms: i64,
    ) -> Result<EnqueueOutcome, StoreError> {
        if uuid::Uuid::parse_str(&intent.job_id).is_err()
            || !(intent
                .artifact_key
                .strip_prefix("fragment:")
                .or_else(|| intent.artifact_key.strip_prefix("artwork:"))
                .is_some_and(digest)
                || super::background_jobs_transcode::parse_key(&intent.artifact_key).is_some())
        {
            return Err(invalid("invalid background delivery intent"));
        }
        let (payload, request_digest) = super::background_jobs_delivery::hydration_identity(
            &intent.artifact_key,
            &intent.target_node_id,
        )?;
        let request = EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            dedupe_key: format!("hydrate:{request_digest}"),
            priority: intent.priority,
            not_before_ms: now_ms,
            now_ms,
            request: JobRequest {
                scope: format!("delivery:{}", intent.job_id),
                request_id: intent.target_node_id.clone(),
                request_digest,
                consumer_kind: "background_delivery".into(),
                consumer_ref: intent.job_id.clone(),
                target_node_id: Some(intent.target_node_id),
                deadline_ms: None,
                retain_identity: false,
            },
        };
        request.validate()?;
        let mut body = enqueue_body(self, &request).await?;
        body["delivery_parent"] = intent.job_id.into();
        let rows = self
            .queue_sql(ENQUEUE_SQL.into(), encode(&body)?, true, true)
            .await?;
        decode(
            rows.first()
                .ok_or_else(|| invalid("delivery admission returned no verdict"))?,
        )
    }

    async fn delivery_intents(&self, now_ms: i64) -> Result<Vec<DeliveryIntent>, StoreError> {
        if now_ms < 0 {
            return Err(invalid("invalid delivery scheduling time"));
        }
        let rows = self
            .queue_sql(
                DELIVERIES_SQL.into(),
                encode(&serde_json::json!({"now_ms": now_ms}))?,
                false,
                false,
            )
            .await?;
        rows.iter().map(|row| decode(row)).collect()
    }

    async fn background_job(&self, id: &str) -> Result<Option<BackgroundJob>, StoreError> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(invalid("invalid background job id"));
        }
        let rows = self.queue_sql(format!("SELECT {JOB_JSON} AS result_json FROM background_jobs WHERE id = json_extract($1, '$.id')"),
            encode(&serde_json::json!({"id": id}))?, false, true).await?;
        rows.first().map(|row| decode(row)).transpose()
    }

    async fn background_staging_jobs(&self, node_id: &str) -> Result<Vec<String>, StoreError> {
        if !identifier(node_id) {
            return Err(invalid("invalid background staging node"));
        }
        let rows = self
            .queue_sql(
                "SELECT json_quote(id) AS result_json FROM background_jobs job
            WHERE (kind = 'transcode_prepare' OR (kind = 'artifact_hydrate'
                AND json_extract(payload_json, '$.artifact_key') LIKE 'transcode:%'))
            AND state IN ('queued','running','cancelling')
            AND ((job.fence = 0 AND json_extract(job.checkpoint_json, '$.staging_node_id') = json_extract($1, '$.node_id'))
                OR EXISTS (SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id = job.id
                AND attempt.fence = job.fence AND attempt.owner_node_id = json_extract($1, '$.node_id')))
            ORDER BY id LIMIT 4097"
                    .into(),
                encode(&serde_json::json!({"node_id": node_id}))?,
                false,
                true,
            )
            .await?;
        if rows.len() > MAX_ACTIVE_JOBS {
            return Err(invalid("background staging inventory exceeds active bound"));
        }
        rows.iter().map(|row| decode(row)).collect()
    }

    async fn publish_fragment_job(
        &self,
        request: PublishFragmentJob,
    ) -> Result<JobPublishOutcome, StoreError> {
        request.token.validate()?;
        let artifact = &request.artifact;
        if request.now_ms < 0
            || artifact.file_id <= 0
            || artifact.source_size < 0
            || artifact.bytes <= 0
            || artifact.bytes > super::MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES as i64
            || artifact.built_at_ms < 0
            || !identifier(&artifact.built_by_node_id)
            || !digest(&artifact.cache_key)
            || !digest(&artifact.source_sha256)
            || !digest(&artifact.pipeline_sha256)
            || !digest(&artifact.blob_sha256)
        {
            return Err(invalid("invalid fragment job publication"));
        }
        let logical_key = super::cluster_fragment_index_key(
            artifact.file_id,
            artifact.source_size,
            artifact.source_mtime,
            &artifact.source_sha256,
            &artifact.pipeline_sha256,
        )
        .ok_or_else(|| invalid("invalid fragment source identity"))?;
        use sha2::{Digest, Sha256};
        let publication_digest = hex::encode(Sha256::digest(encode(&request.artifact)?.as_bytes()));
        let mut body =
            serde_json::to_value(request).map_err(|error| invalid(&error.to_string()))?;
        body["logical_key"] = logical_key.into();
        body["publication_digest"] = publication_digest.into();
        let rows = self
            .queue_sql(PUBLISH_FRAGMENT_SQL.into(), encode(&body)?, true, true)
            .await?;
        decode(
            rows.first()
                .ok_or_else(|| invalid("fragment publication returned no verdict"))?,
        )
    }

    async fn publish_transcode_job(
        &self,
        request: PublishTranscodeJob,
    ) -> Result<JobPublishOutcome, StoreError> {
        request.token.validate()?;
        let output = &request.output;
        let relative = std::path::Path::new(&output.relative_dir);
        if request.now_ms < 0
            || !digest(&output.recipe_hash)
            || !digest(&output.manifest_digest)
            || output.recipe_version <= 0
            || output.bytes < 0
            || output
                .expected_previous_bytes
                .is_some_and(|previous| previous < 0 || output.bytes < previous)
            || output.relative_dir.is_empty()
            || output.relative_dir.len() > 512
            || output.relative_dir.contains(':')
            || output
                .relative_dir
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || output.relative_dir.contains('\\')
            || output.relative_dir.contains('\0')
            || !relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(invalid("invalid background transcode publication"));
        }
        let rows = self
            .queue_sql(
                PUBLISH_TRANSCODE_SQL.to_owned(),
                encode(&request)?,
                true,
                true,
            )
            .await?;
        decode(
            rows.first()
                .ok_or_else(|| invalid("background publication returned no verdict"))?,
        )
    }

    async fn job_candidates(&self, query: CandidateQuery) -> Result<CandidatePage, StoreError> {
        if !identifier(&query.node_id)
            || query.kinds.is_empty()
            || query.kinds.len() > 10
            || query.now_ms < 0
            || query.limit == 0
            || query.limit > MAX_PAGE_SIZE
            || query.after.as_ref().is_some_and(|cursor| {
                cursor.priority > 3
                    || cursor.fairness_floor > 3
                    || cursor.scope_last_claim_ms < 0
                    || cursor.selected_at_ms < 0
                    || cursor.selected_at_ms > query.now_ms
            })
        {
            return Err(invalid("invalid background candidate query"));
        }
        let rows = self.queue_sql(format!(
            r#"WITH selection AS (
                SELECT COALESCE(json_extract($1, '$.after.selected_at_ms'), json_extract($1, '$.now_ms')) AS selected_at_ms
            ), recent AS (
                SELECT job.priority FROM background_job_attempts attempt JOIN background_jobs job ON job.id = attempt.job_id
                WHERE attempt.owner_node_id = json_extract($1, '$.node_id')
                  AND job.kind IN (SELECT value FROM json_each($1, '$.kinds'))
                  AND attempt.outcome IN ('succeeded','failed','cancelled')
                  AND attempt.finished_at_ms <= (SELECT selected_at_ms FROM selection)
                ORDER BY attempt.finished_at_ms DESC, attempt.job_id, attempt.fence DESC LIMIT 8
            ), fairness AS (
                SELECT COALESCE(json_extract($1, '$.after.fairness_floor'),
                    CASE WHEN COUNT(*) = 8 THEN MIN(priority) ELSE 0 END) AS fairness_floor FROM recent
            ), interests AS (
                SELECT waiter.job_id, waiter.state, waiter.not_before_ms, waiter.retry_deadline_ms,
                    waiter.deadline_ms, waiter.failed_attempts, waiter.attempt_limit, waiter.participation_fence,
                    CASE WHEN waiter.request_scope LIKE 'user:%' THEN waiter.request_scope
                        WHEN COALESCE(item.library_id, json_extract(job.payload_json, '$.library_id')) IS NOT NULL
                        THEN 'library:' || COALESCE(item.library_id, json_extract(job.payload_json, '$.library_id'))
                        ELSE waiter.request_scope END AS scheduling_scope
                FROM background_job_waiters waiter JOIN background_jobs job ON job.id = waiter.job_id
                LEFT JOIN files file ON file.id = json_extract(job.payload_json, '$.file_id')
                LEFT JOIN items item ON item.id = file.item_id
            ), scope_usage AS (
                SELECT interest.scheduling_scope, MAX(attempt.started_at_ms) AS last_claim_ms
                FROM interests interest JOIN background_job_attempts attempt ON attempt.job_id = interest.job_id
                WHERE attempt.owner_node_id = json_extract($1, '$.node_id')
                    AND attempt.started_at_ms <= (SELECT selected_at_ms FROM selection)
                GROUP BY interest.scheduling_scope
            ), eligible AS (
                SELECT job.id, MIN(COALESCE(usage.last_claim_ms, 0)) AS scope_last_claim_ms
                FROM background_jobs job JOIN interests interest ON interest.job_id = job.id
                LEFT JOIN scope_usage usage ON usage.scheduling_scope = interest.scheduling_scope
                WHERE job.kind IN (SELECT value FROM json_each($1, '$.kinds')) AND job.payload_version = 1
                    AND (job.kind != 'subtitle_extract' OR EXISTS (SELECT 1 FROM analysis_requests request JOIN files file ON file.id = request.file_id
        AND file.size = request.source_size AND file.mtime = request.source_mtime
      WHERE request.request_id = json_extract(job.payload_json, '$.source_generation')
        AND request.file_id = json_extract(job.payload_json, '$.file_id')
        AND request.component = 'subtitle_source' AND request.state IN ('queued','running')
        AND request.cancel_requested = 0))
                    AND (job.target_node_id IS NULL OR job.target_node_id = json_extract($1, '$.node_id'))
                    AND (job.state = 'queued' OR (job.state = 'running' AND job.lease_expires_ms <= json_extract($1, '$.now_ms')))
                    AND (job.retry_deadline_ms = 0 OR job.retry_deadline_ms > json_extract($1, '$.now_ms'))
                    AND job.not_before_ms <= json_extract($1, '$.now_ms')
                    AND interest.state = 'pending' AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract($1, '$.now_ms'))
                    AND (job.kind != 'fragment_index_build' OR (
                        interest.not_before_ms <= json_extract($1, '$.now_ms')
                        AND (interest.retry_deadline_ms = 0 OR interest.retry_deadline_ms > json_extract($1, '$.now_ms'))
                        AND interest.failed_attempts + CASE WHEN job.state = 'running'
                          AND interest.participation_fence = job.fence THEN 1 ELSE 0 END < interest.attempt_limit))
                GROUP BY job.id
            ), ranked AS (
                SELECT job.*, eligible.scope_last_claim_ms, selection.selected_at_ms, fairness.fairness_floor,
                    CASE WHEN job.priority = 3 THEN 6 WHEN job.priority < fairness.fairness_floor THEN 5
                        ELSE job.priority * 2 END AS selection_rank
                FROM background_jobs job JOIN eligible ON eligible.id = job.id CROSS JOIN fairness CROSS JOIN selection
            )
            SELECT json_object('job', {JOB_JSON}, 'scope_last_claim_ms', scope_last_claim_ms,
                'selected_at_ms', selected_at_ms, 'fairness_floor', fairness_floor) AS result_json FROM ranked
            WHERE json_extract($1, '$.after') IS NULL
                OR selection_rank < CASE WHEN json_extract($1, '$.after.priority') = 3 THEN 6
                    WHEN json_extract($1, '$.after.priority') < fairness_floor THEN 5 ELSE json_extract($1, '$.after.priority') * 2 END
                OR (selection_rank = CASE WHEN json_extract($1, '$.after.priority') = 3 THEN 6
                    WHEN json_extract($1, '$.after.priority') < fairness_floor THEN 5 ELSE json_extract($1, '$.after.priority') * 2 END
                  AND (scope_last_claim_ms, created_at_ms, id) > (json_extract($1, '$.after.scope_last_claim_ms'),
                    json_extract($1, '$.after.created_at_ms'), json_extract($1, '$.after.id')))
            ORDER BY selection_rank DESC, scope_last_claim_ms, created_at_ms, id LIMIT json_extract($1, '$.limit') + 1"#
        ), encode(&query)?, false, false).await?;
        #[derive(Deserialize)]
        struct Candidate {
            job: BackgroundJob,
            scope_last_claim_ms: i64,
            selected_at_ms: i64,
            fairness_floor: u8,
        }
        let mut candidates: Vec<Candidate> = rows
            .iter()
            .map(|row| decode(row))
            .collect::<Result<_, _>>()?;
        let more = candidates.len() > query.limit;
        candidates.truncate(query.limit);
        let next = more
            .then(|| {
                candidates.last().map(|candidate| CandidateCursor {
                    selected_at_ms: candidate.selected_at_ms,
                    fairness_floor: candidate.fairness_floor,
                    scope_last_claim_ms: candidate.scope_last_claim_ms,
                    priority: candidate.job.priority,
                    created_at_ms: candidate.job.created_at_ms,
                    id: candidate.job.id.clone(),
                })
            })
            .flatten();
        Ok(CandidatePage {
            jobs: candidates
                .into_iter()
                .map(|candidate| candidate.job)
                .collect(),
            next,
        })
    }

    async fn cancel_waiter(
        &self,
        request: CancelWaiter,
    ) -> Result<Option<CancelWaiterOutcome>, StoreError> {
        if !identifier(&request.scope) || !identifier(&request.request_id) || request.now_ms < 0 {
            return Err(invalid("invalid background waiter cancellation"));
        }
        let rows = self
            .queue_sql(CANCEL_WAITER_SQL.to_owned(), encode(&request)?, true, true)
            .await?;
        rows.first().map(|row| decode(row)).transpose()
    }

    async fn maintain_jobs(&self, now_ms: i64) -> Result<bool, StoreError> {
        if now_ms < 0 {
            return Err(invalid("invalid background maintenance time"));
        }
        let request = encode(
            &serde_json::json!({"now_ms": now_ms, "command_id": uuid::Uuid::new_v4().to_string()}),
        )?;
        let reconciled = super::background_jobs_subtitle::reconcile(self, &request).await?;
        let refreshed = !self
            .queue_sql(
                REFRESH_ANALYSIS_VIEWERS_NEEDED.to_owned(),
                request.clone(),
                false,
                false,
            )
            .await?
            .is_empty();
        if refreshed {
            self.queue_sql(
                REFRESH_ANALYSIS_VIEWERS_SQL.to_owned(),
                request.clone(),
                true,
                true,
            )
            .await?;
        }
        if self
            .queue_sql(MAINTENANCE_NEEDED.to_owned(), request.clone(), false, false)
            .await?
            .is_empty()
        {
            return Ok(reconciled || refreshed);
        }
        self.queue_sql(MAINTENANCE_SQL.to_owned(), request, true, true)
            .await?;
        Ok(true)
    }

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

    async fn fail_fragment_job(&self, request: FragmentJobFailure) -> Result<bool, StoreError> {
        request.token.validate()?;
        if request.now_ms < 0 {
            return Err(invalid("invalid index failure time"));
        }
        let Some(job) = self.background_job(&request.token.job_id).await? else {
            return Ok(false);
        };
        if job.token.as_ref() != Some(&request.token) || job.state != JobState::Running {
            return Ok(false);
        }
        let payload = job.supported_payload()?;
        if !matches!(payload, JobPayload::FragmentIndexBuild { .. })
            && !matches!(&payload, JobPayload::ArtifactHydrate { artifact_key, .. } if artifact_key.starts_with("fragment:"))
        {
            return Err(invalid("index retry policy used for another job kind"));
        }
        let attempt = job.failed_attempts + 1;
        let decision = crate::content_analysis::index_retry_decision(
            request.code,
            request.transient_allowlisted,
            attempt as u32,
            job.attempt_limit as u32,
            request.now_ms,
            job.retry_deadline_ms,
        );
        let mut diagnostic = request.diagnostic;
        diagnostic.version = 1;
        diagnostic.code = request.code.as_str().into();
        diagnostic.retryable = decision.retryable;
        diagnostic.claim_fence = request.token.fence;
        diagnostic.attempt = attempt;
        diagnostic.recorded_at_ms = request.now_ms;
        // Reserve room for the per-interest attempt/retry fields added inside
        // the transaction, without relaxing the 8 KiB diagnostic bound.
        let diagnostic = loop {
            let encoded = diagnostic.encode_bounded().map_err(StoreError::Task)?;
            if encoded.len() <= 8 * 1024 - 128 {
                break encoded;
            }
            if diagnostic.stderr_tail.is_empty() {
                return Err(invalid("index diagnostic exceeds its metadata reserve"));
            }
            diagnostic.stderr_tail.remove(0);
        };
        let terminal = if attempt >= job.attempt_limit {
            "attempt_limit"
        } else {
            decision
                .terminal_code
                .map(|code| code.as_str())
                .unwrap_or(request.code.as_str())
        };
        let settlement = if matches!(payload, JobPayload::FragmentIndexBuild { .. }) {
            JobSettlement::Retry {
                error_code: request.code.as_str().into(),
                not_before_ms: decision.next_attempt_at_ms,
            }
        } else if decision.retryable {
            JobSettlement::Retry {
                error_code: terminal.into(),
                not_before_ms: decision.next_attempt_at_ms,
            }
        } else {
            JobSettlement::Fail {
                error_code: terminal.into(),
            }
        };
        let mut body = serde_json::to_value(SettleJob {
            token: request.token,
            settlement,
            now_ms: request.now_ms,
        })
        .map_err(|error| invalid(&error.to_string()))?;
        body["retry_deadline_ms"] = decision.retry_deadline_ms.into();
        body["index_diagnostic_json"] = diagnostic.into();
        body["attempt_error"] = request.code.as_str().into();
        body["failure_policy"] = if request.code.automatically_retryable()
            || (matches!(
                request.code,
                crate::content_analysis::IndexFailureCode::IndexSourceIo
                    | crate::content_analysis::IndexFailureCode::IndexProcessFailed
            ) && request.transient_allowlisted)
        {
            "index_retry"
        } else {
            "terminal"
        }
        .into();
        let rows = self
            .queue_sql(
                format!("{SETTLE_SQL} RETURNING 'true' AS result_json"),
                encode(&body)?,
                true,
                true,
            )
            .await?;
        Ok(!rows.is_empty())
    }

    async fn settle_job(&self, request: SettleJob) -> Result<bool, StoreError> {
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
            JobSettlement::Fail { error_code } | JobSettlement::Stop { error_code } => {
                if !identifier(error_code) || error_code.len() > 64 {
                    return Err(invalid("invalid background failure code"));
                }
            }
            JobSettlement::Cancel => {}
        }
        let rows = self
            .queue_sql(
                format!("{SETTLE_SQL} RETURNING 'true' AS result_json"),
                encode(&request)?,
                true,
                true,
            )
            .await?;
        Ok(!rows.is_empty())
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
            .queue_sql(
                ENQUEUE_SQL.to_owned(),
                encode(&enqueue_body(self, &request).await?)?,
                true,
                true,
            )
            .await?;
        let row = rows
            .first()
            .ok_or_else(|| invalid("background admission returned no verdict"))?;
        decode(row)
    }

    async fn retry_background_job(
        &self,
        job_id: &str,
        request_id: &str,
        user_id: i64,
        now_ms: i64,
    ) -> Result<Option<EnqueueOutcome>, StoreError> {
        use sha2::{Digest, Sha256};
        if uuid::Uuid::parse_str(job_id).is_err()
            || uuid::Uuid::parse_str(request_id).is_err()
            || user_id <= 0
        {
            return Err(invalid("invalid administrator retry identity"));
        }
        let Some(original) = self.background_job(job_id).await? else {
            return Ok(None);
        };
        if !matches!(original.state, JobState::Failed | JobState::Cancelled) {
            return Ok(Some(EnqueueOutcome::Conflict));
        }
        let payload = original.supported_payload()?;
        if !matches!(
            payload,
            JobPayload::TranscodePrepare { .. } | JobPayload::ArtifactHydrate { .. }
        ) {
            return Err(invalid("retry requires a domain generation adapter"));
        }
        let target = match &payload {
            JobPayload::ArtifactHydrate { target_node_id, .. } => Some(target_node_id.clone()),
            _ => None,
        };
        let request = EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            dedupe_key: original.dedupe_key,
            priority: 2,
            not_before_ms: now_ms,
            now_ms,
            request: JobRequest {
                scope: format!("user:{user_id}"),
                request_id: request_id.into(),
                request_digest: hex::encode(Sha256::digest(format!("background-retry:{job_id}"))),
                consumer_kind: "admin_retry".into(),
                consumer_ref: job_id.into(),
                target_node_id: target,
                deadline_ms: Some(now_ms.saturating_add(24 * 60 * 60 * 1000)),
                retain_identity: false,
            },
        };
        request.validate()?;
        let mut body = enqueue_body(self, &request).await?;
        body["retry_of"] = job_id.into();
        let rows = self
            .queue_sql(ENQUEUE_SQL.into(), encode(&body)?, true, true)
            .await?;
        Ok(Some(decode(
            rows.first()
                .ok_or_else(|| invalid("missing retry verdict"))?,
        )?))
    }

    async fn enqueue_job_fenced(
        &self,
        mut request: EnqueueJob,
        lease: crate::cluster::coordination::Lease,
        replacement: crate::cluster::coordination::Lease,
    ) -> Result<EnqueueOutcome, StoreError> {
        request.now_ms = crate::cluster::coordination::unix_ms()?;
        request.validate()?;
        if lease.resource != replacement.resource
            || lease.owner_node_id != replacement.owner_node_id
            || lease.fence != replacement.fence
            || lease.fence == 0
            || lease.fence > i64::MAX as u64
            || lease.revision == 0
            || lease.revision >= i64::MAX as u64
            || replacement.revision != lease.revision + 1
            || replacement.expires_at_unix_ms <= lease.expires_at_unix_ms
        {
            return Err(invalid("invalid background producer lease replacement"));
        }
        // A completed automatic receipt is immutable, but its bytes may later
        // be evicted. Only the latest *successful* computation can authorize
        // another automatic interest; failure/cancellation never resets a budget.
        #[derive(Deserialize)]
        struct RepairPredecessor {
            id: String,
            created_at_ms: i64,
        }
        let cache_repair_of = if request.request.scope == "automatic:transcode"
            && matches!(request.payload, JobPayload::TranscodePrepare { .. })
        {
            self.queue_sql(
                "SELECT json_object('id', original.id, 'created_at_ms', original.created_at_ms) AS result_json FROM background_jobs original
                 WHERE original.id = (SELECT id FROM background_jobs WHERE dedupe_key = json_extract($1, '$.dedupe_key')
                    ORDER BY created_at_ms DESC, id DESC LIMIT 1)
                   AND original.kind = 'transcode_prepare' AND original.payload_version = 1 AND original.state = 'succeeded'
                   AND json_valid(original.result_ref) AND NOT EXISTS (SELECT 1 FROM transcode_cache_locations location
                     WHERE location.recipe_hash = CASE WHEN json_valid(original.result_ref) THEN json_extract(original.result_ref, '$.recipe_hash') END
                       AND location.complete = 1)".into(), encode(&request)?, false, true,
            ).await?.first().map(|row| decode::<RepairPredecessor>(row)).transpose()?
        } else {
            None
        };
        if let Some(parent) = &cache_repair_of {
            request.request.scope = "automatic:transcode-repair".into();
            request.request.request_id = parent.id.clone();
            request.request.consumer_kind = "transcode_cache_repair".into();
            request.request.consumer_ref = parent.id.clone();
            // Make the successor order deterministic even within one clock tick.
            request.now_ms = request.now_ms.max(parent.created_at_ms.saturating_add(1));
            request.validate()?;
            request.priority = 1;
        }
        let mut body = enqueue_body(self, &request).await?;
        if let Some(parent) = cache_repair_of {
            body["cache_repair_of"] = parent.id.into();
        }
        body["producer_lease"] =
            serde_json::to_value(&lease).map_err(|error| invalid(&error.to_string()))?;
        body["producer_replacement"] =
            serde_json::to_value(replacement).map_err(|error| invalid(&error.to_string()))?;
        let rows = self
            .queue_sql(ENQUEUE_SQL.to_owned(), encode(&body)?, true, true)
            .await?;
        let outcome: EnqueueOutcome = decode(
            rows.first()
                .ok_or_else(|| invalid("missing background admission verdict"))?,
        )?;
        if matches!(outcome, EnqueueOutcome::ProducerFenced) {
            return Err(StoreError::FenceRejected {
                resource: lease.resource,
                owner_node_id: lease.owner_node_id,
                fence: lease.fence,
            });
        }
        Ok(outcome)
    }

    async fn claim_artifact_job(&self, request: ClaimJob) -> Result<ClaimOutcome, StoreError> {
        if !request.kind.permits_artifact_execution() {
            return Err(invalid(
                "artifact workers cannot claim catalogue or provider work",
            ));
        }
        self.claim_job(request).await
    }

    async fn claim_job(&self, request: ClaimJob) -> Result<ClaimOutcome, StoreError> {
        request.validate()?;
        // Replaying an acknowledged or ambiguous claim must return its current
        // identity, not compete for a fresh fence. Never adopt another boot.
        let previous = self.queue_sql(
            format!("SELECT json_object('job', json({JOB_JSON}), 'resources_current', {CLAIM_RESOURCE_SNAPSHOT}) AS result_json FROM background_jobs WHERE id = json_extract($1, '$.job_id') AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || json_extract($1, '$.node_id'))"),
            encode(&request)?, false, true,
        ).await?;
        let Some(previous) = previous.first() else {
            return Ok(ClaimOutcome::ExpiredOrPruned);
        };
        let snapshot: ClaimSnapshot = decode(previous)?;
        let previous = snapshot.job;
        if previous.payload_version != request.payload_version
            || previous
                .supported_payload()
                .map(|payload| payload.kind())
                .ok()
                != Some(request.kind)
        {
            return Ok(ClaimOutcome::Unsupported);
        }
        if let Some(token) = &previous.token {
            if token.claim_id == request.claim_id {
                return if snapshot.resources_current == 1
                    && previous.state == JobState::Running
                    && token.node_id == request.node_id
                    && token.boot_id == request.boot_id
                    && token.lease_expires_ms > request.now_ms
                    && (previous.retry_deadline_ms == 0
                        || previous.retry_deadline_ms > request.now_ms)
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
            "SELECT json_object('job', json({JOB_JSON}), 'resources_current', {CLAIM_RESOURCE_SNAPSHOT}) AS result_json FROM background_jobs WHERE id = json_extract($1, '$.job_id') AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || json_extract($1, '$.node_id'))
             AND EXISTS (SELECT 1 FROM background_job_attempts a WHERE a.job_id = background_jobs.id
               AND a.claim_id = json_extract($1, '$.claim_id')
               AND a.owner_node_id = json_extract($1, '$.node_id')
               AND a.owner_boot_id = json_extract($1, '$.boot_id')
               AND (json_extract($1, '$.fence') IS NULL OR a.fence = json_extract($1, '$.fence')))"
        ), encode(&request)?, false, true).await?;
        let Some(row) = rows.first() else {
            return Ok(ClaimResolution::ExpiredOrPruned);
        };
        let snapshot: ClaimSnapshot = decode(row)?;
        let job = snapshot.job;
        if job.state == JobState::Running && snapshot.resources_current != 1 {
            return Ok(ClaimResolution::LostOwnership);
        }
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
            if job.state == JobState::Running
                && job.retry_deadline_ms > 0
                && job.retry_deadline_ms <= request.now_ms
            {
                return Ok(ClaimResolution::LostOwnership);
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
        if attempt_fence != Some(job.fence) {
            return Ok(ClaimResolution::LostOwnership);
        }
        Ok(ClaimResolution::Settled {
            state: job.state,
            result_ref: job.result_ref,
        })
    }

    async fn job_labels(&self, ids: &[String]) -> Result<Vec<JobLabel>, StoreError> {
        if ids.len() > MAX_PAGE_SIZE || ids.iter().any(|id| uuid::Uuid::parse_str(id).is_err()) {
            return Err(invalid("invalid job label page"));
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.queue_sql(
            LABELS_SQL.into(),
            encode(&serde_json::json!({"ids": ids}))?,
            false,
            false,
        )
        .await?
        .iter()
        .map(|row| decode(row))
        .collect()
    }

    async fn job_counts(&self, now_ms: i64) -> Result<Vec<JobCount>, StoreError> {
        if now_ms < 0 {
            return Err(invalid("invalid queue observation time"));
        }
        self.queue_sql(
            COUNTS_SQL.into(),
            encode(&serde_json::json!({"now_ms": now_ms}))?,
            false,
            false,
        )
        .await?
        .iter()
        .map(|row| decode(row))
        .collect()
    }

    async fn source_io_holders(&self, now_ms: i64) -> Result<Vec<SourceIoHolder>, StoreError> {
        if now_ms < 0 {
            return Err(invalid("invalid queue observation time"));
        }
        self.queue_sql(
            SOURCE_IO_HOLDERS_SQL.into(),
            encode(&serde_json::json!({"now_ms": now_ms}))?,
            false,
            false,
        )
        .await?
        .iter()
        .map(|row| decode(row))
        .collect()
    }

    async fn job_attempts(&self, job_id: &str) -> Result<Vec<JobAttemptObservation>, StoreError> {
        if uuid::Uuid::parse_str(job_id).is_err() {
            return Err(invalid("invalid job identity"));
        }
        self.queue_sql(
            ATTEMPTS_SQL.into(),
            encode(&serde_json::json!({"job_id": job_id}))?,
            false,
            false,
        )
        .await?
        .iter()
        .map(|row| decode(row))
        .collect()
    }

    async fn list_jobs(&self, query: JobQuery) -> Result<JobPage, StoreError> {
        if query.limit == 0
            || query.limit > MAX_PAGE_SIZE
            || query
                .after_id
                .as_ref()
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
        {
            return Err(invalid("invalid background job page or cursor"));
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
