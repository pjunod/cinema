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
//! - A local demand is folded, handed to its unit and taken as far as the
//!   head decision and the coordination request. The build it then starts is
//!   a physical phase of the real copy engine, whose unseeded first
//!   generation runs in `gap_fill_tests`; the two are not run joined, and no
//!   VOD start emits the demand here.
use super::work::{
    coordination_request, forget_settled, publish_source, quarantine_defect, reconcile_demand,
    reconcile_with_head, recover_demands, work_copy_observations, Attempt, Coordination,
    CopyIndexCacheOwner, HeadDecision, PendingSources,
};
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

fn local_viewer(user_id: i64) -> crate::state::PlaybackViewerDemand {
    crate::state::PlaybackViewerDemand {
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id },
        playback_id: "playback-1".into(),
    }
}

/// The catalogue row of the fixture's file. Its path is never opened here.
fn media_file() -> plurx_core::domain::MediaFile {
    plurx_core::domain::MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 100,
        item_id: 10,
        path: PathBuf::from("/films/film.mkv"),
        size: 4096,
        mtime: 77,
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

/// A local viewer's row-less demand for the fixture's source, with
/// `origin` as the node that holds the object.
fn local_demand(origin: &str) -> CopyAnalysisMessage {
    CopyAnalysisMessage::Demand(Box::new(demand_for(origin, 100)))
}

fn demand_for(origin: &str, file_id: i64) -> CopyIndexDemand {
    let mut source = source();
    source.origin_node_id = origin.into();
    source.file_id = file_id;
    CopyIndexDemand::local(
        &local_viewer(7),
        Arc::new(media_file()),
        NAMESPACE.into(),
        source,
        CopyRecipe {
            video: plurx_core::transcode::CopyVideoOptions::new(false, false),
            audio_index: None,
            transcode_audio: false,
            audio_delivery: None,
        },
    )
    .expect("a local user's own playback")
}

const NO_WAIT: std::time::Duration = std::time::Duration::ZERO;

/// The local authority exists only for a local user's own playback: a
/// Sharing principal, whatever its grant, cannot be turned into one, so a
/// local demand never reuses a Source grant. Pure.
#[test]
fn a_local_copy_authority_is_never_made_from_a_sharing_principal() {
    let local = LocalCopyAuthority::of(&local_viewer(7)).expect("local playback");
    assert_eq!((local.user_id(), local.playback_id()), (7, "playback-1"));
    // The request a local demand runs under names who asked.
    let id = local.request_id();
    assert!(
        id.starts_with("local-play.u7.playback-1.") && id.len() <= 64,
        "{id}"
    );
    // A durable request gives back the authority it was made under.
    assert_eq!(LocalCopyAuthority::of_durable_request(&id), Some(local));
    assert_eq!(
        LocalCopyAuthority::of_durable_request(&uuid::Uuid::new_v4().to_string()),
        None,
        "any other request is not a local demand"
    );
    assert_eq!(
        LocalCopyAuthority::of_durable_request("local-play.u0.playback-1.ab12"),
        None
    );
    let mut unsafe_viewer = local_viewer(7);
    unsafe_viewer.playback_id = "a/b".into();
    let id = LocalCopyAuthority::of(&unsafe_viewer)
        .expect("local playback")
        .request_id();
    assert!(
        id.starts_with("local-play.u7.") && !id.contains('/') && id.len() <= 64,
        "a playback id that cannot sit in a URL path is left out: {id}"
    );
    assert!(LocalCopyAuthority::of(&local_viewer(0)).is_err(), "no user");

    let shared = crate::state::PlaybackViewerDemand {
        principal: plurx_core::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::new_v4(),
            &hex64('b'),
        )
        .expect("sharing principal"),
        playback_id: "playback-1".into(),
    };
    assert!(LocalCopyAuthority::of(&shared).is_err());
    assert!(
        CopyIndexDemand::local(
            &shared,
            Arc::new(media_file()),
            NAMESPACE.into(),
            source(),
            CopyRecipe {
                video: plurx_core::transcode::CopyVideoOptions::new(false, false),
                audio_index: None,
                transcode_audio: false,
                audio_delivery: None,
            },
        )
        .is_err(),
        "shared playback cannot ask as a local viewer"
    );
}

