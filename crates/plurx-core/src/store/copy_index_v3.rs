//! Mechanical v3 records. Deserialization and validation establish shape only;
//! guarded Store transactions must establish full-source/phase/JobToken proof.
//! Engine provenance and requested generation never identify a source head.

use super::analysis_coordination::AnalysisCoordinationSource;
use super::background_jobs::{digest, identifier};
use super::fragment_index_cluster::{
    analysis_request_id_is_valid, MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES,
};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn invalid() -> StoreError {
    StoreError::Task("invalid copy index v3 metadata".into())
}
fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max
}
fn namespace(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256
}
fn field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// File/witness/video/contract identity in the origin namespace. Generation is
/// validated as part of its source DTO but deliberately omitted from the key.
pub fn copy_index_v3_source_key(
    catalog_namespace: &str,
    source: &AnalysisCoordinationSource,
) -> Result<String, StoreError> {
    if !namespace(catalog_namespace) || !source.valid() {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    hash.update(b"plurx/copy-index/source-key/v3\0");
    field(&mut hash, catalog_namespace.as_bytes());
    field(&mut hash, source.origin_node_id.as_bytes());
    hash.update(source.file_id.to_be_bytes());
    field(&mut hash, source.object_version.as_bytes());
    hash.update(source.source_size.to_be_bytes());
    hash.update(source.source_mtime.to_be_bytes());
    field(&mut hash, source.video_identity.as_bytes());
    hash.update(source.contract_version.to_be_bytes());
    Ok(hex::encode(hash.finalize()))
}

/// The quarantine code of a healthy head whose bytes no node holds any more.
/// It says the bytes were lost, never that they were wrong: the rebuild of an
/// unchanged source seals the same bytes again and must be able to publish
/// them.
pub const COPY_INDEX_V3_NO_HOLDER_CODE: &str = "copy_index_no_holder";

/// The quarantine code of a healthy head that fresh observations of its
/// source contradicted. The rebuild seals from a set that still holds those
/// rows, so it cannot produce the quarantined bytes while they stand; when a
/// later rebuild does seal identical bytes, the contradicting observation
/// was the faulty side and the index was good all along.
pub const COPY_INDEX_V3_CONTRADICTION_CODE: &str = "copy_observation_contradiction";

/// The quarantine code of a healthy head whose sealed init its own producer
/// engine did not reproduce. One generation's init is one observation, like
/// a contradicting row: an identical reseal says the index was right.
pub const COPY_INDEX_V3_SEALED_INIT_CODE: &str = "copy_index_sealed_init_mismatch";

/// The codes that record a doubt about a head without proving its bytes
/// wrong. A blob quarantined under one of them may be published again; any
/// other code bans the blob it quarantined for this source.
const NON_BANNING_CODES: [&str; 3] = [
    COPY_INDEX_V3_NO_HOLDER_CODE,
    COPY_INDEX_V3_CONTRADICTION_CODE,
    COPY_INDEX_V3_SEALED_INIT_CODE,
];

/// Holders a consumer may count or ask: not a node the replicated removal
/// fence has ejected from the cluster. `alias` is the location's table alias.
fn holder_is_member(alias: &str) -> String {
    format!("NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||{alias}.node_id)")
}

/// All viewers observing the same bad head converge on one repair generation.
pub fn copy_index_v3_rebuild_generation(
    source_key: &str,
    bad_blob_sha256: &str,
    head_epoch: i64,
) -> Result<String, StoreError> {
    if !digest(source_key) || !digest(bad_blob_sha256) || head_epoch < 0 {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    hash.update(b"plurx/copy-index/quarantine-generation/v3\0");
    field(&mut hash, source_key.as_bytes());
    field(&mut hash, bad_blob_sha256.as_bytes());
    hash.update(head_epoch.to_be_bytes());
    Ok(hex::encode(hash.finalize()))
}

/// A full-file hash receipt, never a sample hash. Shape validation does not
/// mint the physical custody/phase witness required to insert this record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyIndexSourceAttestation {
    pub origin_node_id: String,
    pub file_id: i64,
    pub object_version: String,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub regime_revision: u32,
    pub completed_at_ms: i64,
    pub analysis_request_id: String,
    pub analysis_fence: i64,
    pub phase_sequence: i64,
    pub custody_id: String,
}
impl CopyIndexSourceAttestation {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !identifier(&self.origin_node_id)
            || self.file_id <= 0
            || !bounded(&self.object_version, 4096)
            || self.source_size < 0
            || !digest(&self.source_sha256)
            || self.regime_revision != 1
            || self.completed_at_ms < 0
            || !analysis_request_id_is_valid(&self.analysis_request_id)
            || self.analysis_fence <= 0
            || self.phase_sequence <= 0
            || uuid::Uuid::parse_str(&self.custody_id).is_err()
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Immutable logical-contract/blob association. Identical bytes may belong to
/// multiple files, and a semantic contract key can have several blob variants;
/// raw init/engine metadata differences do not replace an existing healthy head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyIndexV3Artifact {
    pub blob_sha256: String,
    pub contract_key: String,
    pub catalog_namespace: String,
    pub file_id: i64,
    pub source_sha256: String,
    pub contract_version: u32,
    pub output_contract_sha256: String,
    pub init_contract_sha256: String,
    pub bytes: i64,
    pub built_by_node_id: String,
    pub built_at_ms: i64,
    pub producer_engine_sha256: String,
}
impl CopyIndexV3Artifact {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !digest(&self.blob_sha256)
            || !digest(&self.contract_key)
            || !namespace(&self.catalog_namespace)
            || self.file_id <= 0
            || !digest(&self.source_sha256)
            || self.contract_version == 0
            || !digest(&self.output_contract_sha256)
            || !digest(&self.init_contract_sha256)
            || !(1..=MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES as i64).contains(&self.bytes)
            || !identifier(&self.built_by_node_id)
            || self.built_at_ms < 0
            || !digest(&self.producer_engine_sha256)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyIndexV3HeadState {
    Pending,
    Healthy,
    Quarantined,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyIndexV3Head {
    pub source_key: String,
    pub catalog_namespace: String,
    pub file_id: i64,
    pub origin_node_id: String,
    pub object_version: String,
    pub source_size: i64,
    pub source_mtime: i64,
    pub raw_video_identity: String,
    pub contract_version: u32,
    pub desired_generation: String,
    pub head_epoch: i64,
    pub state: CopyIndexV3HeadState,
    pub contract_key: Option<String>,
    pub blob_sha256: Option<String>,
    pub published_generation: Option<String>,
    pub publisher_job_id: Option<String>,
    pub publisher_fence: Option<i64>,
    pub analysis_request_id: Option<String>,
    pub analysis_fence: Option<i64>,
    pub updated_at_ms: i64,
}
impl CopyIndexV3Head {
    pub fn validate(&self) -> Result<(), StoreError> {
        let source = AnalysisCoordinationSource {
            file_id: self.file_id,
            source_size: self.source_size,
            source_mtime: self.source_mtime,
            origin_node_id: self.origin_node_id.clone(),
            object_version: self.object_version.clone(),
            video_identity: self.raw_video_identity.clone(),
            contract_version: self.contract_version,
            requested_generation: self.desired_generation.clone(),
        };
        if self.source_key != copy_index_v3_source_key(&self.catalog_namespace, &source)?
            || self.head_epoch < 0
            || self.updated_at_ms < 0
            || self
                .contract_key
                .as_ref()
                .is_some_and(|value| !digest(value))
            || self
                .blob_sha256
                .as_ref()
                .is_some_and(|value| !digest(value))
            || self
                .published_generation
                .as_ref()
                .is_some_and(|value| !bounded(value, 128))
            || self
                .publisher_job_id
                .as_ref()
                .is_some_and(|value| uuid::Uuid::parse_str(value).is_err())
            || self.publisher_fence.is_some_and(|fence| fence <= 0)
            || self
                .analysis_request_id
                .as_ref()
                .is_some_and(|value| !analysis_request_id_is_valid(value))
            || self.analysis_fence.is_some_and(|fence| fence <= 0)
            || self.publisher_job_id.is_some() != self.publisher_fence.is_some()
            || self.analysis_request_id.is_some() != self.analysis_fence.is_some()
            || (self.state == CopyIndexV3HeadState::Healthy
                && (self.contract_key.is_none() || self.blob_sha256.is_none()))
        {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyIndexV3Quarantine {
    pub source_key: String,
    pub head_epoch: i64,
    pub bad_blob_sha256: String,
    pub successor_generation: String,
    pub code: String,
    pub created_at_ms: i64,
}
impl CopyIndexV3Quarantine {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.successor_generation
            != copy_index_v3_rebuild_generation(
                &self.source_key,
                &self.bad_blob_sha256,
                self.head_epoch,
            )?
            || !identifier(&self.code)
            || self.code.len() > 64
            || self.created_at_ms < 0
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Metadata-only repair request. It cannot mint an Analysis claim or grant
/// source reads. The claim must already own the deterministic repair generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantineCopyIndexV3Head {
    pub catalog_namespace: String,
    pub source: AnalysisCoordinationSource,
    pub claim: super::analysis_coordination::AnalysisCoordinationClaim,
    pub expected_epoch: i64,
    pub bad_blob_sha256: String,
    pub code: String,
    pub now_ms: i64,
}

const HEAD_JSON: &str = "json_object('source_key',head.source_key,'catalog_namespace',head.catalog_namespace,'file_id',head.file_id,'origin_node_id',head.origin_node_id,'object_version',head.object_version,'source_size',head.source_size,'source_mtime',head.source_mtime,'raw_video_identity',head.raw_video_identity,'contract_version',head.contract_version,'desired_generation',head.desired_generation,'head_epoch',head.head_epoch,'state',head.state,'contract_key',head.contract_key,'blob_sha256',head.blob_sha256,'published_generation',head.published_generation,'publisher_job_id',head.publisher_job_id,'publisher_fence',head.publisher_fence,'analysis_request_id',head.analysis_request_id,'analysis_fence',head.analysis_fence,'updated_at_ms',head.updated_at_ms)";

fn metadata_shape_valid(
    source: &AnalysisCoordinationSource,
    claim: &super::analysis_coordination::AnalysisCoordinationClaim,
    now_ms: i64,
) -> bool {
    source.valid()
        && analysis_request_id_is_valid(&claim.request_id)
        && identifier(&claim.node_id)
        && claim.fence > 0
        && now_ms >= 0
}

fn head_source_condition() -> &'static str {
    "head.source_key=json_extract($1,'$.source_key') AND head.catalog_namespace=json_extract($1,'$.catalog_namespace') AND head.file_id=json_extract($1,'$.source.file_id') AND head.origin_node_id=json_extract($1,'$.source.origin_node_id') AND head.object_version=json_extract($1,'$.source.object_version') AND head.source_size=json_extract($1,'$.source.source_size') AND head.source_mtime=json_extract($1,'$.source.source_mtime') AND head.raw_video_identity=json_extract($1,'$.source.video_identity') AND head.contract_version=json_extract($1,'$.source.contract_version')"
}

fn decode_head(row: &str) -> Result<CopyIndexV3Head, StoreError> {
    let head: CopyIndexV3Head = super::background_jobs::decode(row)?;
    head.validate()?;
    Ok(head)
}

/// Read validated metadata only; neither a pending nor a quarantined pointer
/// becomes media-readable through this operation.
pub(super) async fn read_head<T: super::background_jobs::QueueSql>(
    store: &T,
    source_key: &str,
) -> Result<Option<CopyIndexV3Head>, StoreError> {
    if !digest(source_key) {
        return Err(invalid());
    }
    let body = super::background_jobs::encode(&serde_json::json!({"source_key":source_key}))?;
    let rows = store.queue_sql(format!("SELECT {HEAD_JSON} AS result_json FROM copy_index_v3_heads head WHERE head.source_key=json_extract($1,'$.source_key')"), body, false, true).await?;
    rows.first().map(|row| decode_head(row)).transpose()
}

/// Seed once under exact current metadata ownership. An existing healthy,
/// pending or quarantined head is returned unchanged, even for a new generation.
pub(super) async fn ensure_head<T: super::background_jobs::QueueSql>(
    store: &T,
    catalog_namespace: &str,
    source: AnalysisCoordinationSource,
    claim: super::analysis_coordination::AnalysisCoordinationClaim,
    now_ms: i64,
) -> Result<Option<CopyIndexV3Head>, StoreError> {
    if !metadata_shape_valid(&source, &claim, now_ms) {
        return Err(invalid());
    }
    let source_key = copy_index_v3_source_key(catalog_namespace, &source)?;
    let body = super::background_jobs::encode(
        &serde_json::json!({"catalog_namespace":catalog_namespace,"source":source,"claim":claim,"now_ms":now_ms,"source_key":source_key}),
    )?;
    let owned = super::analysis_coordination::metadata_owned_condition();
    let insert = format!("INSERT INTO copy_index_v3_heads(source_key,catalog_namespace,file_id,origin_node_id,object_version,source_size,source_mtime,raw_video_identity,contract_version,desired_generation,head_epoch,state,updated_at_ms) SELECT json_extract($1,'$.source_key'),json_extract($1,'$.catalog_namespace'),json_extract($1,'$.source.file_id'),json_extract($1,'$.source.origin_node_id'),json_extract($1,'$.source.object_version'),json_extract($1,'$.source.source_size'),json_extract($1,'$.source.source_mtime'),json_extract($1,'$.source.video_identity'),json_extract($1,'$.source.contract_version'),json_extract($1,'$.source.requested_generation'),0,'pending',json_extract($1,'$.now_ms') WHERE {owned} ON CONFLICT(source_key) DO NOTHING");
    let attempt = store.queue_sql(insert, body.clone(), true, true).await;
    let rows = store.queue_sql(format!("SELECT {HEAD_JSON} AS result_json FROM copy_index_v3_heads head WHERE {} AND {owned}",head_source_condition()), body, false, true).await?;
    if let Some(row) = rows.first() {
        return decode_head(row).map(Some);
    }
    attempt?;
    Ok(None)
}

/// Quarantine only the exact bad healthy head. Ledger insertion and pointer
/// state change are one transaction; replay preserves its original code/time.
/// Epoch remains fixed until a later guarded publication advances it.
pub(super) async fn quarantine_head<T: super::background_jobs::QueueSql>(
    store: &T,
    input: QuarantineCopyIndexV3Head,
) -> Result<bool, StoreError> {
    if !metadata_shape_valid(&input.source, &input.claim, input.now_ms)
        || !identifier(&input.code)
        || input.code.len() > 64
    {
        return Err(invalid());
    }
    let source_key = copy_index_v3_source_key(&input.catalog_namespace, &input.source)?;
    let successor = copy_index_v3_rebuild_generation(
        &source_key,
        &input.bad_blob_sha256,
        input.expected_epoch,
    )?;
    if input.source.requested_generation != successor {
        return Err(invalid());
    }
    let body = super::background_jobs::encode(&serde_json::json!({
        "catalog_namespace":input.catalog_namespace,"source":input.source,"claim":input.claim,
        "now_ms":input.now_ms,"source_key":source_key,"expected_epoch":input.expected_epoch,
        "bad_blob_sha256":input.bad_blob_sha256,"code":input.code,"successor_generation":successor,
    }))?;
    let owned = super::analysis_coordination::metadata_owned_condition();
    let scope = format!("{} AND head.head_epoch=json_extract($1,'$.expected_epoch') AND head.blob_sha256=json_extract($1,'$.bad_blob_sha256') AND head.contract_key IS NOT NULL",head_source_condition());
    let healthy = format!("{scope} AND head.state='healthy' AND {owned}");
    let ledger = "EXISTS(SELECT 1 FROM copy_index_v3_quarantines quarantine WHERE quarantine.source_key=head.source_key AND quarantine.head_epoch=head.head_epoch AND quarantine.bad_blob_sha256=head.blob_sha256 AND quarantine.successor_generation=json_extract($1,'$.successor_generation'))";
    let insert = format!("INSERT INTO copy_index_v3_quarantines(source_key,head_epoch,bad_blob_sha256,successor_generation,code,created_at_ms) SELECT head.source_key,head.head_epoch,head.blob_sha256,json_extract($1,'$.successor_generation'),json_extract($1,'$.code'),json_extract($1,'$.now_ms') FROM copy_index_v3_heads head WHERE {healthy} ON CONFLICT(source_key,head_epoch,bad_blob_sha256) DO NOTHING");
    let update = format!("UPDATE copy_index_v3_heads AS head SET state='quarantined',desired_generation=json_extract($1,'$.successor_generation'),updated_at_ms=json_extract($1,'$.now_ms') WHERE {healthy} AND {ledger}");
    let attempt = store
        .queue_transaction(vec![(insert, body.clone()), (update, body.clone())])
        .await;
    let replay = format!("SELECT 'true' AS result_json FROM copy_index_v3_heads head WHERE {scope} AND head.state='quarantined' AND head.desired_generation=json_extract($1,'$.successor_generation') AND {ledger} AND {owned}");
    let rows = store.queue_sql(replay, body, false, true).await?;
    if !rows.is_empty() {
        return Ok(true);
    }
    attempt?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    /// The publication trigger's target table, which every real schema
    /// creates long before v125.
    const COMMAND_ENVELOPE: &str = "CREATE TABLE background_job_commands(id TEXT PRIMARY KEY,operation TEXT,request_json TEXT,result_json TEXT);";

    fn source() -> AnalysisCoordinationSource {
        AnalysisCoordinationSource {
            file_id: 1,
            source_size: 100,
            source_mtime: -3,
            origin_node_id: "source-one".into(),
            object_version: "object:one".into(),
            video_identity: "a".repeat(16),
            contract_version: 1,
            requested_generation: "generation-one".into(),
        }
    }

    #[test]
    fn source_key_ignores_requested_generation_and_has_no_engine_input() {
        let source = source();
        let key = copy_index_v3_source_key("catalogue", &source).expect("source key");
        assert_eq!(
            key,
            "9fa5fbfc51b417ad4d5e8167be5542553917220bfd5c891235b05962e51e3a7e"
        );
        let mut successor = source.clone();
        successor.requested_generation = "different-viewer-generation".into();
        assert_eq!(
            key,
            copy_index_v3_source_key("catalogue", &successor).expect("same source, new request")
        );
        assert_ne!(
            key,
            copy_index_v3_source_key("different-catalogue", &source).expect("namespace")
        );
        for field in 0..7 {
            let mut changed = source.clone();
            match field {
                0 => changed.file_id += 1,
                1 => changed.source_size += 1,
                2 => changed.source_mtime += 1,
                3 => changed.origin_node_id = "source-two".into(),
                4 => changed.object_version = "object:two".into(),
                5 => changed.video_identity = "b".repeat(16),
                _ => changed.contract_version += 1,
            }
            assert_ne!(
                key,
                copy_index_v3_source_key("catalogue", &changed).expect("valid changed source"),
                "field {field}"
            );
        }
        assert!(copy_index_v3_source_key(" ", &source).is_err());
    }

    #[test]
    fn quarantine_generation_converges_and_changes_with_bad_head_incarnation() {
        let key = copy_index_v3_source_key("catalogue", &source()).expect("source key");
        let generation =
            copy_index_v3_rebuild_generation(&key, &"b".repeat(64), 7).expect("repair generation");
        assert_eq!(generation.len(), 64);
        assert_eq!(
            generation,
            copy_index_v3_rebuild_generation(&key, &"b".repeat(64), 7).expect("another viewer")
        );
        assert_ne!(
            generation,
            copy_index_v3_rebuild_generation(&key, &"b".repeat(64), 8).expect("new head epoch")
        );
        assert_ne!(
            generation,
            copy_index_v3_rebuild_generation(&key, &"c".repeat(64), 7)
                .expect("different broken blob")
        );
        assert!(copy_index_v3_rebuild_generation(&key, &"b".repeat(64), -1).is_err());
        let quarantine = CopyIndexV3Quarantine {
            source_key: key,
            head_epoch: 7,
            bad_blob_sha256: "b".repeat(64),
            successor_generation: generation,
            code: "corrupt_blob".into(),
            created_at_ms: 1_000,
        };
        quarantine.validate().expect("convergent record");
        let mut wrong = quarantine;
        wrong.successor_generation = "d".repeat(64);
        assert!(wrong.validate().is_err());
    }

    fn artifact(
        conn: &Connection,
        blob: &str,
        engine: &str,
        bytes: i64,
    ) -> rusqlite::Result<usize> {
        conn.execute("INSERT INTO copy_index_v3_artifacts(blob_sha256,contract_key,catalog_namespace,file_id,source_sha256,contract_version,output_contract_sha256,init_contract_sha256,bytes,built_by_node_id,built_at_ms,producer_engine_sha256) VALUES(?1,?2,'catalogue',1,?3,1,?4,?5,?6,'source-one',1000,?7)",
            params![blob,"a".repeat(64),"b".repeat(64),"c".repeat(64),"d".repeat(64),bytes,engine])
    }

    #[test]
    fn schema_keeps_engine_blob_variants_immutable_and_heads_explicitly_healthy() {
        let conn = Connection::open_in_memory().expect("SQLite");
        conn.execute_batch(COMMAND_ENVELOPE)
            .expect("command envelope");
        conn.execute_batch(include_str!("copy_index_v3.sql"))
            .expect("mechanical v3 schema");
        let first = "1".repeat(64);
        let second = "2".repeat(64);
        artifact(
            &conn,
            &first,
            &"e".repeat(64),
            MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES as i64,
        )
        .expect("first engine variant");
        let key = copy_index_v3_source_key("catalogue", &source()).expect("source key");
        conn.execute("INSERT INTO copy_index_v3_heads(source_key,catalog_namespace,file_id,origin_node_id,object_version,source_size,source_mtime,raw_video_identity,contract_version,desired_generation,head_epoch,state,updated_at_ms) VALUES(?1,'catalogue',1,'source-one','object:one',100,-3,?2,1,'generation-one',0,'pending',1000)",params![key,"a".repeat(16)]).expect("pending head");
        assert!(
            conn.execute(
                "UPDATE copy_index_v3_heads SET state='healthy' WHERE source_key=?1",
                [&key]
            )
            .is_err(),
            "healthy needs both pointers"
        );
        conn.execute("UPDATE copy_index_v3_heads SET state='healthy',contract_key=?1,blob_sha256=?2 WHERE source_key=?3",params!["a".repeat(64),first,key]).expect("healthy pointer");
        artifact(&conn, &second, &"f".repeat(64), 100)
            .expect("second engine, same output contract");
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM copy_index_v3_artifacts WHERE contract_key=?1",
                ["a".repeat(64)],
                |row| row.get::<_, i64>(0)
            )
            .expect("variant count"),
            2
        );
        assert_eq!(
            conn.query_row(
                "SELECT blob_sha256 FROM copy_index_v3_heads WHERE source_key=?1",
                [&key],
                |row| row.get::<_, String>(0)
            )
            .expect("retained first head"),
            first
        );
        assert!(
            conn.execute(
                "UPDATE copy_index_v3_artifacts SET producer_engine_sha256=?1 WHERE blob_sha256=?2",
                params!["9".repeat(64), first]
            )
            .is_err(),
            "variant provenance cannot be replaced"
        );
        assert!(
            artifact(
                &conn,
                &"3".repeat(64),
                &"e".repeat(64),
                MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES as i64 + 1
            )
            .is_err(),
            "authoritative blob bound"
        );
        conn.execute(
            "UPDATE copy_index_v3_heads SET state='quarantined' WHERE source_key=?1",
            [&key],
        )
        .expect("quarantine retains diagnostic pointer");
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM copy_index_v3_heads WHERE state='healthy'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("readable heads"),
            0
        );
        let names = super::super::analysis_coordination::schema_objects()
            .expect("composed125 schema objects");
        assert!(names
            .iter()
            .any(|(name, _)| name == "copy_index_source_attestations"));
        assert!(names
            .iter()
            .any(|(name, _)| name == "copy_index_v3_artifacts_immutable"));
        assert!(names.iter().any(|(name, _)| name == "copy_index_v3_heads"));
        assert!(names
            .iter()
            .any(|(name, _)| name == "copy_index_v3_quarantines"));
    }

    #[test]
    fn attestation_requires_full_hash_regime_and_closed_receipt_shape() {
        let receipt = CopyIndexSourceAttestation {
            origin_node_id: "source-one".into(),
            file_id: 1,
            object_version: "object:one".into(),
            source_size: 100,
            source_mtime: -3,
            source_sha256: "b".repeat(64),
            regime_revision: 1,
            completed_at_ms: 1000,
            analysis_request_id: "opaque / analysis\u{0000}é".into(),
            analysis_fence: 7,
            phase_sequence: 1,
            custody_id: uuid::Uuid::new_v4().to_string(),
        };
        receipt.validate().expect("full-hash receipt shape");
        let mut sampled = receipt.clone();
        sampled.regime_revision = 2;
        assert!(sampled.validate().is_err());
        let mut value = serde_json::to_value(&receipt).expect("receipt JSON");
        value["sample_sha256"] = "c".repeat(64).into();
        assert!(serde_json::from_value::<CopyIndexSourceAttestation>(value).is_err());
        let conn = Connection::open_in_memory().expect("SQLite");
        conn.execute_batch(COMMAND_ENVELOPE)
            .expect("command envelope");
        conn.execute_batch(include_str!("copy_index_v3.sql"))
            .expect("v3 schema");
        let insert = |regime: i64| {
            conn.execute("INSERT INTO copy_index_source_attestations(origin_node_id,file_id,object_version,source_size,source_mtime,source_sha256,regime_revision,completed_at_ms,analysis_request_id,analysis_fence,phase_sequence,custody_id) VALUES('source-one',1,'object:one',100,-3,?1,?2,1000,'analysis-one',7,1,?3)",params![receipt.source_sha256,regime,receipt.custody_id])
        };
        assert!(insert(2).is_err());
        insert(1).expect("full-hash regime only");
        assert!(insert(1).is_err(), "origin/file/object receipt is unique");
    }
}

/// Persist only a genuine completed full-source read under its exact current
/// Analysis phase. The caller joins child/readers and verifies the held object
/// before invoking this; this write grants no read or publication authority.
pub(super) async fn record_attestation<T: super::background_jobs::QueueSql>(
    store: &T,
    phase: super::analysis_coordination::AnalysisPhysicalPhase,
    attestation: CopyIndexSourceAttestation,
    now_ms: i64,
) -> Result<bool, StoreError> {
    use super::background_jobs::{decode, encode};
    attestation.validate()?;
    if !phase.valid()
        || now_ms < 0
        || attestation.completed_at_ms > now_ms
        || attestation.origin_node_id != phase.source.origin_node_id
        || attestation.origin_node_id != phase.claim.node_id
        || attestation.file_id != phase.source.file_id
        || attestation.object_version != phase.source.object_version
        || attestation.source_size != phase.source.source_size
        || attestation.source_mtime != phase.source.source_mtime
        || attestation.analysis_request_id != phase.claim.request_id
        || attestation.analysis_fence != phase.claim.fence
        || attestation.phase_sequence != phase.phase_sequence
        || attestation.custody_id != phase.custody.custody_id
    {
        return Err(invalid());
    }
    let body = encode(&serde_json::json!({
        "claim":phase.claim,"source":phase.source,"sequence":phase.phase_sequence,
        "custody_id":phase.custody.custody_id,"attestation":attestation,"now_ms":now_ms,
    }))?;
    let owned = super::analysis_coordination::physical_phase_owned_condition();
    let sql = format!("INSERT INTO copy_index_source_attestations(origin_node_id,file_id,object_version,source_size,source_mtime,source_sha256,regime_revision,completed_at_ms,analysis_request_id,analysis_fence,phase_sequence,custody_id)
        SELECT json_extract($1,'$.attestation.origin_node_id'),json_extract($1,'$.attestation.file_id'),json_extract($1,'$.attestation.object_version'),json_extract($1,'$.attestation.source_size'),json_extract($1,'$.attestation.source_mtime'),json_extract($1,'$.attestation.source_sha256'),1,json_extract($1,'$.attestation.completed_at_ms'),json_extract($1,'$.attestation.analysis_request_id'),json_extract($1,'$.attestation.analysis_fence'),json_extract($1,'$.attestation.phase_sequence'),json_extract($1,'$.attestation.custody_id') WHERE {owned}
        ON CONFLICT(origin_node_id,file_id,object_version) DO UPDATE SET source_sha256=excluded.source_sha256
        WHERE copy_index_source_attestations.source_size=excluded.source_size AND copy_index_source_attestations.source_mtime=excluded.source_mtime AND copy_index_source_attestations.source_sha256=excluded.source_sha256 AND copy_index_source_attestations.regime_revision=1
        RETURNING 'true' AS result_json");
    let rows = store.queue_sql(sql, body, true, true).await?;
    rows.first()
        .map(|row| decode(row))
        .transpose()
        .map(|value| value.unwrap_or(false))
}

pub(super) async fn source_attestation<T: super::background_jobs::QueueSql>(
    store: &T,
    origin_node_id: &str,
    file_id: i64,
    object_version: &str,
) -> Result<Option<CopyIndexSourceAttestation>, StoreError> {
    use super::background_jobs::{decode, encode};
    if !identifier(origin_node_id) || file_id <= 0 || !bounded(object_version, 4096) {
        return Err(invalid());
    }
    let body = encode(
        &serde_json::json!({"origin_node_id":origin_node_id,"file_id":file_id,"object_version":object_version}),
    )?;
    let rows=store.queue_sql("SELECT json_object('origin_node_id',origin_node_id,'file_id',file_id,'object_version',object_version,'source_size',source_size,'source_mtime',source_mtime,'source_sha256',source_sha256,'regime_revision',regime_revision,'completed_at_ms',completed_at_ms,'analysis_request_id',analysis_request_id,'analysis_fence',analysis_fence,'phase_sequence',phase_sequence,'custody_id',custody_id) AS result_json FROM copy_index_source_attestations WHERE origin_node_id=json_extract($1,'$.origin_node_id') AND file_id=json_extract($1,'$.file_id') AND object_version=json_extract($1,'$.object_version')".into(),body,false,true).await?;
    let value: Option<CopyIndexSourceAttestation> =
        rows.first().map(|row| decode(row)).transpose()?;
    if let Some(value) = &value {
        value.validate()?;
    }
    Ok(value)
}

/// Publish verified candidate bytes under an exact metadata and worker attempt.
/// Byte verification is the producer's obligation; this transaction grants no IO.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishCopyIndexV3 {
    pub token: super::background_jobs::JobToken,
    pub claim: super::analysis_coordination::AnalysisCoordinationClaim,
    pub source: AnalysisCoordinationSource,
    pub artifact: CopyIndexV3Artifact,
    pub expected_head_epoch: i64,
    pub now_ms: i64,
}

/// The exact publisher payload a worker enqueues for this candidate. The
/// guarded publication later requires the running job's payload to equal it
/// field for field, so producer and publisher share this one constructor.
pub fn copy_index_v3_publish_payload(
    claim: &super::analysis_coordination::AnalysisCoordinationClaim,
    source: &AnalysisCoordinationSource,
    artifact: &CopyIndexV3Artifact,
    expected_head_epoch: i64,
) -> Result<super::background_jobs::JobPayload, StoreError> {
    artifact.validate()?;
    if !metadata_shape_valid(source, claim, 0)
        || expected_head_epoch < 0
        || artifact.file_id != source.file_id
        || artifact.contract_version != source.contract_version
    {
        return Err(invalid());
    }
    let source_key = copy_index_v3_source_key(&artifact.catalog_namespace, source)?;
    let logical_key = super::fragment_index_cluster::cluster_fragment_index_v3_key(
        &artifact.catalog_namespace,
        artifact.file_id,
        &artifact.source_sha256,
        &super::fragment_index_cluster::FragmentIndexOutputIdentity {
            contract_version: artifact.contract_version,
            keyframe_table_sha256: artifact.output_contract_sha256.clone(),
        },
    )
    .ok_or_else(invalid)?;
    if logical_key != artifact.contract_key {
        return Err(invalid());
    }
    let payload = super::background_jobs::JobPayload::FragmentIndexPublish {
        publication_version: 3,
        source_key,
        catalog_namespace: artifact.catalog_namespace.clone(),
        init_contract_sha256: artifact.init_contract_sha256.clone(),
        expected_head_epoch,
        file_id: source.file_id,
        source_size: source.source_size,
        source_mtime: source.source_mtime,
        origin_node_id: source.origin_node_id.clone(),
        object_version: source.object_version.clone(),
        coordinator_request_id: claim.request_id.clone(),
        coordinator_fence: claim.fence,
        requested_generation: source.requested_generation.clone(),
        contract_version: artifact.contract_version,
        output_contract_sha256: artifact.output_contract_sha256.clone(),
        source_sha256: artifact.source_sha256.clone(),
        blob_sha256: artifact.blob_sha256.clone(),
        bytes: artifact.bytes,
        cache_key: logical_key,
        producer_engine_sha256: artifact.producer_engine_sha256.clone(),
    };
    payload.validate()?;
    Ok(payload)
}

fn publication_body(request: &PublishCopyIndexV3) -> Result<serde_json::Value, StoreError> {
    request.token.validate()?;
    if !metadata_shape_valid(&request.source, &request.claim, request.now_ms) {
        return Err(invalid());
    }
    let artifact = &request.artifact;
    let payload = copy_index_v3_publish_payload(
        &request.claim,
        &request.source,
        artifact,
        request.expected_head_epoch,
    )?;
    let source_key = copy_index_v3_source_key(&artifact.catalog_namespace, &request.source)?;
    let logical_key = artifact.contract_key.clone();
    // Renewal clocks/revisions and builder timestamps are not a new semantic
    // publication. Attempt identity is independently checked against history.
    let semantic = serde_json::json!({"version":3,"source_key":source_key,"source":request.source,"claim":request.claim,
        "expected_head_epoch":request.expected_head_epoch,"catalog_namespace":artifact.catalog_namespace,
        "file_id":artifact.file_id,"contract_key":logical_key,"blob_sha256":artifact.blob_sha256,
        "source_sha256":artifact.source_sha256,"contract_version":artifact.contract_version,
        "output_contract_sha256":artifact.output_contract_sha256,"init_contract_sha256":artifact.init_contract_sha256,
        "bytes":artifact.bytes,"producer_engine_sha256":artifact.producer_engine_sha256});
    let mut hash = Sha256::new();
    hash.update(b"plurx/copy-index/publication/v3\0");
    hash.update(super::background_jobs::encode(&semantic)?.as_bytes());
    let mut body = serde_json::to_value(request).map_err(|_| invalid())?;
    body["source_key"] = source_key.into();
    body["catalog_namespace"] = artifact.catalog_namespace.clone().into();
    body["expected_payload"] = serde_json::to_value(payload).map_err(|_| invalid())?;
    body["publication_digest"] = hex::encode(hash.finalize()).into();
    Ok(body)
}

pub(super) async fn publish<T: super::background_jobs::QueueSql>(
    store: &T,
    request: PublishCopyIndexV3,
) -> Result<super::background_jobs::JobPublishOutcome, StoreError> {
    let body = super::background_jobs::encode(&publication_body(&request)?)?;
    let rows = store.queue_sql(publication_sql(), body, true, true).await?;
    super::background_jobs::decode(rows.first().ok_or_else(invalid)?)
}

fn publication_sql() -> String {
    let non_banning = NON_BANNING_CODES
        .iter()
        .map(|code| format!("'{code}'"))
        .collect::<Vec<_>>()
        .join(",");
    let metadata = super::analysis_coordination::metadata_owned_condition();
    let source_head = head_source_condition();
    format!(
        r#"
WITH input AS (SELECT json($1) AS body), snapshot AS (
 SELECT body, job.id, job.state, job.result_ref,
 CASE WHEN job.kind='fragment_index_publish' AND job.payload_version=1 AND job.state='running'
  AND job.owner_node_id=json_extract(body,'$.token.node_id') AND job.owner_boot_id=json_extract(body,'$.token.boot_id')
  AND job.claim_id=json_extract(body,'$.token.claim_id') AND job.fence=json_extract(body,'$.token.fence')
  AND job.revision=json_extract(body,'$.token.revision') AND job.lease_expires_ms=json_extract(body,'$.token.lease_expires_ms')
  AND job.lease_expires_ms>json_extract(body,'$.now_ms') AND job.revision<9223372036854775807
  AND (job.retry_deadline_ms=0 OR job.retry_deadline_ms>json_extract(body,'$.now_ms'))
  AND json_extract(job.payload_json,'$.origin_node_id')=json_extract(body,'$.source.origin_node_id') AND job.owner_node_id=json_extract(body,'$.source.origin_node_id')
  AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||job.owner_node_id)
  AND EXISTS(SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id=job.id AND attempt.fence=job.fence
   AND attempt.claim_id=job.claim_id AND attempt.owner_node_id=job.owner_node_id AND attempt.owner_boot_id=job.owner_boot_id
   AND attempt.finished_at_ms IS NULL AND attempt.outcome IS NULL) THEN 1 ELSE 0 END AS owns,
 CASE WHEN EXISTS(SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id=job.id AND attempt.fence=job.fence
   AND attempt.fence=json_extract(body,'$.token.fence') AND attempt.claim_id=json_extract(body,'$.token.claim_id')
   AND attempt.owner_node_id=json_extract(body,'$.token.node_id') AND attempt.owner_boot_id=json_extract(body,'$.token.boot_id')) THEN 1 ELSE 0 END AS same_attempt,
 CASE WHEN {metadata} AND EXISTS(SELECT 1 FROM analysis_coordination_roles role
   WHERE role.request_id=json_extract(body,'$.claim.request_id') AND role.active_custody_id IS NULL) THEN 1 ELSE 0 END AS metadata_owns,
 CASE WHEN file.id IS NOT NULL AND file.size=json_extract(body,'$.source.source_size') AND file.mtime=json_extract(body,'$.source.source_mtime') THEN 1 ELSE 0 END AS source_matches,
 CASE WHEN (SELECT count(*) FROM json_each(job.payload_json))=(SELECT count(*) FROM json_each(json_extract(body,'$.expected_payload')))
  AND NOT EXISTS(SELECT 1 FROM json_each(json_extract(body,'$.expected_payload')) field
   WHERE json_type(job.payload_json,'$.'||field.key) IS NOT field.type OR json_extract(job.payload_json,'$.'||field.key) IS NOT field.value) THEN 1 ELSE 0 END AS payload_matches,
 CASE WHEN {source_head} AND head.desired_generation=json_extract(body,'$.source.requested_generation')
  AND head.head_epoch=json_extract(body,'$.expected_head_epoch')
  AND ((head.state='healthy' AND head.contract_key=json_extract(body,'$.artifact.contract_key')
      AND head.published_generation=json_extract(body,'$.source.requested_generation'))
   OR (head.state IN ('pending','quarantined') AND head.head_epoch<9223372036854775807
    AND NOT EXISTS(SELECT 1 FROM copy_index_v3_quarantines bad WHERE bad.source_key=head.source_key
     AND bad.bad_blob_sha256=json_extract(body,'$.artifact.blob_sha256') AND bad.code NOT IN ({non_banning})))) THEN 1 ELSE 0 END AS head_matches,
 CASE WHEN EXISTS(SELECT 1 FROM copy_index_source_attestations receipt
   WHERE receipt.origin_node_id=json_extract(body,'$.source.origin_node_id') AND receipt.file_id=json_extract(body,'$.source.file_id')
    AND receipt.object_version=json_extract(body,'$.source.object_version') AND receipt.source_size=json_extract(body,'$.source.source_size')
    AND receipt.source_mtime=json_extract(body,'$.source.source_mtime') AND receipt.source_sha256=json_extract(body,'$.artifact.source_sha256') AND receipt.regime_revision=1)
  OR EXISTS(SELECT 1 FROM cluster_fragment_index_sources receipt
   WHERE receipt.node_id=json_extract(body,'$.source.origin_node_id') AND receipt.file_id=json_extract(body,'$.source.file_id')
    AND receipt.object_version='hevc-full-v1:'||json_extract(body,'$.source.object_version') AND receipt.source_size=json_extract(body,'$.source.source_size')
    AND receipt.source_mtime=json_extract(body,'$.source.source_mtime') AND receipt.source_sha256=json_extract(body,'$.artifact.source_sha256')) THEN 1 ELSE 0 END AS attested,
 CASE WHEN (artifact.contract_key IS NULL AND json_extract(body,'$.artifact.built_by_node_id')=json_extract(body,'$.token.node_id'))
  OR (artifact.contract_key IS NOT NULL AND artifact.catalog_namespace=json_extract(body,'$.artifact.catalog_namespace')
   AND artifact.file_id=json_extract(body,'$.artifact.file_id') AND artifact.source_sha256=json_extract(body,'$.artifact.source_sha256')
   AND artifact.contract_version=json_extract(body,'$.artifact.contract_version') AND artifact.output_contract_sha256=json_extract(body,'$.artifact.output_contract_sha256')
   AND artifact.init_contract_sha256=json_extract(body,'$.artifact.init_contract_sha256') AND artifact.bytes=json_extract(body,'$.artifact.bytes')
   AND artifact.producer_engine_sha256=json_extract(body,'$.artifact.producer_engine_sha256')) THEN 1 ELSE 0 END AS variant_matches,
 json_object('artifact_kind','fragment_index_v3','artifact_key',json_extract(body,'$.artifact.contract_key'),
  'candidate',json_object('contract_key',json_extract(body,'$.artifact.contract_key'),'blob_sha256',json_extract(body,'$.artifact.blob_sha256'),
    'node_id',json_extract(body,'$.token.node_id'),'bytes',json_extract(body,'$.artifact.bytes')),
  'head',json_object('source_key',json_extract(body,'$.source_key'),'contract_key',json_extract(body,'$.artifact.contract_key'),
    'blob_sha256',CASE WHEN head.state='healthy' THEN head.blob_sha256 ELSE json_extract(body,'$.artifact.blob_sha256') END,
    'head_epoch',CASE WHEN head.state='healthy' THEN head.head_epoch ELSE head.head_epoch+1 END),
  'publication_digest',json_extract(body,'$.publication_digest')) AS output
 FROM input LEFT JOIN background_jobs job ON job.id=json_extract(body,'$.token.job_id')
 LEFT JOIN files file ON file.id=json_extract(body,'$.source.file_id')
 LEFT JOIN copy_index_v3_heads head ON head.source_key=json_extract(body,'$.source_key')
 LEFT JOIN copy_index_v3_artifacts artifact ON artifact.contract_key=json_extract(body,'$.artifact.contract_key') AND artifact.blob_sha256=json_extract(body,'$.artifact.blob_sha256')
), classified AS (
 SELECT *, CASE WHEN state='succeeded' AND same_attempt=1 AND json_valid(result_ref)
   AND json_extract(result_ref,'$.publication_digest')=json_extract(body,'$.publication_digest') THEN 'already_published'
  WHEN source_matches=0 THEN 'source_changed'
  WHEN owns=0 OR metadata_owns=0 THEN 'lost_ownership'
  WHEN payload_matches=0 OR head_matches=0 OR attested=0 OR variant_matches=0 THEN 'artifact_conflict'
  ELSE 'published' END AS outcome FROM snapshot
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
 SELECT json_extract(body,'$.token.claim_id'),'publish_copy_index_v3',body,
  CASE WHEN outcome IN ('published','already_published') THEN json_object('outcome',outcome,'job_id',id,
    'result_ref',(CASE WHEN outcome='already_published' THEN result_ref ELSE output END)||'')
   ELSE json_object('outcome',outcome) END FROM classified RETURNING result_json
"#
    )
}

/// One verified holder of a variant's exact bytes: its publisher, or a node
/// that hydrated the blob and checked its digest before recording itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyIndexV3Location {
    pub contract_key: String,
    pub blob_sha256: String,
    pub node_id: String,
    pub bytes: i64,
    pub verified_at_ms: i64,
    pub last_seen_at_ms: i64,
}
impl CopyIndexV3Location {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !digest(&self.contract_key)
            || !digest(&self.blob_sha256)
            || !identifier(&self.node_id)
            || !(1..=MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES as i64).contains(&self.bytes)
            || self.verified_at_ms < 0
            || self.last_seen_at_ms < self.verified_at_ms
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Healthy heads returned for one catalogue file; a file has one head per
/// origin object, so this bound is far above any real fleet.
const MAX_FILE_HEADS: usize = 16;
/// Holders listed for one variant, most recently seen first.
const MAX_VARIANT_LOCATIONS: usize = 128;

const ARTIFACT_JSON: &str = "json_object('blob_sha256',artifact.blob_sha256,'contract_key',artifact.contract_key,'catalog_namespace',artifact.catalog_namespace,'file_id',artifact.file_id,'source_sha256',artifact.source_sha256,'contract_version',artifact.contract_version,'output_contract_sha256',artifact.output_contract_sha256,'init_contract_sha256',artifact.init_contract_sha256,'bytes',artifact.bytes,'built_by_node_id',artifact.built_by_node_id,'built_at_ms',artifact.built_at_ms,'producer_engine_sha256',artifact.producer_engine_sha256)";

/// A head a consumer may read: healthy, and published for the generation it
/// still desires. A quarantine moves the desire before any successor exists.
const HEAD_READABLE: &str =
    "head.state='healthy' AND head.published_generation=head.desired_generation";

fn variant_body(contract_key: &str, blob_sha256: &str) -> Result<serde_json::Value, StoreError> {
    if !digest(contract_key) || !digest(blob_sha256) {
        return Err(invalid());
    }
    Ok(serde_json::json!({"contract_key":contract_key,"blob_sha256":blob_sha256}))
}

/// One head by its source key with the variant row it points at and whether
/// `node_id` is a recorded holder of those bytes: everything a consumer or
/// the peer route needs, from primary keys only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyIndexV3Variant {
    pub head: CopyIndexV3Head,
    pub artifact: Option<CopyIndexV3Artifact>,
    pub held: bool,
}
impl CopyIndexV3Variant {
    /// The variant a consumer may read or a holder may serve: its head is
    /// healthy at the generation it desires and the pointed-at row exists.
    pub fn readable(&self) -> Option<&CopyIndexV3Artifact> {
        (self.head.state == CopyIndexV3HeadState::Healthy
            && self.head.published_generation.as_deref()
                == Some(self.head.desired_generation.as_str()))
        .then_some(self.artifact.as_ref())
        .flatten()
    }
}

/// Read one head and its variant by primary key. A pending or quarantined
/// head is returned as it is, so the caller can tell absent from unreadable;
/// nothing here locates bytes or makes a pointer media-readable.
pub(super) async fn read_variant<T: super::background_jobs::QueueSql>(
    store: &T,
    source_key: &str,
    node_id: &str,
) -> Result<Option<CopyIndexV3Variant>, StoreError> {
    if !digest(source_key) || !identifier(node_id) {
        return Err(invalid());
    }
    let body = super::background_jobs::encode(
        &serde_json::json!({"source_key":source_key,"node_id":node_id}),
    )?;
    let rows = store.queue_sql(format!("SELECT json_object('head',{HEAD_JSON},'artifact',json(CASE WHEN artifact.contract_key IS NULL THEN NULL ELSE {ARTIFACT_JSON} END),'held',json(CASE WHEN EXISTS(SELECT 1 FROM copy_index_v3_locations location WHERE location.contract_key=head.contract_key AND location.blob_sha256=head.blob_sha256 AND location.node_id=json_extract($1,'$.node_id')) THEN 'true' ELSE 'false' END)) AS result_json FROM copy_index_v3_heads head LEFT JOIN copy_index_v3_artifacts artifact ON artifact.contract_key=head.contract_key AND artifact.blob_sha256=head.blob_sha256 WHERE head.source_key=json_extract($1,'$.source_key')"), body, false, true).await?;
    let variant: Option<CopyIndexV3Variant> = rows
        .first()
        .map(|row| super::background_jobs::decode(row))
        .transpose()?;
    if let Some(variant) = &variant {
        variant.head.validate()?;
        if let Some(artifact) = &variant.artifact {
            artifact.validate()?;
        }
    }
    Ok(variant)
}

/// Readable heads other origins (or other objects) published for the same
/// catalogue file, size, mtime, video identity and contract as `source`,
/// newest first. The filters run before the bound. A consumer must prove
/// content equivalence before using one; this read grants nothing. The table
/// has no `(catalog_namespace, file_id)` index yet, so callers reach this
/// only after an exact-key miss.
pub(super) async fn healthy_heads_for_file<T: super::background_jobs::QueueSql>(
    store: &T,
    catalog_namespace: &str,
    source: &AnalysisCoordinationSource,
) -> Result<Vec<CopyIndexV3Head>, StoreError> {
    if !namespace(catalog_namespace) || !source.valid() {
        return Err(invalid());
    }
    let body = super::background_jobs::encode(
        &serde_json::json!({"catalog_namespace":catalog_namespace,"source":source}),
    )?;
    let rows = store.queue_sql(format!("SELECT {HEAD_JSON} AS result_json FROM copy_index_v3_heads head WHERE head.catalog_namespace=json_extract($1,'$.catalog_namespace') AND head.file_id=json_extract($1,'$.source.file_id') AND head.source_size=json_extract($1,'$.source.source_size') AND head.source_mtime=json_extract($1,'$.source.source_mtime') AND head.raw_video_identity=json_extract($1,'$.source.video_identity') AND head.contract_version=json_extract($1,'$.source.contract_version') AND {HEAD_READABLE} ORDER BY head.updated_at_ms DESC,head.source_key LIMIT {MAX_FILE_HEADS}"), body, false, true).await?;
    rows.iter().map(|row| decode_head(row)).collect()
}

/// Advisory holder list for one variant. A listed node is asked for the exact
/// bytes and its answer is verified by digest; the row itself proves nothing.
/// A node the cluster has removed is not a holder: its row is not listed.
pub(super) async fn list_locations<T: super::background_jobs::QueueSql>(
    store: &T,
    contract_key: &str,
    blob_sha256: &str,
) -> Result<Vec<CopyIndexV3Location>, StoreError> {
    let body = super::background_jobs::encode(&variant_body(contract_key, blob_sha256)?)?;
    let rows = store.queue_sql(format!("SELECT json_object('contract_key',location.contract_key,'blob_sha256',location.blob_sha256,'node_id',location.node_id,'bytes',location.bytes,'verified_at_ms',location.verified_at_ms,'last_seen_at_ms',location.last_seen_at_ms) AS result_json FROM copy_index_v3_locations location WHERE location.contract_key=json_extract($1,'$.contract_key') AND location.blob_sha256=json_extract($1,'$.blob_sha256') AND {} ORDER BY location.last_seen_at_ms DESC,location.node_id LIMIT {MAX_VARIANT_LOCATIONS}", holder_is_member("location")), body, false, false).await?;
    rows.iter()
        .map(|row| {
            let location: CopyIndexV3Location = super::background_jobs::decode(row)?;
            location.validate()?;
            Ok(location)
        })
        .collect()
}

/// Record a node that holds the exact verified bytes of a published variant.
/// The row is written only when the head named by `source_key` is readable
/// and points at exactly this variant with these bytes; a pending,
/// quarantined, superseded or unknown variant gains no holder. Every lookup
/// is by primary key. Verifying the bytes is the caller's obligation.
pub(super) async fn put_location<T: super::background_jobs::QueueSql>(
    store: &T,
    source_key: &str,
    location: CopyIndexV3Location,
) -> Result<bool, StoreError> {
    location.validate()?;
    if !digest(source_key) {
        return Err(invalid());
    }
    let mut body = serde_json::to_value(&location).map_err(|_| invalid())?;
    body["source_key"] = source_key.into();
    let rows = store.queue_sql(format!("INSERT INTO copy_index_v3_locations(contract_key,blob_sha256,node_id,bytes,verified_at_ms,last_seen_at_ms) SELECT artifact.contract_key,artifact.blob_sha256,json_extract($1,'$.node_id'),artifact.bytes,json_extract($1,'$.verified_at_ms'),json_extract($1,'$.last_seen_at_ms') FROM copy_index_v3_heads head JOIN copy_index_v3_artifacts artifact ON artifact.contract_key=head.contract_key AND artifact.blob_sha256=head.blob_sha256 WHERE head.source_key=json_extract($1,'$.source_key') AND {HEAD_READABLE} AND head.contract_key=json_extract($1,'$.contract_key') AND head.blob_sha256=json_extract($1,'$.blob_sha256') AND artifact.bytes=json_extract($1,'$.bytes') ON CONFLICT(contract_key,blob_sha256,node_id) DO UPDATE SET verified_at_ms=excluded.verified_at_ms,last_seen_at_ms=excluded.last_seen_at_ms RETURNING 'true' AS result_json"), super::background_jobs::encode(&body)?, true, true).await?;
    Ok(!rows.is_empty())
}

/// Drop one holder whose bytes were missing or failed verification. Only that
/// node's row is removed; the variant and its head are untouched.
pub(super) async fn forget_location<T: super::background_jobs::QueueSql>(
    store: &T,
    contract_key: &str,
    blob_sha256: &str,
    node_id: &str,
) -> Result<bool, StoreError> {
    if !identifier(node_id) {
        return Err(invalid());
    }
    let mut body = variant_body(contract_key, blob_sha256)?;
    body["node_id"] = node_id.into();
    let rows = store.queue_sql("DELETE FROM copy_index_v3_locations WHERE contract_key=json_extract($1,'$.contract_key') AND blob_sha256=json_extract($1,'$.blob_sha256') AND node_id=json_extract($1,'$.node_id') RETURNING 'true' AS result_json".into(), super::background_jobs::encode(&body)?, true, true).await?;
    Ok(!rows.is_empty())
}

/// Drop every holder row of one node for one blob, whichever logical
/// contracts name those bytes: the node's local file is gone. The variants
/// and their heads are untouched. The table is keyed by contract first, so
/// this reads it whole; only cache eviction calls it.
pub(super) async fn forget_blob_holder<T: super::background_jobs::QueueSql>(
    store: &T,
    blob_sha256: &str,
    node_id: &str,
) -> Result<u64, StoreError> {
    if !digest(blob_sha256) || !identifier(node_id) {
        return Err(invalid());
    }
    let body = super::background_jobs::encode(
        &serde_json::json!({"blob_sha256":blob_sha256,"node_id":node_id}),
    )?;
    let rows = store.queue_sql("DELETE FROM copy_index_v3_locations WHERE blob_sha256=json_extract($1,'$.blob_sha256') AND node_id=json_extract($1,'$.node_id') RETURNING 'true' AS result_json".into(), body, true, true).await?;
    Ok(rows.len() as u64)
}

/// Whether `node_id` is the last listed holder of a readable head's variant
/// with these bytes: it is listed for one, and no other member is. Evicting
/// that node's copy would leave a healthy head nobody can serve. Reads the
/// heads by blob, which no index covers; only cache eviction asks.
pub(super) async fn last_blob_holder<T: super::background_jobs::QueueSql>(
    store: &T,
    blob_sha256: &str,
    node_id: &str,
) -> Result<bool, StoreError> {
    if !digest(blob_sha256) || !identifier(node_id) {
        return Err(invalid());
    }
    let body = super::background_jobs::encode(
        &serde_json::json!({"blob_sha256":blob_sha256,"node_id":node_id}),
    )?;
    let rows = store.queue_sql(format!("SELECT 'true' AS result_json FROM copy_index_v3_heads head WHERE head.blob_sha256=json_extract($1,'$.blob_sha256') AND {HEAD_READABLE} AND EXISTS(SELECT 1 FROM copy_index_v3_locations own WHERE own.contract_key=head.contract_key AND own.blob_sha256=head.blob_sha256 AND own.node_id=json_extract($1,'$.node_id')) AND NOT EXISTS(SELECT 1 FROM copy_index_v3_locations other WHERE other.contract_key=head.contract_key AND other.blob_sha256=head.blob_sha256 AND other.node_id<>json_extract($1,'$.node_id') AND {}) LIMIT 1", holder_is_member("other")), body, false, true).await?;
    Ok(!rows.is_empty())
}
