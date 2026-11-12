use super::*;
use plurx_core::fmp4::{CutClass, FragmentReader, PromotionInputs, Unit};
use plurx_core::segplan::{IndexRow, SourceIdentity};
use plurx_core::store::{
    cluster_fragment_index_blob_sha256, cluster_fragment_index_output_identity,
    cluster_fragment_index_v3_key, copy_index_v3_source_key, encode_cluster_fragment_index_blob_v3,
    BackgroundJobStore, FragmentIndexEngineProvenance, SettingsStore, SqliteStore,
};
use std::path::PathBuf;

const NODE: &str = "node-one";
const NOW: i64 = 1_000_000;

fn hex64(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}

fn rows() -> Vec<IndexRow> {
    (0..3u64)
        .map(|n| IndexRow {
            dts: n * 3_003,
            duration: 3_003,
            bytes: 4_500,
            video_bytes: 3_100,
            class: CutClass::CleanIdr,
        })
        .collect()
}

/// A sealed index as the publisher builds it: whole-title HDR10 promised and
/// an HEVC proof bound to the full source digest.
fn sealed_index(source_sha256: &str, indexed_hdr10: bool) -> FragmentIndex {
    let mut index = FragmentIndex::new(
        90_000,
        rows(),
        hex64('c'),
        SourceIdentity::new(1, 1, "publisher-identity"),
    );
    index.promotion.indexed_hdr10_metadata = indexed_hdr10;
    index.promotion.hevc_configuration = Some(plurx_core::hevc_configuration::Proof {
        revision: plurx_core::hevc_configuration::REVISION,
        source_object_version: "publisher-object".into(),
        source_node_id: "publisher-node".into(),
        source_sha256: Some(source_sha256.to_owned()),
        stream_index: 0,
        slices: 9,
        refusal: None,
    });
    index.parameter_sets_constant = true;
    index
}

fn variant(
    namespace: &str,
    file_id: i64,
    index: &FragmentIndex,
    source_sha256: &str,
) -> (CopyIndexV3Artifact, Vec<u8>) {
    let init_contract = hex64('7');
    let engine = FragmentIndexEngineProvenance {
        engine_sha256: hex64('e'),
        pipeline_sha256: hex64('b'),
    };
    let blob = encode_cluster_fragment_index_blob_v3(index, source_sha256, &init_contract, &engine)
        .expect("v3 blob");
    let output = cluster_fragment_index_output_identity(index, source_sha256, &init_contract)
        .expect("output identity");
    let artifact = CopyIndexV3Artifact {
        blob_sha256: cluster_fragment_index_blob_sha256(&blob),
        contract_key: cluster_fragment_index_v3_key(namespace, file_id, source_sha256, &output)
            .expect("contract key"),
        catalog_namespace: namespace.to_owned(),
        file_id,
        source_sha256: source_sha256.to_owned(),
        contract_version: output.contract_version,
        output_contract_sha256: output.keyframe_table_sha256,
        init_contract_sha256: init_contract,
        bytes: blob.len() as i64,
        built_by_node_id: NODE.into(),
        built_at_ms: NOW,
        producer_engine_sha256: engine.engine_sha256,
    };
    artifact.validate().expect("valid variant");
    (artifact, blob)
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    cache: PathBuf,
    store: SqliteStore,
    namespace: String,
    file: MediaFile,
    video: CopyVideoOptions,
    object_version: String,
    /// The Analysis worker's inbox, where the reader reports defects.
    defects: CopyAnalysisRegistry,
}

fn media_file(path: PathBuf) -> MediaFile {
    let metadata = std::fs::metadata(&path).expect("source metadata");
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 100,
        item_id: 10,
        path,
        size: metadata.len() as i64,
        mtime: metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix mtime")
            .as_secs() as i64,
        duration_ms: Some(12_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main".into()),
        width: Some(640),
        height: Some(360),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(1_000_000),
        audio_streams: Vec::new(),
        subtitle_streams: Vec::new(),
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    }
}