/// A row-less local demand is held as its own source, once, and handed to a
/// unit like any incomplete set. With a healthy head the unit is done
/// without a claim; a demand for an object another node holds is dropped.
/// Covers the worker up to the head decision; no phase is begun.
#[cfg(unix)]
#[tokio::test]
async fn a_local_demand_is_held_once_and_is_done_when_its_head_is_healthy() {
    let fixture = store_fixture().await;
    let init = muxer_init();
    let (artifact, _) = variant(&sealed(&init, rows(4, 4_500), 12, true), &hex64('e'));
    fixture.publish(&artifact);

    let mut pending = PendingSources::default();
    pending.fold(local_demand(NODE), NO_WAIT);
    pending.fold(local_demand(NODE), NO_WAIT);
    let mut units = pending.start_ready(std::time::Instant::now());
    assert_eq!(units.len(), 1, "one source, however often it is asked for");

    // A demand waits out the retry interval before its first attempt, so
    // it never runs against the start that raised it.
    let mut waiting = PendingSources::default();
    let wait = std::time::Duration::from_secs(60);
    waiting.fold(local_demand(NODE), wait);
    assert!(waiting.start_ready(std::time::Instant::now()).is_empty());
    assert_eq!(
        waiting.start_ready(std::time::Instant::now() + wait).len(),
        1
    );
    pending.fold(local_demand(NODE), NO_WAIT);
    assert!(
        pending.start_ready(std::time::Instant::now()).is_empty(),
        "a source being worked is not held a second time"
    );
    let (key, mut source) = units.pop().expect("the unit");
    assert_eq!(key, fixture.key);
    assert!(matches!(
        publish_source(&fixture.context, &key, &mut source).await,
        Attempt::Done
    ));
    assert_eq!(
        fixture
            .sql()
            .query_row("SELECT count(*) FROM analysis_requests", [], |row| row
                .get::<_, i64>(0))
            .expect("requests"),
        0,
        "a healthy head costs a demand one read and no request"
    );

    let mut elsewhere = PendingSources::default();
    elsewhere.fold(local_demand("node-two"), NO_WAIT);
    let (key, mut source) = elsewhere
        .start_ready(std::time::Instant::now())
        .pop()
        .expect("the unit");
    assert!(matches!(
        publish_source(&fixture.context, &key, &mut source).await,
        Attempt::Discard(_)
    ));
}

/// The head decision of a demand, which holds no rows: nothing to build on a
/// healthy head, the repair generation of a quarantined one adopted on the
/// same condition as for a set, any other desire refused.
#[tokio::test]
async fn a_demand_adopts_a_quarantined_heads_repair_generation() {
    let fixture = store_fixture().await;
    let init = muxer_init();
    let (artifact, _) = variant(&sealed(&init, rows(4, 4_500), 12, true), &hex64('e'));
    let successor =
        copy_index_v3_rebuild_generation(&fixture.key, &artifact.blob_sha256, 1).expect("repair");

    let mut asked = source();
    assert_eq!(
        reconcile_demand(&fixture.context, &fixture.key, &mut asked).await,
        Ok(HeadDecision::Proceed { contradicted: None }),
        "no head: the demand builds one"
    );
    fixture.publish(&artifact);
    assert_eq!(
        reconcile_demand(&fixture.context, &fixture.key, &mut asked).await,
        Ok(HeadDecision::Done)
    );
    assert_eq!(asked.requested_generation, hex64('c'));

    fixture
        .sql()
        .execute(
            "UPDATE copy_index_v3_heads SET state='quarantined',desired_generation=?1 WHERE source_key=?2",
            rusqlite::params![successor, fixture.key],
        )
        .expect("quarantined head");
    assert_eq!(
        reconcile_demand(&fixture.context, &fixture.key, &mut asked).await,
        Ok(HeadDecision::Proceed { contradicted: None })
    );
    assert_eq!(asked.requested_generation, successor);

    fixture
        .sql()
        .execute(
            "UPDATE copy_index_v3_heads SET desired_generation=?1 WHERE source_key=?2",
            rusqlite::params![hex64('8'), fixture.key],
        )
        .expect("foreign desire");
    let mut refused = source();
    assert_eq!(
        reconcile_demand(&fixture.context, &fixture.key, &mut refused).await,
        Ok(HeadDecision::Discard("head desires another generation"))
    );
}

