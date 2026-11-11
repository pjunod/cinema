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