async fn fixture() -> Fixture {
    let dir = crate::test_tempdir().expect("directory");
    let db = dir.path().join("plurx.db");
    let store = SqliteStore::open(&db).expect("store");
    let source = dir.path().join("film.mkv");
    std::fs::write(&source, b"the catalogued source object").expect("source");
    let file = media_file(source);
    let object_version = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .expect("object version");
    let namespace = store.instance_id().await.expect("namespace");
    let cache = dir.path().join("cache");
    Fixture {
        _dir: dir,
        db,
        cache,
        store,
        namespace,
        file,
        video: CopyVideoOptions::new(false, false),
        object_version,
        defects: CopyAnalysisRegistry::default(),
    }
}

impl Fixture {
    /// Seed a healthy head for `origin`/`object_version` pointing at the
    /// variant, with the publisher's location, as a publication leaves them.
    fn publish(&self, origin: &str, object_version: &str, artifact: &CopyIndexV3Artifact) {
        let identity = crate::fragindex::identity_for(&self.file, self.video);
        let source = crate::copy_analysis::source_for_copy(
            &self.namespace,
            &self.file,
            &identity.argv_fingerprint,
            object_version,
            origin,
        )
        .expect("copy source");
        let key = copy_index_v3_source_key(&self.namespace, &source).expect("source key");
        let db = rusqlite::Connection::open(&self.db).expect("seed");
        db.execute(
            "INSERT INTO copy_index_v3_artifacts(blob_sha256,contract_key,catalog_namespace,file_id,source_sha256,contract_version,output_contract_sha256,init_contract_sha256,bytes,built_by_node_id,built_at_ms,producer_engine_sha256) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT DO NOTHING",
            rusqlite::params![
                artifact.blob_sha256,
                artifact.contract_key,
                artifact.catalog_namespace,
                artifact.file_id,
                artifact.source_sha256,
                artifact.contract_version,
                artifact.output_contract_sha256,
                artifact.init_contract_sha256,
                artifact.bytes,
                origin,
                artifact.built_at_ms,
                artifact.producer_engine_sha256
            ],
        )
        .expect("variant row");
        db.execute(
            "INSERT INTO copy_index_v3_heads(source_key,catalog_namespace,file_id,origin_node_id,object_version,source_size,source_mtime,raw_video_identity,contract_version,desired_generation,head_epoch,state,contract_key,blob_sha256,published_generation,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,'healthy',?11,?12,?10,?13)",
            rusqlite::params![
                key,
                self.namespace,
                source.file_id,
                source.origin_node_id,
                source.object_version,
                source.source_size,
                source.source_mtime,
                source.video_identity,
                source.contract_version,
                source.requested_generation,
                artifact.contract_key,
                artifact.blob_sha256,
                NOW
            ],
        )
        .expect("healthy head");
        db.execute(
            "INSERT INTO copy_index_v3_locations(contract_key,blob_sha256,node_id,bytes,verified_at_ms,last_seen_at_ms) VALUES(?1,?2,?3,?4,?5,?5)",
            rusqlite::params![artifact.contract_key, artifact.blob_sha256, origin, artifact.bytes, NOW],
        )
        .expect("publisher location");
    }

    async fn install(&self, artifact: &CopyIndexV3Artifact, blob: &[u8]) {
        crate::copy_analysis::install_blob(
            &crate::copy_analysis::v3_root(&self.cache),
            &artifact.contract_key,
            &artifact.blob_sha256,
            blob,
        )
        .await
        .expect("local blob");
    }

    async fn resolve(&self) -> Option<ResolvedCopyIndex> {
        resolve(
            &self.store,
            None,
            NODE,
            &self.namespace,
            &self.cache,
            &self.file,
            self.video,
            Some(&self.defects),
        )
        .await
        .expect("resolution")
    }

    /// Every defect the reader has reported since the last call.
    fn reported(&self) -> Vec<CopyIndexDefect> {
        self.defects
            .take_pending()
            .into_iter()
            .filter_map(|message| match message {
                crate::copy_analysis::CopyAnalysisMessage::Defect(defect) => Some(defect),
                crate::copy_analysis::CopyAnalysisMessage::Range(_)
                | crate::copy_analysis::CopyAnalysisMessage::Demand(_) => None,
            })
            .collect()
    }

