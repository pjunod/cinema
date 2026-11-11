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
