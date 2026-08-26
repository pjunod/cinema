//! Cluster coordination for content-addressed VOD fragment indexes.
//!
//! Only small ownership and queue facts belong in the replicated store.  The
//! encoded index itself is deliberately returned to the daemon as an opaque,
//! checksummed blob and lives in a node-local or explicitly shared cache.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::fmp4::{CutClass, PromotionInputs};
use crate::segplan::{FragmentIndex, IndexRow, SourceIdentity, SEGPLAN_VERSION};

pub const CLUSTER_FRAGMENT_INDEX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cluster_fragment_index_sources (
    node_id          TEXT NOT NULL,
    file_id          INTEGER NOT NULL,
    object_version   TEXT NOT NULL,
    source_size      INTEGER NOT NULL,
    source_mtime     INTEGER NOT NULL,
    source_sha256    TEXT NOT NULL,
    observed_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (node_id, file_id)
) STRICT;

CREATE TABLE IF NOT EXISTS cluster_fragment_index_jobs (
    cache_key         TEXT PRIMARY KEY,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    source_sha256     TEXT NOT NULL,
    pipeline_sha256   TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
    owner_node_id     TEXT,
    fence             INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms  INTEGER,
    attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms     INTEGER NOT NULL,
    last_error_code   TEXT,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_jobs_due
    ON cluster_fragment_index_jobs(state, not_before_ms, created_at_ms, cache_key);

CREATE TABLE IF NOT EXISTS cluster_fragment_index_artifacts (
    cache_key         TEXT PRIMARY KEY,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    source_sha256     TEXT NOT NULL,
    pipeline_sha256   TEXT NOT NULL,
    blob_sha256       TEXT NOT NULL,
    bytes             INTEGER NOT NULL CHECK (bytes > 0),
    built_by_node_id  TEXT NOT NULL,
    built_at_ms       INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_artifacts_file
    ON cluster_fragment_index_artifacts(file_id, source_size, source_mtime, pipeline_sha256);

CREATE TABLE IF NOT EXISTS cluster_fragment_index_locations (
    cache_key         TEXT NOT NULL,
    node_id           TEXT NOT NULL,
    bytes             INTEGER NOT NULL CHECK (bytes > 0),
    verified_at_ms    INTEGER NOT NULL,
    last_seen_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (cache_key, node_id)
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_locations_node
    ON cluster_fragment_index_locations(node_id, last_seen_at_ms, cache_key);

CREATE TRIGGER IF NOT EXISTS cluster_fragment_indexes_cancel_source BEFORE DELETE ON files
BEGIN
    DELETE FROM cluster_fragment_index_sources WHERE file_id = OLD.id;
    UPDATE cluster_fragment_index_jobs
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           last_error_code = 'source_deleted'
     WHERE file_id = OLD.id AND state IN ('queued', 'running');
END;
"#;

pub const MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES: usize = 32 * 1024 * 1024;
const BLOB_MAGIC: &[u8; 8] = b"PLRXIDX2";
const BLOB_FORMAT_VERSION: u16 = 1;
const ROW_BYTES: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragmentIndexSourceObservation {
    pub node_id: String,
    pub file_id: i64,
    pub object_version: String,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewClusterFragmentIndexJob {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub not_before_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexJob {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub state: String,
    pub owner_node_id: String,
    pub fence: i64,
    pub lease_expires_ms: i64,
    pub attempts: i64,
    pub not_before_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexArtifact {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub blob_sha256: String,
    pub bytes: i64,
    pub built_by_node_id: String,
    pub built_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexLocation {
    pub cache_key: String,
    pub node_id: String,
    pub bytes: i64,
    pub verified_at_ms: i64,
    pub last_seen_at_ms: i64,
}

#[async_trait]
pub trait ClusterFragmentIndexStore: Send + Sync + 'static {
    async fn record_fragment_index_source(
        &self,
        observation: &FragmentIndexSourceObservation,
    ) -> Result<(), StoreError>;

    async fn fragment_index_source(
        &self,
        node_id: &str,
        file_id: i64,
        object_version: &str,
    ) -> Result<Option<FragmentIndexSourceObservation>, StoreError>;

    async fn cluster_fragment_index_artifact(
        &self,
        cache_key: &str,
    ) -> Result<Option<ClusterFragmentIndexArtifact>, StoreError>;

    async fn enqueue_cluster_fragment_index(
        &self,
        job: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError>;

    /// Reopen an existing catalog artifact whose physical holders could not
    /// supply a valid blob. The immutable artifact remains the expected
    /// identity while a fenced worker deterministically reconstructs it.
    async fn requeue_cluster_fragment_index(
        &self,
        cache_key: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn claim_cluster_fragment_index(
        &self,
        node_id: &str,
        excluded_cache_keys: &[String],
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError>;

    async fn renew_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Return a claim this node cannot execute without charging the shared
    /// retry budget. Used for node-local path/mount refusals; the worker stops
    /// its pass after yielding so it cannot immediately reclaim the same row.
    async fn yield_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn complete_cluster_fragment_index(
        &self,
        job: &ClusterFragmentIndexJob,
        artifact: &ClusterFragmentIndexArtifact,
        location: &ClusterFragmentIndexLocation,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    #[allow(clippy::too_many_arguments)]
    async fn fail_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        error_code: &str,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn put_cluster_fragment_index_location(
        &self,
        location: &ClusterFragmentIndexLocation,
    ) -> Result<(), StoreError>;

    async fn cluster_fragment_index_locations(
        &self,
        cache_key: &str,
    ) -> Result<Vec<ClusterFragmentIndexLocation>, StoreError>;

    async fn forget_cluster_fragment_index_location(
        &self,
        cache_key: &str,
        node_id: &str,
    ) -> Result<bool, StoreError>;

    /// Remove a bounded set of terminal catalog generations that have not
    /// had a verified holder within the retention window. Returns artifact
    /// keys whose node-local bytes may now be removed.
    async fn prune_cluster_fragment_indexes(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<Vec<String>, StoreError>;
}

/// Canonical cache identity.  Every component is length-delimited and the
/// source digest is over the complete file, so metadata aliases cannot collide.
pub fn cluster_fragment_index_key(source_sha256: &str, pipeline_sha256: &str) -> Option<String> {
    if !is_sha256(source_sha256) || !is_sha256(pipeline_sha256) {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/cache-key\0");
    digest.update(BLOB_FORMAT_VERSION.to_be_bytes());
    digest.update(SEGPLAN_VERSION.to_be_bytes());
    update_field(&mut digest, source_sha256.as_bytes());
    update_field(&mut digest, pipeline_sha256.as_bytes());
    Some(hex::encode(digest.finalize()))
}

/// Cryptographic digest of a pipeline vector. The daemon passes the exact
/// video-deciding argv plus its engine digest; source path is intentionally
/// excluded because the complete source digest already names the bytes.
pub fn cluster_fragment_index_pipeline_digest(
    engine_sha256: &str,
    args: &[String],
) -> Option<String> {
    if !is_sha256(engine_sha256) {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/pipeline\0");
    digest.update(SEGPLAN_VERSION.to_be_bytes());
    update_field(&mut digest, engine_sha256.as_bytes());
    for arg in args {
        update_field(&mut digest, arg.as_bytes());
    }
    Some(hex::encode(digest.finalize()))
}

fn update_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Serialize, Deserialize)]
struct BlobHeader {
    segplan_version: u32,
    timescale: u32,
    init_sha256: String,
    promotion: PromotionInputs,
    parameter_sets_constant: bool,
    source: SourceIdentity,
    source_sha256: String,
    pipeline_sha256: String,
    rows: usize,
}

pub fn encode_cluster_fragment_index_blob(
    index: &FragmentIndex,
    source_sha256: &str,
    pipeline_sha256: &str,
) -> Result<Vec<u8>, StoreError> {
    if index.version != SEGPLAN_VERSION
        || index.timescale == 0
        || index.rows.is_empty()
        || !is_sha256(source_sha256)
        || !is_sha256(pipeline_sha256)
    {
        return Err(StoreError::Task(
            "invalid cluster fragment-index artifact".to_owned(),
        ));
    }
    let header = serde_json::to_vec(&BlobHeader {
        segplan_version: index.version,
        timescale: index.timescale,
        init_sha256: index.init_sha256.clone(),
        promotion: index.promotion.clone(),
        parameter_sets_constant: index.parameter_sets_constant,
        source: index.source.clone(),
        source_sha256: source_sha256.to_ascii_lowercase(),
        pipeline_sha256: pipeline_sha256.to_ascii_lowercase(),
        rows: index.rows.len(),
    })
    .map_err(|error| StoreError::Task(format!("encoding fragment-index header: {error}")))?;
    let rows_bytes = index
        .rows
        .len()
        .checked_mul(ROW_BYTES)
        .ok_or_else(|| StoreError::Task("fragment-index row size overflow".to_owned()))?;
    let capacity = 8_usize
        .checked_add(2 + 4)
        .and_then(|bytes| bytes.checked_add(header.len()))
        .and_then(|bytes| bytes.checked_add(rows_bytes))
        .ok_or_else(|| StoreError::Task("fragment-index blob size overflow".to_owned()))?;
    if capacity > MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES {
        return Err(StoreError::Task(
            "fragment-index blob exceeds hard limit".to_owned(),
        ));
    }
    let mut blob = Vec::with_capacity(capacity);
    blob.extend_from_slice(BLOB_MAGIC);
    blob.extend_from_slice(&BLOB_FORMAT_VERSION.to_be_bytes());
    blob.extend_from_slice(
        &u32::try_from(header.len())
            .map_err(|_| StoreError::Task("fragment-index header is too large".to_owned()))?
            .to_be_bytes(),
    );
    blob.extend_from_slice(&header);
    for row in &index.rows {
        blob.extend_from_slice(&row.dts.to_le_bytes());
        blob.extend_from_slice(
            &u32::try_from(row.duration)
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        blob.extend_from_slice(&row.bytes.to_le_bytes());
        blob.extend_from_slice(&row.video_bytes.to_le_bytes());
        blob.push(class_code(row.class));
        blob.extend_from_slice(&[0, 0, 0]);
    }
    Ok(blob)
}

pub fn decode_cluster_fragment_index_blob(
    blob: &[u8],
    expected_source_sha256: &str,
    expected_pipeline_sha256: &str,
) -> Result<FragmentIndex, StoreError> {
    if blob.len() > MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES || blob.len() < 14 {
        return Err(StoreError::Task(
            "invalid fragment-index blob size".to_owned(),
        ));
    }
    if &blob[..8] != BLOB_MAGIC {
        return Err(StoreError::Task(
            "invalid fragment-index blob magic".to_owned(),
        ));
    }
    let version = u16::from_be_bytes(blob[8..10].try_into().expect("two bytes"));
    if version != BLOB_FORMAT_VERSION {
        return Err(StoreError::Task(format!(
            "unsupported fragment-index blob version {version}"
        )));
    }
    let header_len = u32::from_be_bytes(blob[10..14].try_into().expect("four bytes")) as usize;
    let rows_offset = 14_usize
        .checked_add(header_len)
        .filter(|offset| *offset <= blob.len())
        .ok_or_else(|| StoreError::Task("truncated fragment-index blob header".to_owned()))?;
    let header: BlobHeader = serde_json::from_slice(&blob[14..rows_offset])
        .map_err(|error| StoreError::Task(format!("decoding fragment-index header: {error}")))?;
    if header.segplan_version != SEGPLAN_VERSION
        || header.timescale == 0
        || header.rows == 0
        || !header
            .source_sha256
            .eq_ignore_ascii_case(expected_source_sha256)
        || !header
            .pipeline_sha256
            .eq_ignore_ascii_case(expected_pipeline_sha256)
    {
        return Err(StoreError::Task(
            "fragment-index blob identity mismatch".to_owned(),
        ));
    }
    let packed = &blob[rows_offset..];
    if packed.len() != header.rows.saturating_mul(ROW_BYTES) {
        return Err(StoreError::Task(
            "fragment-index blob row count mismatch".to_owned(),
        ));
    }
    let mut rows = Vec::with_capacity(header.rows);
    for chunk in packed.chunks_exact(ROW_BYTES) {
        let class = class_from_code(chunk[20]).ok_or_else(|| {
            StoreError::Task(format!("fragment-index blob has cut class {}", chunk[20]))
        })?;
        rows.push(IndexRow {
            dts: u64::from_le_bytes(chunk[0..8].try_into().expect("eight bytes")),
            duration: u64::from(u32::from_le_bytes(
                chunk[8..12].try_into().expect("four bytes"),
            )),
            bytes: u32::from_le_bytes(chunk[12..16].try_into().expect("four bytes")),
            video_bytes: u32::from_le_bytes(chunk[16..20].try_into().expect("four bytes")),
            class,
        });
    }
    let mut index = FragmentIndex::new(header.timescale, rows, header.init_sha256, header.source);
    index.promotion = header.promotion;
    index.parameter_sets_constant = header.parameter_sets_constant;
    Ok(index)
}

pub fn cluster_fragment_index_blob_sha256(blob: &[u8]) -> String {
    hex::encode(Sha256::digest(blob))
}

fn class_code(class: CutClass) -> u8 {
    match class {
        CutClass::CleanIdr => 1,
        CutClass::CleanCra => 2,
        CutClass::Dirty => 3,
        CutClass::Unparseable => 4,
    }
}

fn class_from_code(code: u8) -> Option<CutClass> {
    match code {
        1 => Some(CutClass::CleanIdr),
        2 => Some(CutClass::CleanCra),
        3 => Some(CutClass::Dirty),
        4 => Some(CutClass::Unparseable),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    #[test]
    fn cache_key_is_domain_separated_and_exact() {
        let a = cluster_fragment_index_key(&digest('a'), &digest('b')).expect("valid key");
        let b = cluster_fragment_index_key(&digest('b'), &digest('a')).expect("valid key");
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(cluster_fragment_index_key("not-a-digest", &digest('b')).is_none());
    }

    #[test]
    fn blob_round_trips_and_rejects_identity_aliases() {
        let mut index = FragmentIndex::new(
            90_000,
            vec![IndexRow {
                dts: 12,
                duration: 3_003,
                bytes: 45,
                video_bytes: 31,
                class: CutClass::CleanIdr,
            }],
            digest('c'),
            SourceIdentity::new(123, 456, "argv"),
        );
        index.parameter_sets_constant = true;
        let source = digest('a');
        let pipeline = digest('b');
        let blob = encode_cluster_fragment_index_blob(&index, &source, &pipeline).expect("encode");
        assert_eq!(
            decode_cluster_fragment_index_blob(&blob, &source, &pipeline).expect("decode"),
            index
        );
        assert!(decode_cluster_fragment_index_blob(&blob, &digest('d'), &pipeline).is_err());
    }
}
