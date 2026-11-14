//! The claim predicate fragments (docs/cluster/CLAIM-LOCAL-VERDICT-BUILD.md).

use super::*;

/// `CLAIM_SQL` exactly as it stood before its WHERE clause was split into the
/// predicate fragments (M1). Kept while the fragments are new so the identity
/// test below proves the split changed no predicate, only how the text is
/// assembled.
const CLAIM_SQL_BEFORE_SPLIT: &str = concat!(
    r#"
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
  AND (kind!='dv_title_scan' OR EXISTS(SELECT 1 FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id
    WHERE d.revision=json_extract(background_jobs.payload_json,'$.probe_revision')
    AND d.file_id=json_extract(background_jobs.payload_json,'$.file_id') AND d.terminal=0
    AND d.scanner_digest=json_extract(background_jobs.payload_json,'$.scanner_digest')
    AND f.size=json_extract(background_jobs.payload_json,'$.source_size') AND f.mtime=json_extract(background_jobs.payload_json,'$.source_mtime')))
  AND (kind NOT IN ('subtitle_text_translate','subtitle_text_translate_artifact','subtitle_text_model_operation') OR ("#,
    text_admission!("payload_json", "json_extract($1,'$.now_ms')"),
    r#"))
  AND (kind NOT IN ('subtitle_translation_prepare','subtitle_translation_range') OR ("#,
    translation_admission!(
        "background_jobs.payload_json",
        "json_extract($1,'$.now_ms')",
        "background_jobs.id"
    ),
    r#"))
  AND (kind!='subtitle_transcribe_managed' OR ("#,
    subtitle_managed_admission!(),
    r#"))
  AND (kind!='subtitle_transcribe' OR ("#,
    subtitle_advanced_admission!(),
    r#"))
  AND (kind!='subtitle_transcribe_managed' OR EXISTS (SELECT 1 FROM files file WHERE file.id=json_extract(background_jobs.payload_json,'$.file_id') AND file.size=json_extract(background_jobs.payload_json,'$.source_size') AND file.mtime=json_extract(background_jobs.payload_json,'$.source_mtime') AND file.probe_json IS NOT NULL))
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
  AND (kind != 'subtitle_model_operation' OR NOT EXISTS (
    SELECT 1 FROM background_jobs other WHERE other.kind='subtitle_model_operation'
      AND other.id != background_jobs.id AND other.target_node_id=background_jobs.target_node_id
      AND other.state IN ('running','cancelling') AND other.lease_expires_ms > json_extract($1,'$.now_ms')))
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
  -- The outer CAS already fixes id to this request. Bind resource lookup to
  -- that same value so SQLite pushes the identity into every UNION branch
  -- of the resource view instead of scanning all jobs under the Raft writer.
  -- A lapsed custody counts as one ordinary reader below until the sweep
  -- releases it; it no longer refuses every source claim on its own.
  AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
    JOIN foreground_source_permissions fg ON required.resource_key LIKE 'source_io%'
      AND (fg.resource_key=required.resource_key OR fg.resource_key='source_io' OR required.resource_key='source_io')
    WHERE required.job_id=json_extract($1,'$.job_id') AND (fg.lease_expires_at_ms>json_extract($1,'$.now_ms') OR fg.custody_held=1)
      AND (fg.lease_expires_at_ms<=json_extract($1,'$.now_ms') OR COALESCE(fg.permits_through_ms,0)<=json_extract($1,'$.now_ms')))
  AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
    WHERE required.job_id = json_extract($1, '$.job_id') AND (SELECT COUNT(DISTINCT json_array(held.owner_kind,held.work_id,held.fence)) FROM source_io_occupancy held
      WHERE (held.resource_key=required.resource_key OR (held.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (held.resource_key='source_io' OR required.resource_key='source_io'))) AND held.active_occupancy=1
        AND (held.expires_at_ms>json_extract($1,'$.now_ms') OR held.custody_id IS NOT NULL)
        AND (held.work_id!=json_extract($1,'$.job_id') OR held.custody_id IS NOT NULL))>=2)
  -- A second source reader is reserved for live demand. Classification uses
  -- durable consumer ownership, never the job's caller-supplied priority.
  AND NOT EXISTS (SELECT 1 FROM background_job_required_resources required
    WHERE required.job_id = json_extract($1, '$.job_id')
      AND required.resource_key LIKE 'source_io%'
      AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
          WHERE interest.job_id = background_jobs.id
            AND interest.state IN ('pending','awaiting_hydration')
            AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract($1, '$.now_ms'))
            AND (interest.consumer_kind IN ('playback_fragment','playback_qualification')
              OR (interest.consumer_kind = 'background_delivery' AND EXISTS (
                  SELECT 1 FROM background_job_waiters viewer
                  WHERE viewer.job_id = interest.consumer_ref
                    AND viewer.target_node_id = interest.target_node_id
                    AND viewer.consumer_kind = 'playback_fragment'
                    AND viewer.state IN ('pending','awaiting_hydration')
                    AND viewer.deadline_ms > json_extract($1, '$.now_ms')))
              OR (interest.consumer_kind IN ('subtitle_translation','subtitle_translation_range') AND EXISTS (
                  SELECT 1 FROM background_jobs live_range WHERE live_range.id=interest.job_id
                    AND ((interest.consumer_kind='subtitle_translation_range' AND live_range.kind='subtitle_translation_range')
                      OR (interest.consumer_kind='subtitle_translation' AND live_range.kind='subtitle_translation_prepare')) AND ("#,
    translation_admission!(
        "live_range.payload_json",
        "json_extract($1,'$.now_ms')",
        "live_range.id"
    ),
    r#")))
              OR (interest.consumer_kind = 'subtitle_source' AND EXISTS (
                  SELECT 1 FROM analysis_requests request
                  WHERE request.request_id = interest.consumer_ref
                    AND request.component = 'subtitle_source'
                    AND request.trigger = 'playback'
                    AND request.state IN ('queued','running')))))
      AND ((SELECT COUNT(*) FROM background_job_reservations held
          WHERE (held.resource_key=required.resource_key OR (held.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (held.resource_key='source_io' OR required.resource_key='source_io')))
            AND held.expires_at_ms > json_extract($1, '$.now_ms')
            AND held.job_id != background_jobs.id
            AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
                WHERE interest.job_id = held.job_id
                  AND interest.state IN ('pending','awaiting_hydration')
                  AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract($1, '$.now_ms'))
                  AND (interest.consumer_kind IN ('playback_fragment','playback_qualification')
                    OR (interest.consumer_kind = 'background_delivery' AND EXISTS (
                        SELECT 1 FROM background_job_waiters viewer
                        WHERE viewer.job_id = interest.consumer_ref
                          AND viewer.target_node_id = interest.target_node_id
                          AND viewer.consumer_kind = 'playback_fragment'
                          AND viewer.state IN ('pending','awaiting_hydration')
                          AND viewer.deadline_ms > json_extract($1, '$.now_ms')))
                    OR (interest.consumer_kind IN ('subtitle_translation','subtitle_translation_range') AND EXISTS (
                        SELECT 1 FROM background_jobs live_range WHERE live_range.id=interest.job_id
                          AND ((interest.consumer_kind='subtitle_translation_range' AND live_range.kind='subtitle_translation_range')
                      OR (interest.consumer_kind='subtitle_translation' AND live_range.kind='subtitle_translation_prepare')) AND ("#,
    translation_admission!(
        "live_range.payload_json",
        "json_extract($1,'$.now_ms')",
        "live_range.id",
        "live_range.fence"
    ),
    r#")))
                    OR (interest.consumer_kind = 'subtitle_source' AND EXISTS (
                        SELECT 1 FROM analysis_requests request
                        WHERE request.request_id = interest.consumer_ref
                          AND request.component = 'subtitle_source'
                          AND request.trigger = 'playback'
                          AND request.state IN ('queued','running'))))))
        + (SELECT COUNT(*) FROM analysis_source_reservations held
            WHERE (held.resource_key=required.resource_key OR (held.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (held.resource_key='source_io' OR required.resource_key='source_io')))
              AND held.expires_at_ms > json_extract($1, '$.now_ms')
              AND NOT EXISTS (SELECT 1 FROM background_job_waiters viewer
                  WHERE viewer.request_scope = 'playback-analysis'
                    AND viewer.job_id = held.request_id
                    AND viewer.state = 'pending'
                    AND viewer.deadline_ms > json_extract($1, '$.now_ms')))
        + (SELECT COUNT(DISTINCT json_array(held.owner_kind,held.work_id,held.fence)) FROM source_io_occupancy held
          WHERE held.owner_kind IN ('lease','offline') AND held.active_occupancy=1
            AND (held.resource_key=required.resource_key OR held.resource_key='source_io' OR required.resource_key='source_io')
            AND (held.expires_at_ms>json_extract($1,'$.now_ms') OR held.custody_id IS NOT NULL)) >= 1))
"#
);

/// SQL text with whitespace made insignificant: runs collapse, and whitespace
/// next to punctuation disappears. Two identifiers are never joined, so a
/// dropped or added token still differs, and the newline that ends a `--`
/// comment is kept, so a predicate cannot slide into a comment unseen.
fn sql_tokens(sql: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(sql.len());
    let mut pending_space = false;
    let mut in_comment = false;
    for c in sql.chars() {
        if c == '\n' && in_comment {
            out.push('\n');
            in_comment = false;
            pending_space = false;
            continue;
        }
        if c == '-' && !pending_space && out.ends_with('-') {
            in_comment = true;
        }
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && out.chars().last().is_some_and(word) && word(c) {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    out
}

#[test]
fn claim_sql_assembled_from_fragments_is_the_statement_before_the_split() {
    assert_eq!(sql_tokens(CLAIM_SQL), sql_tokens(CLAIM_SQL_BEFORE_SPLIT));
    // The comparison itself must see a changed predicate.
    assert_ne!(
        sql_tokens(&CLAIM_SQL.replacen(">=2)", ">=3)", 1)),
        sql_tokens(CLAIM_SQL_BEFORE_SPLIT)
    );
    assert_ne!(sql_tokens("NOT EXISTS"), sql_tokens("NOTEXISTS"));
    assert_ne!(sql_tokens("-- note\n  AND x"), sql_tokens("-- note AND x"));
}
