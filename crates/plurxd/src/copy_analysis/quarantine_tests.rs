//! Discrepancy quarantine against a real SQLite store: the head decision for
//! a set of observations, a reader's typed defects, and the v3 eviction owner.
//!
//! What is real: the store and its coordination, quarantine and location
//! transactions; the blob codec; the blob files under a v3 root; the
//! fixture's HEVC muxer init.
//!
//! What is not:
//! - No test here runs `publish_source` itself. Its `PendingSource` carries an
//!   `AnalysisSourceInterestAuthority` only plurx-core's Source session store
//!   can construct, so the join between `reconcile_with_head`, the claim and
//!   the quarantine write is exercised in two halves: the decision here, and
//!   the claim-then-quarantine through `quarantine_defect`, which shares
//!   `coordination_request` and `quarantine_head` with it.
//! - Observed rows are merged directly, not through a playback range or an
//!   Analysis generation; no source is read and nothing is rebuilt.
//! - The playback-cache budget never calls the eviction owner; its two
//!   callbacks are called as the budget would.
use super::work::{quarantine_defect, reconcile_with_head, CopyIndexCacheOwner, HeadDecision};
use super::*;
use crate::playback_cache::{CacheCandidate, CacheOwner};
use plurx_core::fmp4::{CutClass, FragmentReader, Unit};
use plurx_core::segplan::{FragmentIndex, SourceIdentity};
use plurx_core::store::{
    cluster_fragment_index_blob_sha256, cluster_fragment_index_output_identity,
    cluster_fragment_index_v3_key, copy_index_v3_rebuild_generation, copy_index_v3_source_key,
    encode_cluster_fragment_index_blob_v3, CopyIndexV3Artifact, CopyIndexV3HeadState,
    FragmentIndexEngineProvenance, SqliteStore, COPY_INDEX_V3_NO_HOLDER_CODE,
};

const NODE: &str = "origin-node";
const NAMESPACE: &str = "catalogue";
const NOW: i64 = 1_000_000;
const SOURCE_SHA256: &str = "5555555555555555555555555555555555555555555555555555555555555555";

fn hex64(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}

fn source() -> AnalysisCoordinationSource {
    AnalysisCoordinationSource {
        file_id: 100,
        source_size: 4096,
        source_mtime: 77,
        origin_node_id: NODE.into(),
        object_version: "dev:1:ino:2".into(),
        video_identity: "ab".repeat(16),
        contract_version: plurx_core::store::FRAGMENT_INDEX_CONTRACT,
        requested_generation: hex64('c'),
    }
}

/// The fixture pipe's HEVC muxer init, as a copy range observes it.
fn muxer_init() -> Arc<Init> {
    plurx_core::testfixtures::require_ffmpeg();
    let mut reader = FragmentReader::new();
    reader.push(&plurx_core::testfixtures::pipe("closed-gop"));
    let Some(Unit::Init(mut init)) = reader.next_unit().expect("fixture pipe") else {
        panic!("fixture init");
    };
    crate::copyseg::sanitize_stale_dolby_brand(&mut init);
    Arc::new(init)
}

fn rows(count: u64, bytes: u32) -> Vec<IndexRow> {
    (0..count)
        .map(|n| IndexRow {
            dts: n * 3_003,
            duration: 3_003,
            bytes,
            video_bytes: bytes - 100,
            class: CutClass::CleanIdr,
        })
        .collect()
}

fn inspections(rows: &[IndexRow]) -> Vec<CopyRowInspection> {
    rows.iter()
        .map(|row| CopyRowInspection {
            dts: row.dts,
            duration: row.duration,
            packets: 2,
            slices: 3,
            configuration_sha256: hex64('d'),
            hdr10_mastering: None,
            hdr10_light: None,
            hdr10_conflict: false,
        })
        .collect()
}