    /// The source key of this node's own head.
    fn own_key(&self) -> String {
        let identity = crate::fragindex::identity_for(&self.file, self.video);
        let source = crate::copy_analysis::source_for_copy(
            &self.namespace,
            &self.file,
            &identity.argv_fingerprint,
            &self.object_version,
            NODE,
        )
        .expect("copy source");
        copy_index_v3_source_key(&self.namespace, &source).expect("source key")
    }

    /// This node's own head, quarantined while still pointing at the variant.
    fn quarantine_own_head(&self, artifact: &CopyIndexV3Artifact) {
        let identity = crate::fragindex::identity_for(&self.file, self.video);
        let source = crate::copy_analysis::source_for_copy(
            &self.namespace,
            &self.file,
            &identity.argv_fingerprint,
            &self.object_version,
            NODE,
        )
        .expect("copy source");
        let key = copy_index_v3_source_key(&self.namespace, &source).expect("source key");
        rusqlite::Connection::open(&self.db)
            .expect("seed")
            .execute(
                "INSERT INTO copy_index_v3_heads(source_key,catalog_namespace,file_id,origin_node_id,object_version,source_size,source_mtime,raw_video_identity,contract_version,desired_generation,head_epoch,state,contract_key,blob_sha256,published_generation,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,'quarantined',?11,?12,?13,?14)",
                rusqlite::params![
                    key,
                    self.namespace,
                    source.file_id,
                    source.origin_node_id,
                    source.object_version,
                    source.source_size,
                    source.source_mtime,
                    source.video_identity,
                    source.contract_version,
                    hex64('f'),
                    artifact.contract_key,
                    artifact.blob_sha256,
                    source.requested_generation,
                    NOW
                ],
            )
            .expect("quarantined head");
    }

    async fn holders(&self, artifact: &CopyIndexV3Artifact) -> Vec<String> {
        self.store
            .copy_index_v3_locations(&artifact.contract_key, &artifact.blob_sha256)
            .await
            .expect("locations")
            .into_iter()
            .map(|location| location.node_id)
            .collect()
    }
}

/// The origin node plans from its own published head: the exact source key,
/// the local verified blob, and an index bound to this node's object.
#[tokio::test]
async fn the_origin_plans_from_its_exact_head_and_local_blob() {
    let fixture = fixture().await;
    assert!(
        fixture.resolve().await.is_none(),
        "no head: the caller keeps its existing index order"
    );
    let source_sha256 = hex64('5');
    let (artifact, blob) = variant(
        &fixture.namespace,
        fixture.file.id,
        &sealed_index(&source_sha256, true),
        &source_sha256,
    );
    fixture.publish(NODE, &fixture.object_version, &artifact);
    assert!(
        fixture.resolve().await.is_none(),
        "a head whose bytes no reachable holder has is not usable"
    );
    fixture.install(&artifact, &blob).await;
    let resolved = fixture.resolve().await.expect("the published index");
    assert_eq!(resolved.index.rows, rows());
    assert_eq!(resolved.object_version, fixture.object_version);
    assert_eq!(
        resolved.index.source,
        crate::fragindex::identity_for(&fixture.file, fixture.video),
        "the index carries this node's planning identity, not the publisher's"
    );
    assert_eq!(
        resolved.rendition_cache_key,
        format!("copy-index-v3:{}", artifact.contract_key)
    );
    assert_eq!(
        resolved.init_contract_sha256, artifact.init_contract_sha256,
        "the plan carries the contract its generations must serve"
    );
    let proof = resolved
        .index
        .promotion
        .hevc_configuration
        .expect("HEVC proof");
    assert!(
        proof.permits_on_node(&fixture.object_version, NODE),
        "the proof is bound to this node's current object"
    );

    // A holder row another node retired is restored by the next verified
    // local hit, from what the head read already said.
    assert!(fixture
        .store
        .forget_copy_index_v3_location(&artifact.contract_key, &artifact.blob_sha256, NODE)
        .await
        .expect("a peer retires this node's row"));
    assert!(fixture.resolve().await.is_some());
    assert_eq!(fixture.holders(&artifact).await, vec![NODE.to_owned()]);

    // A local blob that is not the catalogued bytes is discarded, never used.
    let path = crate::copy_analysis::v3_blob_path(
        &crate::copy_analysis::v3_root(&fixture.cache),
        &artifact.blob_sha256,
    )
    .expect("blob path");
    let mut corrupt = blob.clone();
    *corrupt.last_mut().expect("rows") ^= 1;
    std::fs::write(&path, corrupt).expect("corrupt the blob");
    assert!(fixture.resolve().await.is_none());
    assert!(!path.exists(), "the corrupt blob is removed");
    assert!(
        fixture.holders(&artifact).await.is_empty(),
        "and this node no longer claims to hold it"
    );

    // An engine that emitted another init than the sealed one stops this
    // process planning from the contract; the caller uses its next index.
    fixture.install(&artifact, &blob).await;
    assert!(fixture.resolve().await.is_some());
    refuse_on_this_engine(&resolved.rendition_cache_key);
    assert!(fixture.resolve().await.is_none());
}