/// A generation whose coordination request reached a terminal state is
/// reported as ended, so its source is dropped rather than asked about on
/// every pass; a queued one is this worker's and a claimed one is busy.
#[tokio::test]
async fn an_ended_generation_is_told_apart_from_a_busy_one() {
    let fixture = store_fixture().await;
    let source = source();
    let asked = LocalCopyAuthority::of(&local_viewer(7))
        .expect("local playback")
        .request_id();
    let coordinate = || async {
        coordination_request(&fixture.context, &source, uuid::Uuid::new_v4().to_string())
            .await
            .expect("coordination")
    };
    let Coordination::Queued(request) =
        coordination_request(&fixture.context, &source, asked.clone())
            .await
            .expect("coordination")
    else {
        panic!("a fresh generation is queued");
    };
    assert_eq!(
        request.request_id, asked,
        "the request is persisted under the id that names who asked"
    );
    let now = crate::state::clock_ms();
    fixture
        .context
        .store
        .claim_analysis_coordination(
            &request.request_id,
            source.clone(),
            NODE,
            request.fence,
            now,
            now + 600_000,
        )
        .await
        .expect("claim")
        .expect("metadata claim");
    assert!(matches!(coordinate().await, Coordination::Busy));
    fixture
        .sql()
        .execute(
            "UPDATE analysis_requests SET state='failed',owner_node_id=NULL,lease_expires_ms=NULL WHERE request_id=?1",
            [&request.request_id],
        )
        .expect("the attempt limit ended it");
    assert!(matches!(coordinate().await, Coordination::Ended));

    // Retired as stranded, the generation is free again: enqueue replaces
    // the request. (The same row returned under the active-request ceiling
    // is busy, never ended; the ceiling is not constructed here.)
    fixture
        .sql()
        .execute(
            "UPDATE analysis_requests SET state='cancelled',last_error_code='coordination_stranded' WHERE request_id=?1",
            [&request.request_id],
        )
        .expect("stranded");
    let Coordination::Queued(replacement) = coordinate().await else {
        panic!("a stranded generation is coordinated again");
    };
    assert_ne!(replacement.request_id, request.request_id);
}

/// A sealed-init mismatch bans its blob, so the engine that built the
/// variant does not rebuild it: it would seal the same bytes. Another
/// engine seals other bytes and adopts the repair generation.
#[tokio::test]
async fn a_demand_does_not_rebuild_its_own_engines_sealed_init_mismatch() {
    let init = muxer_init();
    for (own_engine, expected) in [
        (
            true,
            HeadDecision::Discard("this engine cannot rebuild its own sealed-init mismatch"),
        ),
        (false, HeadDecision::Proceed { contradicted: None }),
    ] {
        let fixture = store_fixture().await;
        let engine = if own_engine {
            crate::ffmpeg::fragment_index_engine_digest().await
        } else {
            hex64('e')
        };
        let (artifact, _) = variant(&sealed(&init, rows(4, 4_500), 12, true), &engine);
        let successor = copy_index_v3_rebuild_generation(&fixture.key, &artifact.blob_sha256, 1)
            .expect("repair");
        fixture.publish(&artifact);
        let db = fixture.sql();
        db.execute(
            "UPDATE copy_index_v3_heads SET state='quarantined',desired_generation=?1 WHERE source_key=?2",
            rusqlite::params![successor, fixture.key],
        )
        .expect("quarantined head");
        db.execute(
            "INSERT INTO copy_index_v3_quarantines(source_key,head_epoch,bad_blob_sha256,successor_generation,code,created_at_ms) VALUES(?1,1,?2,?3,?4,?5)",
            rusqlite::params![
                fixture.key,
                artifact.blob_sha256,
                successor,
                plurx_core::store::COPY_INDEX_V3_SEALED_INIT_CODE,
                NOW
            ],
        )
        .expect("ledger");
        let mut asked = source();
        assert_eq!(
            reconcile_demand(&fixture.context, &fixture.key, &mut asked).await,
            Ok(expected),
            "own engine: {own_engine}"
        );
        assert_eq!(asked.requested_generation == successor, !own_engine);
    }
}

/// A title asks once: a second demand for a source already sent is refused
/// before the inbox, and it may ask again once nothing holds its source. A
/// full table of demands gives up its oldest demand for a new one.
#[cfg(unix)]
#[test]
fn a_source_is_asked_for_once_until_its_work_settles() {
    let registry = CopyAnalysisRegistry::default();
    assert!(registry.notify_demand(demand_for(NODE, 100)));
    assert!(
        !registry.notify_demand(demand_for(NODE, 100)),
        "already asked"
    );
    assert!(
        registry.notify_demand(demand_for(NODE, 101)),
        "another source"
    );
    assert_eq!(registry.take_pending().len(), 2);

    // Nothing holds either source: both may ask again.
    forget_settled(&registry.asked(), &PendingSources::default());
    assert!(registry.notify_demand(demand_for(NODE, 100)));

    let mut pending = PendingSources::default();
    let key = |file_id: i64| {
        let demand = demand_for(NODE, file_id);
        copy_index_v3_source_key(&demand.catalog_namespace, &demand.source).expect("key")
    };
    for file_id in 1..=64 {
        pending.fold(
            CopyAnalysisMessage::Demand(Box::new(demand_for(NODE, file_id))),
            NO_WAIT,
        );
    }
    assert!(pending.holds(&key(1)) && pending.holds(&key(64)));
    pending.fold(
        CopyAnalysisMessage::Demand(Box::new(demand_for(NODE, 65))),
        NO_WAIT,
    );
    assert!(pending.holds(&key(65)));
    assert_eq!(
        (1..=65)
            .filter(|file_id| pending.holds(&key(*file_id)))
            .count(),
        64,
        "one demand left to make room"
    );
}