/// A set of this source holding `rows`, which begin the title and, when
/// `eof`, end it.
fn set_of(init: &Arc<Init>, rows: &[IndexRow], eof: bool) -> CopyObservationSet {
    let timescale = init.video().expect("video track").timescale;
    let origin = SourceTimestamp {
        dts: 0,
        time_base_num: 1,
        time_base_den: timescale,
    };
    let promotion = PromotionInputs::default();
    let mut set =
        CopyObservationSet::empty(NAMESPACE, &source(), init, timescale, origin, 0, &promotion)
            .expect("an empty set of this source");
    let row_inspections = inspections(rows);
    let inspection = plurx_core::hevc_configuration::RangeInspection {
        packets: 2 * rows.len() as u64,
        slices: 3 * rows.len() as u64,
        configuration_sha256: hex64('d'),
    };
    set.merge(ObservedRows {
        init,
        timescale,
        source_origin: origin,
        canonical_title_start: 0,
        rows,
        boundaries: RangeBoundaries {
            begins_title: true,
            eof,
        },
        inspection: Some(&inspection),
        row_inspections: &row_inspections,
        promotion: &promotion,
    })
    .expect("observed rows");
    set
}

/// The index a seal of `rows` publishes, with `slices` in its HEVC proof.
fn sealed(init: &Init, rows: Vec<IndexRow>, slices: u64, indexed_hdr10: bool) -> FragmentIndex {
    let mut index = FragmentIndex::new(
        init.video().expect("video track").timescale,
        rows,
        hex64('c'),
        SourceIdentity::new(1, 1, "publisher-identity"),
    );
    index.promotion.indexed_hdr10_metadata = indexed_hdr10;
    index.promotion.hevc_configuration = Some(plurx_core::hevc_configuration::Proof {
        revision: plurx_core::hevc_configuration::REVISION,
        source_object_version: "dev:1:ino:2".into(),
        source_node_id: NODE.into(),
        source_sha256: Some(SOURCE_SHA256.to_owned()),
        stream_index: 0,
        slices,
        refusal: None,
    });
    index.parameter_sets_constant = true;
    index
}