/// Another origin's head describes this node's bytes only when this node's
/// own full-source receipt names the same content.
#[tokio::test]
async fn another_origins_head_needs_this_nodes_own_full_source_receipt() {
    let fixture = fixture().await;
    let source_sha256 = hex64('5');
    let (artifact, blob) = variant(
        &fixture.namespace,
        fixture.file.id,
        &sealed_index(&source_sha256, true),
        &source_sha256,
    );
    fixture.publish("node-two", "s1:another:object", &artifact);
    fixture.install(&artifact, &blob).await;
    assert!(
        fixture.resolve().await.is_none(),
        "an opaque foreign object version is not proof of equivalence"
    );
    let receipt = |object_version: String, sha256: String| {
        rusqlite::Connection::open(&fixture.db)
            .expect("seed")
            .execute(
                "INSERT OR REPLACE INTO cluster_fragment_index_sources(node_id,file_id,object_version,source_size,source_mtime,source_sha256,observed_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![
                    NODE,
                    fixture.file.id,
                    object_version,
                    fixture.file.size,
                    fixture.file.mtime,
                    sha256,
                    NOW
                ],
            )
            .expect("source receipt");
    };
    // A sampled receipt is stored under the bare object version: not full.
    receipt(fixture.object_version.clone(), source_sha256.clone());
    assert!(fixture.resolve().await.is_none(), "a sampled receipt");
    let full = crate::fragment_index_cluster::full_copy_object_version(&fixture.object_version);
    receipt(full.clone(), hex64('6'));
    assert!(fixture.resolve().await.is_none(), "different content");
    receipt(full, source_sha256);
    let resolved = fixture.resolve().await.expect("equivalent content");
    assert_eq!(resolved.index.rows, rows());
    assert!(resolved
        .index
        .promotion
        .hevc_configuration
        .expect("HEVC proof")
        .permits_on_node(&fixture.object_version, NODE));
    let mut holders = fixture.holders(&artifact).await;
    holders.sort();
    assert_eq!(
        holders,
        vec![NODE.to_owned(), "node-two".to_owned()],
        "a verified local copy this node was not listed for is recorded"
    );

    // The blob this node's own head was quarantined for is never adopted
    // back through another origin's pointer.
    fixture.quarantine_own_head(&artifact);
    assert!(fixture.resolve().await.is_none());
}

/// A variant sealed before the whole-title HDR10 fold would serve another
/// init than the legacy index; it is refused and planning falls back.
#[tokio::test]
async fn a_variant_sealed_without_whole_title_hdr10_metadata_is_refused() {
    let fixture = fixture().await;
    let source_sha256 = hex64('5');
    let (artifact, blob) = variant(
        &fixture.namespace,
        fixture.file.id,
        &sealed_index(&source_sha256, false),
        &source_sha256,
    );
    assert_eq!(
        decode_verified(&blob, &artifact).expect_err("refused"),
        "sealed without whole-title HDR10 metadata"
    );
    fixture.publish(NODE, &fixture.object_version, &artifact);
    fixture.install(&artifact, &blob).await;
    assert!(fixture.resolve().await.is_none());
    assert!(
        crate::copy_analysis::v3_blob_path(
            &crate::copy_analysis::v3_root(&fixture.cache),
            &artifact.blob_sha256
        )
        .expect("blob path")
        .exists(),
        "refused, not removed: quarantine belongs to its own owner"
    );
}