impl Fixture {
    /// The same store and node as `context`, as the dispatcher hands a pass.
    fn shared_context(&self) -> Arc<CopyAnalysisContext> {
        Arc::new(CopyAnalysisContext {
            store: Arc::clone(&self.context.store),
            node_id: self.context.node_id.clone(),
            cache_dir: self.context.cache_dir.clone(),
            checkpoint_root: None,
            lease_ms: self.context.lease_ms,
            renew_every: self.context.renew_every,
            retry_ms: self.context.retry_ms,
            metrics: Default::default(),
        })
    }

    /// The source a local copy start of the fixture's catalogue file asks
    /// for: the video identity of the plain copy recipe over the stored
    /// facts, and the fixture's recorded object version. No file exists at
    /// the catalogue path; nothing here may open it.
    async fn local_source(&self) -> (String, AnalysisCoordinationSource) {
        let store = &self.context.store;
        let file = store.get_file(100).await.expect("file read").expect("file");
        let video = plurx_core::transcode::CopyVideoOptions::from_probe(
            &file,
            None,
            crate::ffmpeg::has_dovi_rpu().await,
            false,
        );
        let namespace = store.instance_id().await.expect("namespace");
        let source = source_for_copy(
            &namespace,
            &file,
            &crate::fragindex::identity_for(&file, video).argv_fingerprint,
            "dev:1:ino:2",
            NODE,
        )
        .expect("copy source");
        (namespace, source)
    }

    /// Make every Analysis request look as if a previous process made it.
    fn age_requests(&self) {
        self.sql()
            .execute("UPDATE analysis_requests SET created_at_ms=1", [])
            .expect("aged requests");
    }