fn variant(index: &FragmentIndex, engine_sha256: &str) -> (CopyIndexV3Artifact, Vec<u8>) {
    let init_contract = hex64('7');
    let engine = FragmentIndexEngineProvenance {
        engine_sha256: engine_sha256.to_owned(),
        pipeline_sha256: hex64('b'),
    };
    let blob = encode_cluster_fragment_index_blob_v3(index, SOURCE_SHA256, &init_contract, &engine)
        .expect("v3 blob");
    let output = cluster_fragment_index_output_identity(index, SOURCE_SHA256, &init_contract)
        .expect("output identity");
    let artifact = CopyIndexV3Artifact {
        blob_sha256: cluster_fragment_index_blob_sha256(&blob),
        contract_key: cluster_fragment_index_v3_key(NAMESPACE, 100, SOURCE_SHA256, &output)
            .expect("contract key"),
        catalog_namespace: NAMESPACE.to_owned(),
        file_id: 100,
        source_sha256: SOURCE_SHA256.to_owned(),
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
    context: CopyAnalysisContext,
    key: String,
}

async fn store_fixture() -> Fixture {
    let dir = crate::test_tempdir().expect("directory");
    let db = dir.path().join("plurx.db");
    let store = SqliteStore::open(&db).expect("store");
    rusqlite::Connection::open(&db)
        .expect("seed")
        .execute_batch(
            "INSERT INTO libraries (id, name, kind, paths, anime) VALUES (1, 'Films', 'movies', '[\"/films\"]', 0);
             INSERT INTO items (id, library_id, kind, title, sort_title) VALUES (10, 1, 'movie', 'Film', 'film');
             INSERT INTO files (id, item_id, path, size, mtime) VALUES (100, 10, '/films/film.mkv', 4096, 77);",
        )
        .expect("seed catalogue");
    let context = CopyAnalysisContext {
        store: Arc::new(store),
        node_id: NODE.into(),
        cache_dir: dir.path().join("cache"),
        checkpoint_root: None,
        lease_ms: 600_000,
        renew_every: std::time::Duration::from_secs(60),
        retry_ms: 1_000,
        metrics: Default::default(),
    };
    Fixture {
        _dir: dir,
        db,
        context,
        key: copy_index_v3_source_key(NAMESPACE, &source()).expect("source key"),
    }
}

impl Fixture {
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db).expect("fixture connection")
    }

    /// A healthy head at epoch 1 pointing at the variant, with the origin's
    /// holder row, as a publication at the observations' generation leaves it.
    fn publish(&self, artifact: &CopyIndexV3Artifact) {
        let source = source();
        let db = self.sql();
        db.execute(
            "INSERT INTO copy_index_v3_artifacts(blob_sha256,contract_key,catalog_namespace,file_id,source_sha256,contract_version,output_contract_sha256,init_contract_sha256,bytes,built_by_node_id,built_at_ms,producer_engine_sha256) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
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
                artifact.built_by_node_id,
                artifact.built_at_ms,
                artifact.producer_engine_sha256
            ],
        )
        .expect("variant row");
        db.execute(
            "INSERT INTO copy_index_v3_heads(source_key,catalog_namespace,file_id,origin_node_id,object_version,source_size,source_mtime,raw_video_identity,contract_version,desired_generation,head_epoch,state,contract_key,blob_sha256,published_generation,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,'healthy',?11,?12,?10,?13)",
            rusqlite::params![
                self.key,
                NAMESPACE,
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
        self.hold(artifact, NODE);
    }

    fn hold(&self, artifact: &CopyIndexV3Artifact, node: &str) {
        self.sql()
            .execute(
                "INSERT INTO copy_index_v3_locations(contract_key,blob_sha256,node_id,bytes,verified_at_ms,last_seen_at_ms) VALUES(?1,?2,?3,?4,?5,?5)",
                rusqlite::params![artifact.contract_key, artifact.blob_sha256, node, artifact.bytes, NOW],
            )
            .expect("holder row");
    }

    async fn install(&self, artifact: &CopyIndexV3Artifact, blob: &[u8]) -> PathBuf {
        let root = v3_root(&self.context.cache_dir);
        install_blob(&root, &artifact.contract_key, &artifact.blob_sha256, blob)
            .await
            .expect("local blob");
        v3_blob_path(&root, &artifact.blob_sha256).expect("blob path")
    }

    async fn head(&self) -> plurx_core::store::CopyIndexV3Head {
        self.context
            .store
            .copy_index_v3_head(&self.key)
            .await
            .expect("head read")
            .expect("head")
    }

    /// The ledger code the head's one quarantine was recorded with.
    fn quarantine_code(&self) -> Option<String> {
        use rusqlite::OptionalExtension;
        self.sql()
            .query_row(
                "SELECT code FROM copy_index_v3_quarantines WHERE source_key=?1",
                [&self.key],
                |row| row.get(0),
            )
            .optional()
            .expect("ledger")
    }

    fn defect(&self, artifact: &CopyIndexV3Artifact, kind: CopyIndexDefectKind) -> CopyIndexDefect {
        CopyIndexDefect {
            source_key: self.key.clone(),
            head_epoch: 1,
            blob_sha256: artifact.blob_sha256.clone(),
            kind,
        }
    }
}

/// Observed rows are compared with a published index of the same source row
/// by row, and only the observed rows are judged. Adopting a repair
/// generation changes only what the set seals for. Pure: no store, no blob
/// file.
#[test]
fn observations_contradict_a_published_index_only_where_they_differ() {
    let init = muxer_init();
    let all = rows(4, 4_500);
    let published = sealed(&init, all.clone(), 12, true);

    let head = set_of(&init, &all[..2], false);
    assert_eq!(head.contradiction(&published), None, "an agreeing head");
    let whole = set_of(&init, &all, true);
    assert_eq!(whole.contradiction(&published), None, "the whole title");

    let mut other = all.clone();
    other[1].bytes += 1;
    assert!(
        set_of(&init, &other[..2], false)
            .contradiction(&published)
            .is_some_and(|reason| reason.contains("differs")),
        "an overlapping row that is not the published one"
    );
    assert!(
        whole
            .contradiction(&sealed(&init, all[..3].to_vec(), 9, true))
            .is_some(),
        "a row the published index does not have"
    );
    assert_eq!(
        whole.contradiction(&sealed(&init, all.clone(), 11, true)),
        None,
        "totals the rows do not carry are not this check's"
    );
    let mut retimed = published.clone();
    retimed.timescale += 1;
    assert!(whole.contradiction(&retimed).is_some());

    let mut adopted = set_of(&init, &all[..2], false);
    adopted.adopt_generation(&hex64('9'));
    assert_eq!(adopted.source().requested_generation, hex64('9'));
    assert_eq!(adopted.observed_rows(), 2, "the rows stay");
    assert_eq!(adopted.contradiction(&published), None);
}