#[test]
fn a_variant_decodes_only_as_the_exact_bytes_and_init_contract_of_its_row() {
    let source_sha256 = hex64('5');
    let (artifact, blob) = variant(
        "catalogue",
        100,
        &sealed_index(&source_sha256, true),
        &source_sha256,
    );
    let index = decode_verified(&blob, &artifact).expect("the row's own blob");
    assert_eq!(index.rows, rows());
    assert!(index.promotion.indexed_hdr10_metadata);

    let mut other_init = artifact.clone();
    other_init.init_contract_sha256 = hex64('8');
    assert_eq!(
        decode_verified(&blob, &other_init).expect_err("another init contract"),
        "init contract is not the one this index was sealed with"
    );
    let mut other_source = artifact.clone();
    other_source.source_sha256 = hex64('6');
    assert!(decode_verified(&blob, &other_source).is_err());
    let mut other_output = artifact.clone();
    other_output.output_contract_sha256 = hex64('9');
    assert!(decode_verified(&blob, &other_output).is_err());
    let mut truncated = blob.clone();
    truncated.pop();
    assert_eq!(
        decode_verified(&truncated, &artifact).expect_err("other bytes"),
        "blob checksum or size mismatch"
    );
}

/// The init a v3 index serves is the init its seal published the contract
/// of: VOD establishes its rendition identity with `promote_from` over the
/// index's stored promotion, and the seal hashes that same promotion of the
/// same muxer init. A legacy index of the title stores the same whole-title
/// promotion, so both serve one init.
#[test]
fn a_v3_index_serves_the_init_its_seal_published_the_contract_of() {
    fn light(max: u16, average: u16) -> Vec<u8> {
        let mut nal = vec![0x4e, 1, 144, 4];
        nal.extend(max.to_be_bytes());
        nal.extend(average.to_be_bytes());
        nal.push(0x80);
        nal
    }
    let mut reader = FragmentReader::new();
    reader.push(&plurx_core::testfixtures::pipe("closed-gop"));
    let Some(Unit::Init(mut muxer)) = reader.next_unit().expect("fixture pipe") else {
        panic!("fixture init");
    };
    // What every generation's pipe reader does before identity.
    crate::copyseg::sanitize_stale_dolby_brand(&mut muxer);
    for promotion in [
        PromotionInputs {
            indexed_hdr10_metadata: true,
            ..Default::default()
        },
        PromotionInputs {
            indexed_hdr10_metadata: true,
            hdr10_sei: vec![light(283, 200)],
            ..Default::default()
        },
    ] {
        let sealed = crate::copy_normalization::served_init_contract_sha256(&muxer, &promotion)
            .expect("the seal's init contract");
        let identity = crate::renditiondir::InitIdentity::establish(&muxer, promotion)
            .expect("the consumer's rendition identity");
        let served = identity
            .served_init_for(&muxer)
            .expect("the consumer's served init");
        assert_eq!(
            plurx_core::fmp4::copy_video_init_contract_sha256(&served).expect("served contract"),
            sealed
        );
    }
}

#[test]
fn the_peer_route_names_exactly_one_variant() {
    // A head's source key, then the blob digest it points at.
    let contract_key = hex64('a');
    let blob_sha256 = hex64('b');
    let path = peer_path(&contract_key, &blob_sha256);
    assert_eq!(
        peer_route_key(&path),
        Some((contract_key.as_str(), blob_sha256.as_str()))
    );
    for refused in [
        PEER_PATH_PREFIX.to_owned(),
        format!("{PEER_PATH_PREFIX}{contract_key}"),
        format!("{PEER_PATH_PREFIX}{contract_key}/"),
        format!("{PEER_PATH_PREFIX}{contract_key}/{blob_sha256}/extra"),
        format!("{PEER_PATH_PREFIX}{contract_key}/../{blob_sha256}"),
        format!("{PEER_PATH_PREFIX}{}/{blob_sha256}", "A".repeat(64)),
        format!("/internal/media/fragment-index/{contract_key}/{blob_sha256}"),
    ] {
        assert_eq!(peer_route_key(&refused), None, "{refused}");
    }
}