    fn request_state(&self, request_id: &str) -> (String, String) {
        self.sql()
            .query_row(
                "SELECT state,COALESCE(last_error_code,'') FROM analysis_requests WHERE request_id=?1",
                [request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("request")
    }
}

/// A demand outlives its process: the coordination request it made is found
/// by the next process, which rebuilds the demand from the catalogue row
/// and the request's own recorded object and video identity, without
/// opening the file (none exists at the fixture's path). A claim the dead
/// process still held is handed back to the queue. A user who no longer
/// exists is not named. A file that changed in the catalogue is left out.
/// The attributed case (an existing user) is covered only by the id parse.
#[tokio::test]
async fn a_restarted_worker_takes_up_the_demands_its_predecessor_left() {
    let fixture = store_fixture().await;
    let (namespace, source) = fixture.local_source().await;
    let asked = LocalCopyAuthority::of(&local_viewer(7)).expect("local playback");
    let Coordination::Queued(request) =
        coordination_request(&fixture.context, &source, asked.request_id())
            .await
            .expect("coordination")
    else {
        panic!("a fresh generation is queued");
    };
    // The predecessor died holding the claim.
    let now = crate::state::clock_ms();
    fixture
        .context
        .store
        .claim_analysis_coordination(
            &request.request_id,
            source.clone(),
            NODE,
            request.fence,
            now,
            now + 600_000,
        )
        .await
        .expect("claim")
        .expect("the predecessor's claim");
    let later = crate::state::clock_ms() + 60_000;

    let mut demands = recover_demands(&fixture.context, later)
        .await
        .expect("recovery");
    assert_eq!(demands.len(), 1);
    let demand = demands.pop().expect("the demand");
    assert_eq!(demand.source, source, "as playback observes it");
    assert_eq!(demand.catalog_namespace, namespace);
    assert_eq!(
        demand.durable_request.as_deref(),
        Some(request.request_id.as_str())
    );
    assert!(
        matches!(&demand.authority, CopyObservationAuthority::Local(local)
            if *local == LocalCopyAuthority::unattributed()),
        "user 7 is not in this catalogue: attribution is dropped, the build is not"
    );
    assert_eq!(
        crate::fragindex::identity_for(&demand.file, demand.recipe.video).argv_fingerprint,
        source.video_identity,
        "the recipe a fill of this source needs"
    );
    assert!(
        !demand.recipe.transcode_audio,
        "a file with no audio track asks for no encode"
    );
    assert!(
        plurx_core::playback::audio::copy_pipe_admits_audio("eac3")
            && !plurx_core::playback::audio::copy_pipe_admits_audio("truehd"),
        "a track the copy pipe cannot carry is encoded, never copied"
    );
    assert_eq!(
        fixture.request_state(&request.request_id).0,
        "queued",
        "the dead process's claim is handed back"
    );

    assert!(
        recover_demands(&fixture.context, 1)
            .await
            .expect("recovery")
            .is_empty(),
        "a request this process made is not a predecessor's"
    );
    fixture
        .sql()
        .execute("UPDATE files SET mtime=mtime+1 WHERE id=100", [])
        .expect("the file changed");
    assert!(
        recover_demands(&fixture.context, later)
            .await
            .expect("recovery")
            .is_empty(),
        "a demand for a file that changed is not taken up"
    );
}

/// The first pass of a process takes up what its predecessor left: the
/// demand is held, its request stays queued rather than being cancelled and
/// made again, and a coordination request with nothing behind it is
/// retired. The second pass does none of it again.
#[cfg(unix)]
#[tokio::test]
async fn the_first_pass_takes_up_durable_demands_and_keeps_their_requests() {
    let fixture = store_fixture().await;
    let (namespace, source) = fixture.local_source().await;
    let asked = LocalCopyAuthority::of(&local_viewer(7)).expect("local playback");
    let Coordination::Queued(durable) =
        coordination_request(&fixture.context, &source, asked.request_id())
            .await
            .expect("coordination")
    else {
        panic!("a fresh generation is queued");
    };
    let mut other = source.clone();
    other.requested_generation = hex64('d');
    let Coordination::Queued(stranded) =
        coordination_request(&fixture.context, &other, uuid::Uuid::new_v4().to_string())
            .await
            .expect("coordination")
    else {
        panic!("a fresh generation is queued");
    };
    fixture.age_requests();

    let registry = CopyAnalysisRegistry::default();
    work_copy_observations(&registry, fixture.shared_context()).await;
    let key = copy_index_v3_source_key(&namespace, &source).expect("key");
    assert!(registry.pending().lock().await.holds(&key));
    assert_eq!(
        fixture.request_state(&durable.request_id),
        ("queued".to_owned(), String::new()),
        "kept, not cancelled and made again"
    );
    assert_eq!(
        fixture.request_state(&stranded.request_id),
        ("cancelled".to_owned(), "coordination_stranded".to_owned())
    );

    // Once per process: a request aged after the first pass is left alone.
    let mut later = source.clone();
    later.requested_generation = hex64('9');
    let Coordination::Queued(later) =
        coordination_request(&fixture.context, &later, uuid::Uuid::new_v4().to_string())
            .await
            .expect("coordination")
    else {
        panic!("a fresh generation is queued");
    };
    fixture.age_requests();
    work_copy_observations(&registry, fixture.shared_context()).await;
    assert_eq!(fixture.request_state(&later.request_id).0, "queued");
}

/// A demand's request is made on its first attempt even while a viewer
/// holds the source, so a restart finds it; the attempt is then busy, and a
/// second attempt against the busy source makes no second request.
#[cfg(unix)]
#[tokio::test]
async fn a_demand_makes_its_request_before_its_source_is_idle() {
    let fixture = store_fixture().await;
    // A foreground owner of the whole source domain, as a playing viewer is.
    fixture
        .sql()
        .execute(
            "INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms) VALUES('viewer','{}','[\"source_io\"]',1,'startup','{}',?1,?1,?2)",
            rusqlite::params![NOW, i64::MAX],
        )
        .expect("a playing viewer");
    let requests = || {
        fixture
            .sql()
            .prepare("SELECT request_id,state FROM analysis_requests")
            .expect("statement")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("requests")
    };

    let mut pending = PendingSources::default();
    pending.fold(local_demand(NODE), NO_WAIT);
    let (key, mut source) = pending
        .start_ready(std::time::Instant::now())
        .pop()
        .expect("the unit");
    assert!(matches!(
        publish_source(&fixture.context, &key, &mut source).await,
        Attempt::Busy
    ));
    let made = requests();
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(
        made[0].0.starts_with("local-play.u7.playback-1.") && made[0].1 == "queued",
        "made before the busy source was looked at: {made:?}"
    );
    assert!(matches!(
        publish_source(&fixture.context, &key, &mut source).await,
        Attempt::Busy
    ));
    assert_eq!(requests(), made, "a busy source costs no further request");

    // Dropped unbuilt, the demand's request is named for retirement.
    pending.settle_for_test(key, source, &Attempt::Discard("dropped".into()));
    assert_eq!(pending.take_dropped(), vec![made[0].0.clone()]);
}

/// The whole-title rows a content-analysis pass hands over for the fixture's
/// source: `origin` holds the object, `custody` names the read.
fn pass_rows(
    origin: &str,
    custody: IndexPassCustody,
    init: &Arc<Init>,
    rows: Vec<IndexRow>,
    eof: bool,
) -> CopyAnalysisMessage {
    let timescale = init.video().expect("video track").timescale;
    let mut source = source();
    source.origin_node_id = origin.into();
    let file = media_file();
    let recipe = CopyRecipe::playback_default(
        &file,
        plurx_core::transcode::CopyVideoOptions::new(false, false),
    );
    CopyAnalysisMessage::Pass(Box::new(PassCopyObservation {
        file: Arc::new(file),
        catalog_namespace: NAMESPACE.into(),
        source,
        custody,
        recipe,
        rows: AnalysisCopyRows {
            init: Arc::clone(init),
            source_origin: SourceTimestamp {
                dts: 0,
                time_base_num: 1,
                time_base_den: timescale,
            },
            canonical_title_start: 0,
            boundaries: RangeBoundaries {
                begins_title: true,
                eof,
            },
            inspection: Some(plurx_core::hevc_configuration::RangeInspection {
                packets: 2 * rows.len() as u64,
                slices: 3 * rows.len() as u64,
                configuration_sha256: hex64('d'),
            }),
            row_inspections: inspections(&rows),
            promotion: PromotionInputs::default(),
            rows,
        },
    }))
}

/// The custody of a pass this node ran over the fixture's file and object.
fn pass_custody() -> IndexPassCustody {
    IndexPassCustody::for_test(NODE, 100, "dev:1:ino:2")
}

fn pass_observation(init: &Arc<Init>, rows: Vec<IndexRow>) -> CopyAnalysisMessage {
    pass_rows(NODE, pass_custody(), init, rows, true)
}

/// One committed range of a local viewer's own playback over the fixture's
/// source, holding `rows` from the title's first tick.
fn viewer_range(init: &Arc<Init>, rows: &[IndexRow], eof: bool) -> CopyAnalysisMessage {
    let timescale = init.video().expect("video track").timescale;
    let file = media_file();
    let recipe = CopyRecipe::playback_default(
        &file,
        plurx_core::transcode::CopyVideoOptions::new(false, false),
    );
    CopyAnalysisMessage::Range(Box::new(CompletedCopyRange {
        file: Arc::new(file),
        catalog_namespace: NAMESPACE.into(),
        source: source(),
        authority: CopyObservationAuthority::Local(
            LocalCopyAuthority::of(&local_viewer(7)).expect("a local user's own playback"),
        ),
        range_id: uuid::Uuid::new_v4(),
        range_epoch: 1,
        init: Arc::clone(init),
        checkpoint: Arc::new(CopyRangeCheckpoint {
            manifest_path: PathBuf::from("/ranges/manifest.json"),
            manifest_sha256: hex64('a'),
            timescale,
            canonical_title_start: 0,
            source_origin: SourceTimestamp {
                dts: 0,
                time_base_num: 1,
                time_base_den: timescale,
            },
            rows: rows.to_vec().into(),
            boundaries: RangeBoundaries {
                begins_title: true,
                eof,
            },
            first_frame_rendered: true,
            inspection: eof.then(|| plurx_core::hevc_configuration::RangeInspection {
                packets: 2 * rows.len() as u64,
                slices: 3 * rows.len() as u64,
                configuration_sha256: hex64('d'),
            }),
            row_inspections: inspections(rows).into(),
            promotion: PromotionInputs::default(),
        }),
        recipe,
    }))
}

fn ready(pending: &mut PendingSources) -> Vec<(String, super::work::PendingSource)> {
    pending.start_ready(std::time::Instant::now())
}

/// The content-analysis pass is a third observation source. Its whole-title
/// rows establish a complete set under the pass's own custody and turn a
/// row-less demand into rows. They are dropped, leaving what is held, when
/// the custody is another node's, another file's or another object's, when
/// the read was not the whole title, or when the rows disagree with rows
/// already held. Pure: no store, no source read.
#[test]
fn a_pass_observation_folds_as_a_third_source_under_the_pass_custody() {
    let init = muxer_init();
    let all = rows(4, 4_500);

    let mut pending = PendingSources::default();
    pending.fold(pass_observation(&init, all.clone()), NO_WAIT);
    let (key, held) = ready(&mut pending).pop().expect("the unit");
    assert_eq!(
        key,
        copy_index_v3_source_key(NAMESPACE, &source()).expect("source key")
    );
    assert_eq!(held.held_rows(), (all.clone(), true), "the whole title");

    // A read that is not this source's: another node's custody, a claim
    // over another file or another object, and one that stopped early.
    for refused in [
        pass_rows(
            NODE,
            IndexPassCustody::for_test("node-two", 100, "dev:1:ino:2"),
            &init,
            all.clone(),
            true,
        ),
        pass_rows(
            NODE,
            IndexPassCustody::for_test(NODE, 101, "dev:1:ino:2"),
            &init,
            all.clone(),
            true,
        ),
        pass_rows(
            NODE,
            IndexPassCustody::for_test(NODE, 100, "dev:1:ino:3"),
            &init,
            all.clone(),
            true,
        ),
        pass_rows(NODE, pass_custody(), &init, all.clone(), false),
    ] {
        let mut pending = PendingSources::default();
        pending.fold(refused, NO_WAIT);
        assert!(ready(&mut pending).is_empty());
    }

    // A demand waiting out its first interval has its rows at once.
    let mut asked = PendingSources::default();
    asked.fold(local_demand(NODE), std::time::Duration::from_secs(60));
    assert!(ready(&mut asked).is_empty());
    asked.fold(pass_observation(&init, all.clone()), NO_WAIT);
    let (_, held) = ready(&mut asked)
        .pop()
        .expect("whole now: nothing left to wait for");
    assert_eq!(held.held_rows(), (all.clone(), true));

    // Rows that disagree with what is held are dropped; the set stands.
    let mut other = all.clone();
    other[1].bytes += 1;
    let mut first = PendingSources::default();
    first.fold(pass_observation(&init, all.clone()), NO_WAIT);
    first.fold(pass_observation(&init, other), NO_WAIT);
    let (_, held) = ready(&mut first).pop().expect("the unit");
    assert_eq!(held.held_rows(), (all, true));
}

/// `ridden` means a pass's rows and nothing else. A viewer's range folding
/// in clears it; a pass folding into rows a viewer left never sets it; and a
/// viewer's range that disagrees with a pass's rows replaces them, so it is
/// the viewer's rows that are judged against the head. Pure.
#[test]
fn a_viewers_range_makes_a_set_its_own_whatever_a_pass_rode_in_with() {
    let init = muxer_init();
    let all = rows(4, 4_500);
    let mut other = all.clone();
    other[1].bytes += 1;

    // A pass, then the viewer's agreeing range.
    let mut pending = PendingSources::default();
    pending.fold(pass_observation(&init, all.clone()), NO_WAIT);
    pending.fold(viewer_range(&init, &all[..2], false), NO_WAIT);
    let (_, held) = ready(&mut pending).pop().expect("the unit");
    assert_eq!(held.held_rows(), (all.clone(), false));

    // The viewer's range, then a pass over it.
    let mut pending = PendingSources::default();
    pending.fold(viewer_range(&init, &all[..2], false), NO_WAIT);
    pending.fold(pass_observation(&init, all.clone()), NO_WAIT);
    let (_, held) = ready(&mut pending).pop().expect("the unit");
    assert_eq!(held.held_rows(), (all.clone(), false));

    // A pass, then a viewer's range that disagrees with it: the range wins.
    let mut pending = PendingSources::default();
    pending.fold(pass_observation(&init, all.clone()), NO_WAIT);
    pending.fold(viewer_range(&init, &other[..2], false), NO_WAIT);
    let (_, held) = ready(&mut pending).pop().expect("the unit");
    assert_eq!(held.held_rows(), (other[..2].to_vec(), false));

    // The viewer's range first: a disagreeing pass is dropped.
    let mut pending = PendingSources::default();
    pending.fold(viewer_range(&init, &other[..2], false), NO_WAIT);
    pending.fold(pass_observation(&init, all), NO_WAIT);
    let (_, held) = ready(&mut pending).pop().expect("the unit");
    assert_eq!(held.held_rows(), (other[..2].to_vec(), false));
}

/// A pass may make room only among other passes' sets. With every slot
/// held by a demand, its rows are dropped and no demand is forgotten; with
/// a ridden set among them, that one gives way. Pure.
#[test]
fn a_pass_evicts_only_another_ridden_set() {
    let init = muxer_init();
    let all = rows(4, 4_500);
    let other_file = |file_id: i64| {
        let mut message = pass_rows(
            NODE,
            IndexPassCustody::for_test(NODE, file_id, "dev:1:ino:2"),
            &init,
            all.clone(),
            true,
        );
        if let CopyAnalysisMessage::Pass(observation) = &mut message {
            let mut file = media_file();
            file.id = file_id;
            observation.file = Arc::new(file);
            observation.source.file_id = file_id;
        }
        message
    };

    // Sixty-four demands, one per file: the bound.
    let mut demands = PendingSources::default();
    for file_id in 1..=64 {
        demands.fold(
            CopyAnalysisMessage::Demand(Box::new(demand_for(NODE, file_id))),
            NO_WAIT,
        );
    }
    demands.fold(other_file(500), NO_WAIT);
    let held = ready(&mut demands);
    assert_eq!(held.len(), 64, "no demand gave way");
    assert!(held.iter().all(|(_, source)| !source.held_rows().1));

    // Sixty-three demands and one ridden set: the ridden set gives way.
    let mut mixed = PendingSources::default();
    for file_id in 1..=63 {
        mixed.fold(
            CopyAnalysisMessage::Demand(Box::new(demand_for(NODE, file_id))),
            NO_WAIT,
        );
    }
    mixed.fold(other_file(500), NO_WAIT);
    mixed.fold(other_file(501), NO_WAIT);
    let held = ready(&mut mixed);
    assert_eq!(held.len(), 64);
    assert_eq!(
        held.iter()
            .filter(|(_, source)| source.held_rows().1)
            .count(),
        1,
        "one ridden set, the later one"
    );
}

/// A pass's rows only ever add an index. Against a healthy head of this
/// same engine they are done when they agree, and when they disagree the
/// source is discarded with the head untouched: no request, no claim, no
/// quarantine. A viewer's own range with the same disagreement still
/// quarantines the head, whether or not a pass rode in first. A real store
/// and blob; no source is read.
#[tokio::test]
async fn ridden_rows_never_quarantine_a_healthy_head_and_a_viewers_range_still_does() {
    let fixture = store_fixture().await;
    let init = muxer_init();
    let all = rows(4, 4_500);
    let engine = crate::ffmpeg::fragment_index_engine_digest().await;
    let (artifact, blob) = variant(&sealed(&init, all.clone(), 12, true), &engine);
    fixture.publish(&artifact);
    fixture.install(&artifact, &blob).await;

    let mut agreeing = PendingSources::default();
    agreeing.fold(pass_observation(&init, all.clone()), NO_WAIT);
    let (key, mut held) = ready(&mut agreeing).pop().expect("the unit");
    assert!(matches!(
        publish_source(&fixture.context, &key, &mut held).await,
        Attempt::Done
    ));

    let mut other = all.clone();
    other[1].bytes += 1;
    let mut differing = PendingSources::default();
    differing.fold(pass_observation(&init, other.clone()), NO_WAIT);
    let (key, mut held) = ready(&mut differing).pop().expect("the unit");
    assert!(matches!(
        publish_source(&fixture.context, &key, &mut held).await,
        Attempt::Discard(_)
    ));
    assert_eq!(fixture.head().await.state, CopyIndexV3HeadState::Healthy);
    assert_eq!(fixture.quarantine_code(), None);
    assert_eq!(
        fixture
            .sql()
            .query_row("SELECT count(*) FROM analysis_requests", [], |row| row
                .get::<_, i64>(0))
            .expect("requests"),
        0,
        "a ride that disagrees costs a read and writes nothing"
    );

    // The pass's agreeing rows, then the viewer's own contradicting range:
    // the set is the viewer's, and the head is quarantined under a claim.
    // What follows the quarantine (the seal's attestation of a file this
    // fixture does not have) is not this test's.
    let mut viewer = PendingSources::default();
    viewer.fold(pass_observation(&init, all), NO_WAIT);
    viewer.fold(viewer_range(&init, &other, true), NO_WAIT);
    let (key, mut held) = ready(&mut viewer).pop().expect("the unit");
    assert_eq!(held.held_rows(), (other, false));
    let _ = publish_source(&fixture.context, &key, &mut held).await;
    assert_eq!(
        fixture.quarantine_code().as_deref(),
        Some(plurx_core::store::COPY_INDEX_V3_CONTRADICTION_CODE),
        "a viewer's own range still quarantines"
    );
}