/// The head decision of a set of observations, against a real store and the
/// origin's installed blob. Covers items 1 and 2 up to the claim: a healthy
/// head agreeing with the rows is done whichever generation built it; rows
/// that differ from a variant another engine built prove nothing; a variant
/// this engine built is contradicted, names its exact epoch and blob, and
/// the set adopts the repair generation; a quarantined head's desire is
/// adopted only when it is that same computed generation.
#[tokio::test]
async fn a_set_adopts_only_the_repair_generation_of_the_head_it_contradicts() {
    let init = muxer_init();
    let all = rows(4, 4_500);
    let mut other = all.clone();
    other[1].bytes += 1;

    // Another engine's variant: its rows may differ from this engine's cut.
    let foreign = store_fixture().await;
    let (theirs, blob) = variant(&sealed(&init, all.clone(), 12, true), &hex64('e'));
    foreign.publish(&theirs);
    foreign.install(&theirs, &blob).await;
    let mut differing = set_of(&init, &other[..2], false);
    assert_eq!(
        reconcile_with_head(&foreign.context, &foreign.key, &mut differing).await,
        Ok(HeadDecision::Done),
        "a difference between two engines is not a contradiction"
    );
    assert_eq!(differing.source().requested_generation, hex64('c'));

    let fixture = store_fixture().await;
    let engine = crate::ffmpeg::fragment_index_engine_digest().await;
    let (artifact, blob) = variant(&sealed(&init, all.clone(), 12, true), &engine);
    let successor =
        copy_index_v3_rebuild_generation(&fixture.key, &artifact.blob_sha256, 1).expect("repair");

    // No head yet: the set seals for its own generation.
    let mut set = set_of(&init, &all[..2], false);
    assert_eq!(
        reconcile_with_head(&fixture.context, &fixture.key, &mut set).await,
        Ok(HeadDecision::Proceed { contradicted: None })
    );

    // A healthy head whose blob is not on this node proves nothing.
    fixture.publish(&artifact);
    assert_eq!(
        reconcile_with_head(&fixture.context, &fixture.key, &mut set).await,
        Ok(HeadDecision::Done)
    );
    fixture.install(&artifact, &blob).await;
    assert_eq!(
        reconcile_with_head(&fixture.context, &fixture.key, &mut set).await,
        Ok(HeadDecision::Done),
        "agreeing rows"
    );
    assert_eq!(set.source().requested_generation, hex64('c'));

    // Fresh rows that are not the ones this same engine published.
    let mut contradicting = set_of(&init, &other[..2], false);
    assert_eq!(
        reconcile_with_head(&fixture.context, &fixture.key, &mut contradicting).await,
        Ok(HeadDecision::Proceed {
            contradicted: Some((1, artifact.blob_sha256.clone()))
        })
    );
    assert_eq!(contradicting.source().requested_generation, successor);
    assert_eq!(
        fixture.head().await.state,
        CopyIndexV3HeadState::Healthy,
        "the decision writes nothing"
    );

    // Quarantined at exactly that blob and epoch: a new set adopts the desire.
    fixture
        .sql()
        .execute(
            "UPDATE copy_index_v3_heads SET state='quarantined',desired_generation=?1 WHERE source_key=?2",
            rusqlite::params![successor, fixture.key],
        )
        .expect("quarantined head");
    let mut later = set_of(&init, &all[..2], false);
    assert_eq!(
        reconcile_with_head(&fixture.context, &fixture.key, &mut later).await,
        Ok(HeadDecision::Proceed { contradicted: None })
    );
    assert_eq!(later.source().requested_generation, successor);

    // A desire no quarantine of this blob and epoch computes is not adopted.
    fixture
        .sql()
        .execute(
            "UPDATE copy_index_v3_heads SET desired_generation=?1 WHERE source_key=?2",
            rusqlite::params![hex64('8'), fixture.key],
        )
        .expect("foreign desire");
    let mut refused = set_of(&init, &all[..2], false);
    assert_eq!(
        reconcile_with_head(&fixture.context, &fixture.key, &mut refused).await,
        Ok(HeadDecision::Discard("head desires another generation"))
    );
    assert_eq!(refused.source().requested_generation, hex64('c'));
}

