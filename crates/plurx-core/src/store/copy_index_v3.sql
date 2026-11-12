-- Full-file receipts and output-contract variants, not sampled hashes.
-- Source/grant/JobToken authority is established only by guarded Store APIs.
CREATE TABLE IF NOT EXISTS copy_index_source_attestations (
    origin_node_id TEXT NOT NULL CHECK(length(CAST(origin_node_id AS BLOB)) BETWEEN 1 AND 256 AND instr(origin_node_id,char(0))=0 AND origin_node_id NOT GLOB '*[^A-Za-z0-9._:-]*'),
    file_id INTEGER NOT NULL CHECK(file_id>0),
    object_version TEXT NOT NULL CHECK(length(CAST(object_version AS BLOB)) BETWEEN 1 AND 4096),
    source_size INTEGER NOT NULL CHECK(source_size>=0),
    source_mtime INTEGER NOT NULL,
    source_sha256 TEXT NOT NULL CHECK(length(source_sha256)=64 AND length(CAST(source_sha256 AS BLOB))=64 AND source_sha256 NOT GLOB '*[^0-9a-f]*'),
    regime_revision INTEGER NOT NULL CHECK(regime_revision=1),
    completed_at_ms INTEGER NOT NULL CHECK(completed_at_ms>=0),
    analysis_request_id TEXT NOT NULL CHECK(length(CAST(analysis_request_id AS BLOB)) BETWEEN 1 AND 128),
    analysis_fence INTEGER NOT NULL CHECK(analysis_fence>0),
    phase_sequence INTEGER NOT NULL CHECK(phase_sequence>0),
    custody_id TEXT NOT NULL CHECK(length(CAST(custody_id AS BLOB)) BETWEEN 1 AND 128),
    PRIMARY KEY(origin_node_id,file_id,object_version)
) STRICT;
-- next statement
-- Variant identity is the blob digest; multiple engines/raw init metadata may
-- produce distinct blobs with the same semantic output-contract key.
CREATE TABLE IF NOT EXISTS copy_index_v3_artifacts (
    blob_sha256 TEXT NOT NULL CHECK(length(blob_sha256)=64 AND length(CAST(blob_sha256 AS BLOB))=64 AND blob_sha256 NOT GLOB '*[^0-9a-f]*'),
    contract_key TEXT NOT NULL CHECK(length(contract_key)=64 AND length(CAST(contract_key AS BLOB))=64 AND contract_key NOT GLOB '*[^0-9a-f]*'),
    catalog_namespace TEXT NOT NULL CHECK(length(CAST(catalog_namespace AS BLOB)) BETWEEN 1 AND 256 AND length(CAST(trim(catalog_namespace) AS BLOB))>0),
    file_id INTEGER NOT NULL CHECK(file_id>0),
    source_sha256 TEXT NOT NULL CHECK(length(source_sha256)=64 AND length(CAST(source_sha256 AS BLOB))=64 AND source_sha256 NOT GLOB '*[^0-9a-f]*'),
    contract_version INTEGER NOT NULL CHECK(contract_version>0),
    output_contract_sha256 TEXT NOT NULL CHECK(length(output_contract_sha256)=64 AND length(CAST(output_contract_sha256 AS BLOB))=64 AND output_contract_sha256 NOT GLOB '*[^0-9a-f]*'),
    init_contract_sha256 TEXT NOT NULL CHECK(length(init_contract_sha256)=64 AND length(CAST(init_contract_sha256 AS BLOB))=64 AND init_contract_sha256 NOT GLOB '*[^0-9a-f]*'),
    bytes INTEGER NOT NULL CHECK(bytes BETWEEN 1 AND 33554432),
    built_by_node_id TEXT NOT NULL CHECK(length(CAST(built_by_node_id AS BLOB)) BETWEEN 1 AND 256 AND instr(built_by_node_id,char(0))=0 AND built_by_node_id NOT GLOB '*[^A-Za-z0-9._:-]*'),
    built_at_ms INTEGER NOT NULL CHECK(built_at_ms>=0),
    producer_engine_sha256 TEXT NOT NULL CHECK(length(producer_engine_sha256)=64 AND length(CAST(producer_engine_sha256 AS BLOB))=64 AND producer_engine_sha256 NOT GLOB '*[^0-9a-f]*'),
    PRIMARY KEY(contract_key,blob_sha256)
) STRICT;
-- next statement
CREATE TRIGGER IF NOT EXISTS copy_index_v3_artifacts_immutable
BEFORE UPDATE ON copy_index_v3_artifacts
BEGIN SELECT RAISE(ABORT,'copy index v3 artifact variants are immutable'); END;
-- next statement
CREATE TABLE IF NOT EXISTS copy_index_v3_heads (
    source_key TEXT PRIMARY KEY CHECK(length(source_key)=64 AND length(CAST(source_key AS BLOB))=64 AND source_key NOT GLOB '*[^0-9a-f]*'),
    catalog_namespace TEXT NOT NULL CHECK(length(CAST(catalog_namespace AS BLOB)) BETWEEN 1 AND 256 AND length(CAST(trim(catalog_namespace) AS BLOB))>0),
    file_id INTEGER NOT NULL CHECK(file_id>0),
    origin_node_id TEXT NOT NULL CHECK(length(CAST(origin_node_id AS BLOB)) BETWEEN 1 AND 256 AND instr(origin_node_id,char(0))=0 AND origin_node_id NOT GLOB '*[^A-Za-z0-9._:-]*'),
    object_version TEXT NOT NULL CHECK(length(CAST(object_version AS BLOB)) BETWEEN 1 AND 4096),
    source_size INTEGER NOT NULL CHECK(source_size>=0),
    source_mtime INTEGER NOT NULL,
    raw_video_identity TEXT NOT NULL CHECK(length(CAST(raw_video_identity AS BLOB)) BETWEEN 1 AND 96 AND length(raw_video_identity)=length(CAST(raw_video_identity AS BLOB)) AND raw_video_identity NOT GLOB '*[^0-9a-fA-F]*'),
    contract_version INTEGER NOT NULL CHECK(contract_version>0),
    desired_generation TEXT NOT NULL CHECK(length(CAST(desired_generation AS BLOB)) BETWEEN 1 AND 128),
    head_epoch INTEGER NOT NULL DEFAULT 0 CHECK(head_epoch>=0),
    state TEXT NOT NULL CHECK(state IN ('pending','healthy','quarantined')),
    contract_key TEXT CHECK(contract_key IS NULL OR (length(contract_key)=64 AND length(CAST(contract_key AS BLOB))=64 AND contract_key NOT GLOB '*[^0-9a-f]*')),
    blob_sha256 TEXT CHECK(blob_sha256 IS NULL OR (length(blob_sha256)=64 AND length(CAST(blob_sha256 AS BLOB))=64 AND blob_sha256 NOT GLOB '*[^0-9a-f]*')),
    published_generation TEXT CHECK(published_generation IS NULL OR (length(CAST(published_generation AS BLOB)) BETWEEN 1 AND 128)),
    publisher_job_id TEXT CHECK(publisher_job_id IS NULL OR (length(CAST(publisher_job_id AS BLOB)) BETWEEN 1 AND 128)),
    publisher_fence INTEGER CHECK(publisher_fence IS NULL OR publisher_fence>0),
    analysis_request_id TEXT CHECK(analysis_request_id IS NULL OR (length(CAST(analysis_request_id AS BLOB)) BETWEEN 1 AND 128)),
    analysis_fence INTEGER CHECK(analysis_fence IS NULL OR analysis_fence>0),
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms>=0),
    CHECK(state!='healthy' OR (contract_key IS NOT NULL AND blob_sha256 IS NOT NULL)),
    CHECK((publisher_job_id IS NULL)=(publisher_fence IS NULL)),
    CHECK((analysis_request_id IS NULL)=(analysis_fence IS NULL))
) STRICT;
-- next statement
CREATE TABLE IF NOT EXISTS copy_index_v3_quarantines (
    source_key TEXT NOT NULL CHECK(length(source_key)=64 AND length(CAST(source_key AS BLOB))=64 AND source_key NOT GLOB '*[^0-9a-f]*'),
    head_epoch INTEGER NOT NULL CHECK(head_epoch>=0),
    bad_blob_sha256 TEXT NOT NULL CHECK(length(bad_blob_sha256)=64 AND length(CAST(bad_blob_sha256 AS BLOB))=64 AND bad_blob_sha256 NOT GLOB '*[^0-9a-f]*'),
    successor_generation TEXT NOT NULL CHECK(length(successor_generation)=64 AND length(CAST(successor_generation AS BLOB))=64 AND successor_generation NOT GLOB '*[^0-9a-f]*'),
    code TEXT NOT NULL CHECK(length(CAST(code AS BLOB)) BETWEEN 1 AND 64 AND instr(code,char(0))=0 AND code NOT GLOB '*[^A-Za-z0-9._:-]*'),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms>=0),
    PRIMARY KEY(source_key,head_epoch,bad_blob_sha256)
) STRICT;
-- next statement
-- The actual verified holder of one variant's bytes. Builder provenance on the
-- artifact row is history, never a location; only a publisher or a hydrating
-- node that verified the exact blob may appear here.
CREATE TABLE IF NOT EXISTS copy_index_v3_locations (
    contract_key TEXT NOT NULL CHECK(length(contract_key)=64 AND length(CAST(contract_key AS BLOB))=64 AND contract_key NOT GLOB '*[^0-9a-f]*'),
    blob_sha256 TEXT NOT NULL CHECK(length(blob_sha256)=64 AND length(CAST(blob_sha256 AS BLOB))=64 AND blob_sha256 NOT GLOB '*[^0-9a-f]*'),
    node_id TEXT NOT NULL CHECK(length(CAST(node_id AS BLOB)) BETWEEN 1 AND 256 AND instr(node_id,char(0))=0 AND node_id NOT GLOB '*[^A-Za-z0-9._:-]*'),
    bytes INTEGER NOT NULL CHECK(bytes BETWEEN 1 AND 33554432),
    verified_at_ms INTEGER NOT NULL CHECK(verified_at_ms>=0),
    last_seen_at_ms INTEGER NOT NULL CHECK(last_seen_at_ms>=verified_at_ms),
    PRIMARY KEY(contract_key,blob_sha256,node_id)
) STRICT;
-- next statement
-- One atomic publication. The guarded INSERT (copy_index_v3.rs) classifies the
-- attempt; this trigger applies only a 'published' outcome and settles the
-- worker, Analysis and waiters together. No physical custody is touched here:
-- the metadata owner already finished every phase before publishing.
CREATE TRIGGER IF NOT EXISTS copy_index_v3_publish_command
AFTER INSERT ON background_job_commands WHEN NEW.operation='publish_copy_index_v3'
BEGIN
    INSERT INTO copy_index_v3_artifacts(blob_sha256,contract_key,catalog_namespace,file_id,source_sha256,contract_version,output_contract_sha256,init_contract_sha256,bytes,built_by_node_id,built_at_ms,producer_engine_sha256)
    SELECT json_extract(NEW.request_json,'$.artifact.blob_sha256'),json_extract(NEW.request_json,'$.artifact.contract_key'),
        json_extract(NEW.request_json,'$.artifact.catalog_namespace'),json_extract(NEW.request_json,'$.artifact.file_id'),
        json_extract(NEW.request_json,'$.artifact.source_sha256'),json_extract(NEW.request_json,'$.artifact.contract_version'),
        json_extract(NEW.request_json,'$.artifact.output_contract_sha256'),json_extract(NEW.request_json,'$.artifact.init_contract_sha256'),
        json_extract(NEW.request_json,'$.artifact.bytes'),json_extract(NEW.request_json,'$.artifact.built_by_node_id'),
        json_extract(NEW.request_json,'$.artifact.built_at_ms'),json_extract(NEW.request_json,'$.artifact.producer_engine_sha256')
    WHERE json_extract(NEW.result_json,'$.outcome')='published'
    ON CONFLICT(contract_key,blob_sha256) DO NOTHING;

    -- Only the candidate the publisher verified locally is a location; a
    -- retained healthy head's older blob is never claimed on its behalf.
    INSERT INTO copy_index_v3_locations(contract_key,blob_sha256,node_id,bytes,verified_at_ms,last_seen_at_ms)
    SELECT json_extract(NEW.request_json,'$.artifact.contract_key'),json_extract(NEW.request_json,'$.artifact.blob_sha256'),
        json_extract(NEW.request_json,'$.token.node_id'),json_extract(NEW.request_json,'$.artifact.bytes'),
        json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms')
    WHERE json_extract(NEW.result_json,'$.outcome')='published'
    ON CONFLICT(contract_key,blob_sha256,node_id) DO UPDATE SET bytes=excluded.bytes,
        verified_at_ms=excluded.verified_at_ms,last_seen_at_ms=excluded.last_seen_at_ms;

    -- A pending or quarantined head adopts the candidate at the next epoch. A
    -- healthy same-contract head keeps its first variant and epoch unchanged.
    UPDATE copy_index_v3_heads SET state='healthy',
        contract_key=json_extract(NEW.request_json,'$.artifact.contract_key'),
        blob_sha256=json_extract(NEW.request_json,'$.artifact.blob_sha256'),
        head_epoch=head_epoch+1,published_generation=desired_generation,
        publisher_job_id=json_extract(NEW.request_json,'$.token.job_id'),
        publisher_fence=json_extract(NEW.request_json,'$.token.fence'),
        analysis_request_id=json_extract(NEW.request_json,'$.claim.request_id'),
        analysis_fence=json_extract(NEW.request_json,'$.claim.fence'),
        updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE source_key=json_extract(NEW.request_json,'$.source_key')
        AND state IN ('pending','quarantined')
        AND head_epoch=json_extract(NEW.request_json,'$.expected_head_epoch')
        AND desired_generation=json_extract(NEW.request_json,'$.source.requested_generation')
        AND json_extract(NEW.result_json,'$.outcome')='published';

    -- The pointer the result_ref names is exactly the one now stored, or the
    -- whole publication rolls back.
    SELECT RAISE(ABORT,'copy index v3 head publication postcondition')
    WHERE json_extract(NEW.result_json,'$.outcome')='published' AND NOT EXISTS(
        SELECT 1 FROM copy_index_v3_heads head
        WHERE head.source_key=json_extract(NEW.request_json,'$.source_key') AND head.state='healthy'
          AND head.contract_key=json_extract(json_extract(NEW.result_json,'$.result_ref'),'$.head.contract_key')
          AND head.blob_sha256=json_extract(json_extract(NEW.result_json,'$.result_ref'),'$.head.blob_sha256')
          AND head.head_epoch=json_extract(json_extract(NEW.result_json,'$.result_ref'),'$.head.head_epoch')
          AND head.published_generation=json_extract(NEW.request_json,'$.source.requested_generation')
          AND EXISTS(SELECT 1 FROM copy_index_v3_artifacts artifact WHERE artifact.contract_key=head.contract_key
              AND artifact.blob_sha256=head.blob_sha256));

    UPDATE analysis_attempts SET phase='published',terminal_code=NULL,
        phase_updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE request_id=json_extract(NEW.request_json,'$.claim.request_id')
        AND claim_epoch=json_extract(NEW.request_json,'$.claim.fence')
        AND json_extract(NEW.result_json,'$.outcome')='published';
    UPDATE analysis_requests SET state='ready',owner_node_id=NULL,lease_expires_ms=NULL,
        result_cache_key=json_extract(NEW.request_json,'$.artifact.contract_key'),last_error_code=NULL,
        updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE request_id=json_extract(NEW.request_json,'$.claim.request_id')
        AND owner_node_id=json_extract(NEW.request_json,'$.claim.node_id')
        AND fence=json_extract(NEW.request_json,'$.claim.fence') AND state='running'
        AND json_extract(NEW.result_json,'$.outcome')='published';

    UPDATE background_jobs SET state='succeeded',result_ref=json_extract(NEW.result_json,'$.result_ref'),
        owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL,
        revision=revision+1,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE id=json_extract(NEW.result_json,'$.job_id') AND json_extract(NEW.result_json,'$.outcome')='published';

    UPDATE background_job_waiters SET state=CASE
        WHEN deadline_ms<=json_extract(NEW.request_json,'$.now_ms') THEN 'cancelled'
        WHEN retry_deadline_ms>0 AND retry_deadline_ms<=json_extract(NEW.request_json,'$.now_ms') THEN 'failed'
        WHEN target_node_id IS NULL OR target_node_id=json_extract(NEW.request_json,'$.token.node_id') THEN 'succeeded'
        ELSE 'awaiting_hydration' END,
        last_error_code=CASE WHEN retry_deadline_ms>0 AND retry_deadline_ms<=json_extract(NEW.request_json,'$.now_ms')
            THEN 'index_retry_window_expired' ELSE last_error_code END,
        result_ref=json_extract(NEW.result_json,'$.result_ref'),updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE job_id=json_extract(NEW.result_json,'$.job_id') AND state='pending'
        AND json_extract(NEW.result_json,'$.outcome')='published';
    -- Reproject after waiter settlement, as the fragment publisher does.
    UPDATE background_jobs SET priority=priority
    WHERE id=json_extract(NEW.result_json,'$.job_id') AND json_extract(NEW.result_json,'$.outcome')='published';
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