/// The reader hands the Analysis worker a typed defect for exactly the head
/// epoch and blob it met: no holder anywhere, or a verified blob sealed
/// without the HDR10 fold. A holder that merely cannot be asked is not a
/// defect, and a planned variant is remembered for a later sealed-init
/// refusal. Covers the reader's reporting only; the worker's quarantine of a
/// reported head runs against a real store in the copy-analysis tests.
#[tokio::test]
async fn the_reader_reports_a_holderless_head_and_an_unindexed_blob_as_typed_defects() {
    let fixture = fixture().await;
    let source_sha256 = hex64('5');
    let (artifact, blob) = variant(
        &fixture.namespace,
        fixture.file.id,
        &sealed_index(&source_sha256, false),
        &source_sha256,
    );
    fixture.publish(NODE, &fixture.object_version, &artifact);
    let defect = |kind| CopyIndexDefect {
        source_key: fixture.own_key(),
        head_epoch: 1,
        blob_sha256: artifact.blob_sha256.clone(),
        kind,
    };

    // Listed only for this node, whose bytes are gone: nobody holds them.
    assert!(fixture.resolve().await.is_none());
    assert_eq!(
        fixture.reported(),
        vec![defect(CopyIndexDefectKind::NoHolder)]
    );
    assert!(fixture.holders(&artifact).await.is_empty());

    // Another listed holder that cannot be asked may still have the bytes.
    rusqlite::Connection::open(&fixture.db)
        .expect("seed")
        .execute(
            "INSERT INTO copy_index_v3_locations(contract_key,blob_sha256,node_id,bytes,verified_at_ms,last_seen_at_ms) VALUES(?1,?2,'node-two',?3,?4,?4)",
            rusqlite::params![artifact.contract_key, artifact.blob_sha256, artifact.bytes, NOW],
        )
        .expect("another holder");
    assert!(fixture.resolve().await.is_none());
    assert!(fixture.reported().is_empty(), "unreachable is not absent");

    // The verified bytes themselves lack the whole-title HDR10 fold.
    fixture.install(&artifact, &blob).await;
    assert!(lacks_indexed_hdr10(&blob, &artifact));
    assert!(fixture.resolve().await.is_none());
    assert_eq!(
        fixture.reported(),
        vec![defect(CopyIndexDefectKind::UnindexedHdr10)]
    );
    assert_eq!(
        sealed_init_defect(&format!("copy-index-v3:{}", artifact.contract_key)),
        None,
        "a refused variant was never planned from"
    );
}

/// A variant the reader planned from is remembered by its rendition key, so
/// a generation that refuses the sealed init names the exact head epoch and
/// blob. Whether that is a defect of the variant is the worker's decision.
#[tokio::test]
async fn a_planned_variant_names_its_head_for_a_sealed_init_refusal() {
    let fixture = fixture().await;
    let source_sha256 = hex64('6');
    let (artifact, blob) = variant(
        &fixture.namespace,
        fixture.file.id,
        &sealed_index(&source_sha256, true),
        &source_sha256,
    );
    fixture.publish(NODE, &fixture.object_version, &artifact);
    fixture.install(&artifact, &blob).await;
    let resolved = fixture.resolve().await.expect("the published index");
    assert!(fixture.reported().is_empty(), "a usable head has no defect");
    assert!(!lacks_indexed_hdr10(&blob, &artifact));
    assert_eq!(
        sealed_init_defect(&resolved.rendition_cache_key),
        Some(CopyIndexDefect {
            source_key: fixture.own_key(),
            head_epoch: 1,
            blob_sha256: artifact.blob_sha256.clone(),
            kind: CopyIndexDefectKind::SealedInit,
        })
    );
}