/// A reader's holderless report quarantines the exact head under a real
/// coordination claim on its repair generation, with its own ledger code, and
/// ends the borrowed request as a stranded coordinator, uncharged. A holder that is still listed, or a head
/// that has moved, keeps the head. The quarantined pointer keeps its blob
/// and epoch but is not readable.
#[tokio::test]
async fn a_holderless_head_is_quarantined_under_a_claim_and_its_borrowed_request_ended() {
    let fixture = store_fixture().await;
    let init = muxer_init();
    let (artifact, _) = variant(&sealed(&init, rows(4, 4_500), 12, true), &hex64('e'));
    fixture.publish(&artifact);
    let defect = fixture.defect(&artifact, CopyIndexDefectKind::NoHolder);

    assert_eq!(
        quarantine_defect(&fixture.context, &defect).await,
        Ok(false),
        "the origin is still a listed holder"
    );
    let mut stale = defect.clone();
    stale.head_epoch = 2;
    fixture
        .sql()
        .execute("DELETE FROM copy_index_v3_locations", [])
        .expect("no holder");
    assert_eq!(quarantine_defect(&fixture.context, &stale).await, Ok(false));
    assert_eq!(fixture.head().await.state, CopyIndexV3HeadState::Healthy);

    assert_eq!(quarantine_defect(&fixture.context, &defect).await, Ok(true));
    let head = fixture.head().await;
    let successor =
        copy_index_v3_rebuild_generation(&fixture.key, &artifact.blob_sha256, 1).expect("repair");
    assert_eq!(head.state, CopyIndexV3HeadState::Quarantined);
    assert_eq!(head.desired_generation, successor);
    assert_eq!(
        (head.head_epoch, head.blob_sha256.as_deref()),
        (1, Some(artifact.blob_sha256.as_str())),
        "the pointer is kept for diagnosis"
    );
    assert_eq!(
        fixture.quarantine_code().as_deref(),
        Some(COPY_INDEX_V3_NO_HOLDER_CODE)
    );
    let variant = fixture
        .context
        .store
        .copy_index_v3_variant(&fixture.key, NODE)
        .await
        .expect("variant read")
        .expect("variant");
    assert!(
        variant.readable().is_none(),
        "a quarantined pointer is not readable before its successor publishes"
    );
    let (state, code, generation, attempts): (String, String, String, i64) = fixture
        .sql()
        .query_row(
            "SELECT state,last_error_code,requested_generation,attempts FROM analysis_requests",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the one coordination request");
    assert_eq!(
        (state.as_str(), code.as_str(), generation, attempts),
        ("cancelled", "coordination_stranded", successor, 0),
        "the borrowed request is ended, not left queued for a worker that may never run"
    );
    assert_eq!(
        quarantine_defect(&fixture.context, &defect).await,
        Ok(false),
        "a second report of the same head finds it quarantined"
    );
}

/// A blob sealed without the whole-title HDR10 fold is quarantined only when
/// this node's own verified copy shows it; a sealed-init refusal only when
/// this node's engine is the variant's producer engine. Each has its own
/// ledger code.
#[tokio::test]
async fn an_unindexed_blob_and_a_same_engine_sealed_init_each_quarantine_with_their_code() {
    let init = muxer_init();
    let all = rows(4, 4_500);

    // Sealed without the fold, by another engine than this node's.
    let fixture = store_fixture().await;
    let (unindexed, blob) = variant(&sealed(&init, all.clone(), 12, false), &hex64('e'));
    fixture.publish(&unindexed);
    let hdr10 = fixture.defect(&unindexed, CopyIndexDefectKind::UnindexedHdr10);
    assert_eq!(
        quarantine_defect(&fixture.context, &hdr10).await,
        Ok(false),
        "no local copy to check the report against"
    );
    assert_eq!(
        quarantine_defect(
            &fixture.context,
            &fixture.defect(&unindexed, CopyIndexDefectKind::SealedInit)
        )
        .await,
        Ok(false),
        "another engine's init says nothing about the variant"
    );
    assert_eq!(fixture.head().await.state, CopyIndexV3HeadState::Healthy);
    fixture.install(&unindexed, &blob).await;
    assert_eq!(quarantine_defect(&fixture.context, &hdr10).await, Ok(true));
    assert_eq!(
        fixture.quarantine_code().as_deref(),
        Some("copy_index_hdr10_unindexed")
    );

    // Sealed with the fold by exactly this node's engine.
    let fixture = store_fixture().await;
    let engine = crate::ffmpeg::fragment_index_engine_digest().await;
    let (own, blob) = variant(&sealed(&init, all, 12, true), &engine);
    fixture.publish(&own);
    fixture.install(&own, &blob).await;
    assert_eq!(
        quarantine_defect(
            &fixture.context,
            &fixture.defect(&own, CopyIndexDefectKind::UnindexedHdr10)
        )
        .await,
        Ok(false),
        "the verified copy carries the fold"
    );
    assert_eq!(
        quarantine_defect(
            &fixture.context,
            &fixture.defect(&own, CopyIndexDefectKind::SealedInit)
        )
        .await,
        Ok(true)
    );
    assert_eq!(
        fixture.quarantine_code().as_deref(),
        Some(plurx_core::store::COPY_INDEX_V3_SEALED_INIT_CODE)
    );
}

/// The v3 eviction owner offers exactly this root's blobs. It refuses to
/// evict the bytes of a readable head this node is the last member holding
/// (a removed node's row does not count), and otherwise removes the blob and
/// forgets this node's holder rows for it under every contract.
#[tokio::test]
async fn evicting_a_v3_blob_forgets_this_nodes_holder_rows_for_it() {
    let fixture = store_fixture().await;
    let init = muxer_init();
    let (artifact, blob) = variant(&sealed(&init, rows(4, 4_500), 12, true), &hex64('e'));
    fixture.publish(&artifact);
    // The same bytes named by a second logical contract.
    let mut alias = artifact.clone();
    alias.contract_key = hex64('a');
    fixture.hold(&alias, NODE);
    let path = fixture.install(&artifact, &blob).await;
    let root = v3_root(&fixture.context.cache_dir);
    let stray = path.with_file_name("notes.txt");
    std::fs::write(&stray, b"not a variant").expect("stray file");

    let owner = CopyIndexCacheOwner {
        root: root.clone(),
        store: Arc::clone(&fixture.context.store),
        node: NODE.into(),
    };
    let candidates = owner.candidates().await.expect("candidates");
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect::<Vec<_>>(),
        vec![path.clone()],
        "only variants are offered"
    );
    let outside = CacheCandidate {
        path: stray.clone(),
        last_used: std::time::SystemTime::now(),
    };
    assert!(!owner.evict(&outside).await.expect("refusal"));
    assert!(stray.exists());

    // The origin is the head's only holder: its copy is not evictable.
    assert!(!owner.evict(&candidates[0]).await.expect("refusal"));
    assert!(path.exists(), "the last holder keeps the bytes");
    fixture.hold(&artifact, "node-two");
    let removed = "internal.cluster_job_owner_removed.node-two";
    fixture
        .sql()
        .execute("INSERT INTO settings(key,value) VALUES(?1,'1')", [removed])
        .expect("removal fence");
    assert!(
        !owner.evict(&candidates[0]).await.expect("refusal"),
        "a node the cluster removed holds nothing"
    );
    fixture
        .sql()
        .execute("DELETE FROM settings WHERE key=?1", [removed])
        .expect("a member again");

    assert!(owner.evict(&candidates[0]).await.expect("eviction"));
    assert!(!path.exists());
    let holders = |contract: &str| {
        let store = Arc::clone(&fixture.context.store);
        let (contract, blob) = (contract.to_owned(), artifact.blob_sha256.clone());
        async move {
            store
                .copy_index_v3_locations(&contract, &blob)
                .await
                .expect("holders")
                .into_iter()
                .map(|location| location.node_id)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(holders(&artifact.contract_key).await, vec!["node-two"]);
    assert!(holders(&alias.contract_key).await.is_empty());
    assert_eq!(
        fixture.head().await.state,
        CopyIndexV3HeadState::Healthy,
        "the head is untouched; its next reader finds whether it has a holder"
    );
    assert!(
        !owner.evict(&candidates[0]).await.expect("already gone"),
        "an evicted blob is not evicted twice"
    );
}
