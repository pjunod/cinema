//! Guarded v3 publication against the real SQLite schema and public Store
//! APIs: coordination claim, head, publisher job, publish, replay, refusal.

use super::analysis_coordination::{AnalysisCoordinationClaim, AnalysisCoordinationSource};
use super::background_jobs::*;
use super::copy_index_v3::{CopyIndexV3Artifact, CopyIndexV3HeadState, PublishCopyIndexV3};
use super::sqlite::SqliteStore;
use super::{ClusterFragmentIndexStore, FragmentIndexOutputIdentity, NewAnalysisRequest};

const NODE: &str = "origin-node";
const NAMESPACE: &str = "cluster-catalogue";
const NOW: i64 = 1_000_000;

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    store: SqliteStore,
    source: AnalysisCoordinationSource,
    claim: AnalysisCoordinationClaim,
    head_epoch: i64,
}

fn source() -> AnalysisCoordinationSource {
    AnalysisCoordinationSource {
        file_id: 100,
        source_size: 4096,
        source_mtime: 77,
        origin_node_id: NODE.into(),
        object_version: "dev:1:ino:2".into(),
        video_identity: "ab".repeat(16),
        contract_version: super::FRAGMENT_INDEX_CONTRACT,
        requested_generation: "c".repeat(64),
    }
}

fn artifact(output: char, blob: char) -> CopyIndexV3Artifact {
    let output_contract_sha256 = output.to_string().repeat(64);
    let source_sha256 = "5".repeat(64);
    let contract_key = super::cluster_fragment_index_v3_key(
        NAMESPACE,
        100,
        &source_sha256,
        &FragmentIndexOutputIdentity {
            contract_version: super::FRAGMENT_INDEX_CONTRACT,
            keyframe_table_sha256: output_contract_sha256.clone(),
        },
    )
    .expect("contract key");
    CopyIndexV3Artifact {
        blob_sha256: blob.to_string().repeat(64),
        contract_key,
        catalog_namespace: NAMESPACE.into(),
        file_id: 100,
        source_sha256,
        contract_version: super::FRAGMENT_INDEX_CONTRACT,
        output_contract_sha256,
        init_contract_sha256: "7".repeat(64),
        bytes: 4096,
        built_by_node_id: NODE.into(),
        built_at_ms: NOW,
        producer_engine_sha256: "e".repeat(64),
    }
}

fn sql(fixture: &Fixture) -> rusqlite::Connection {
    rusqlite::Connection::open(&fixture.path).expect("fixture connection")
}

/// Seed one file and a full-v1 source receipt, then take the real metadata
/// claim and seed the pending head through the coordinator APIs.
async fn fixture(full_receipt: bool) -> Fixture {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("plurx.db");
    let store = SqliteStore::open(&path).expect("store");
    let source = source();
    {
        let db = rusqlite::Connection::open(&path).expect("seed");
        db.execute_batch(
            "INSERT INTO libraries (id, name, kind, paths, anime) VALUES (1, 'Films', 'movies', '[\"/films\"]', 0);
             INSERT INTO items (id, library_id, kind, title, sort_title) VALUES (10, 1, 'movie', 'Film', 'film');
             INSERT INTO files (id, item_id, path, size, mtime) VALUES (100, 10, '/films/film.mkv', 4096, 77);",
        )
        .expect("seed catalogue");
        let object = if full_receipt {
            format!("hevc-full-v1:{}", source.object_version)
        } else {
            source.object_version.clone()
        };
        db.execute(
            "INSERT INTO cluster_fragment_index_sources(node_id,file_id,object_version,source_size,source_mtime,source_sha256,observed_at_ms) VALUES (?1,100,?2,4096,77,?3,?4)",
            rusqlite::params![NODE, object, "5".repeat(64), NOW],
        )
        .expect("source receipt");
    }
    let claim = coordinate(&store, &source, NOW).await;
    let head = store
        .ensure_copy_index_v3_head(NAMESPACE, source.clone(), claim.clone(), NOW)
        .await
        .expect("head")
        .expect("seeded head");
    assert_eq!(head.state, CopyIndexV3HeadState::Pending);
    Fixture {
        _dir: dir,
        path,
        store,
        source,
        claim,
        head_epoch: head.head_epoch,
    }
}

/// A fresh queued coordination request for the source, established and
/// claimed through the real coordinator APIs.
async fn coordinate(
    store: &SqliteStore,
    source: &AnalysisCoordinationSource,
    at: i64,
) -> AnalysisCoordinationClaim {
    let request = store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            file_id: 100,
            source_size: 4096,
            source_mtime: 77,
            component: "fragment_index".into(),
            pipeline_version: source.pipeline_marker(),
            video_identity: source.video_marker(),
            requested_generation: source.requested_generation.clone(),
            priority: "normal".into(),
            trigger: "background".into(),
            force_rebuild: false,
            target_node_id: NODE.into(),
            not_before_ms: at - 10,
            created_at_ms: at - 10,
        })
        .await
        .expect("coordination request");
    let role = store
        .establish_analysis_coordination(&request.request_id, source.clone(), at - 5)
        .await
        .expect("establish")
        .expect("role");
    assert_eq!(
        role.request_id, request.request_id,
        "a fresh request owns the role"
    );
    store
        .claim_analysis_coordination(
            &request.request_id,
            source.clone(),
            NODE,
            request.fence,
            at - 1,
            at + 600_000,
        )
        .await
        .expect("claim")
        .expect("metadata claim")
}

async fn publisher(fixture: &Fixture, artifact: &CopyIndexV3Artifact) -> JobToken {
    let payload = super::copy_index_v3_publish_payload(
        &fixture.claim,
        &fixture.source,
        artifact,
        fixture.head_epoch,
    )
    .expect("payload");
    let id = uuid::Uuid::new_v4().to_string();
    let dedupe = format!("{:064x}", rand_dedupe(&id));
    let outcome = fixture
        .store
        .enqueue_job(EnqueueJob {
            id: id.clone(),
            payload,
            dedupe_key: dedupe.clone(),
            priority: 1,
            not_before_ms: NOW,
            now_ms: NOW,
            request: JobRequest {
                scope: "copy-index-v3".into(),
                request_id: dedupe.clone(),
                request_digest: dedupe,
                consumer_kind: "copy_index_v3".into(),
                consumer_ref: fixture.claim.request_id.clone(),
                target_node_id: None,
                deadline_ms: Some(NOW + 600_000),
                retain_identity: false,
            },
        })
        .await
        .expect("enqueue publisher");
    assert!(
        matches!(outcome, EnqueueOutcome::Accepted { .. }),
        "{outcome:?}"
    );
    let job = fixture
        .store
        .background_job(&id)
        .await
        .expect("job")
        .expect("publisher row");
    match fixture
        .store
        .claim_job(ClaimJob {
            job_id: id,
            expected_revision: job.revision,
            node_id: NODE.into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::FragmentIndexPublish,
            payload_version: 1,
            now_ms: NOW,
            dispatched_at_ms: NOW,
        })
        .await
        .expect("claim publisher")
    {
        ClaimOutcome::Claimed { job } => job.token.expect("token"),
        other => panic!("publisher not claimed: {other:?}"),
    }
}

fn rand_dedupe(seed: &str) -> u128 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(seed.as_bytes());
    u128::from_be_bytes(digest[..16].try_into().expect("sixteen bytes"))
}

fn publish(
    fixture: &Fixture,
    token: &JobToken,
    artifact: &CopyIndexV3Artifact,
    now_ms: i64,
) -> PublishCopyIndexV3 {
    PublishCopyIndexV3 {
        token: token.clone(),
        claim: fixture.claim.clone(),
        source: fixture.source.clone(),
        artifact: artifact.clone(),
        expected_head_epoch: fixture.head_epoch,
        now_ms,
    }
}

#[tokio::test]
async fn publication_adopts_the_candidate_and_settles_analysis_job_and_location() {
    let fixture = fixture(true).await;
    let artifact = artifact('a', '1');
    let token = publisher(&fixture, &artifact).await;
    let outcome = fixture
        .store
        .publish_copy_index_v3(publish(&fixture, &token, &artifact, NOW + 1))
        .await
        .expect("publish");
    let JobPublishOutcome::Published { result_ref, .. } = outcome else {
        panic!("expected publication, got {outcome:?}");
    };
    let result: serde_json::Value = serde_json::from_str(&result_ref).expect("result_ref");
    assert_eq!(result["candidate"]["blob_sha256"], artifact.blob_sha256);
    assert_eq!(result["head"]["head_epoch"], fixture.head_epoch + 1);
    let key = super::copy_index_v3_source_key(NAMESPACE, &fixture.source).expect("key");
    let head = fixture
        .store
        .copy_index_v3_head(&key)
        .await
        .expect("head")
        .expect("published head");
    assert_eq!(head.state, CopyIndexV3HeadState::Healthy);
    assert_eq!(
        head.blob_sha256.as_deref(),
        Some(artifact.blob_sha256.as_str())
    );
    assert_eq!(
        head.contract_key.as_deref(),
        Some(artifact.contract_key.as_str())
    );
    assert_eq!(head.head_epoch, fixture.head_epoch + 1);
    assert_eq!(
        head.published_generation.as_deref(),
        Some(fixture.source.requested_generation.as_str())
    );
    assert_eq!(
        head.publisher_job_id.as_deref(),
        Some(token.job_id.as_str())
    );
    let db = sql(&fixture);
    let (state, owner, cache): (String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT state,owner_node_id,result_cache_key FROM analysis_requests WHERE request_id=?1",
            [&fixture.claim.request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("analysis");
    assert_eq!(
        (state.as_str(), owner, cache.as_deref()),
        ("ready", None, Some(artifact.contract_key.as_str()))
    );
    let job_state: String = db
        .query_row(
            "SELECT state FROM background_jobs WHERE id=?1",
            [&token.job_id],
            |row| row.get(0),
        )
        .expect("job");
    assert_eq!(job_state, "succeeded");
    let locations: i64 = db
        .query_row(
            "SELECT count(*) FROM copy_index_v3_locations WHERE contract_key=?1 AND blob_sha256=?2 AND node_id=?3",
            rusqlite::params![artifact.contract_key, artifact.blob_sha256, NODE],
            |row| row.get(0),
        )
        .expect("location");
    assert_eq!(locations, 1);
    let commands: i64 = db
        .query_row("SELECT count(*) FROM background_job_commands", [], |row| {
            row.get(0)
        })
        .expect("commands");
    assert_eq!(
        commands, 0,
        "the command envelope is consumed by its trigger"
    );

    // Replay after the Analysis became ready returns the stored receipt; a
    // later timestamp is not a new publication and moves nothing.
    let replay = fixture
        .store
        .publish_copy_index_v3(publish(&fixture, &token, &artifact, NOW + 50))
        .await
        .expect("replay");
    let JobPublishOutcome::AlreadyPublished {
        result_ref: replayed,
        ..
    } = replay
    else {
        panic!("expected replay, got {replay:?}");
    };
    assert_eq!(replayed, result_ref);
    let after = fixture
        .store
        .copy_index_v3_head(&key)
        .await
        .expect("head")
        .expect("head");
    assert_eq!(after, head);
}

#[tokio::test]
async fn publication_refuses_a_sampled_receipt_and_writes_nothing() {
    let fixture = fixture(false).await;
    let artifact = artifact('a', '1');
    let token = publisher(&fixture, &artifact).await;
    let outcome = fixture
        .store
        .publish_copy_index_v3(publish(&fixture, &token, &artifact, NOW + 1))
        .await
        .expect("publish");
    assert!(
        matches!(outcome, JobPublishOutcome::ArtifactConflict),
        "{outcome:?}"
    );
    let db = sql(&fixture);
    let (heads, artifacts, locations): (String, i64, i64) = db
        .query_row(
            "SELECT (SELECT state FROM copy_index_v3_heads),(SELECT count(*) FROM copy_index_v3_artifacts),(SELECT count(*) FROM copy_index_v3_locations)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("state");
    assert_eq!((heads.as_str(), artifacts, locations), ("pending", 0, 0));
    let analysis: String = db
        .query_row(
            "SELECT state FROM analysis_requests WHERE request_id=?1",
            [&fixture.claim.request_id],
            |row| row.get(0),
        )
        .expect("analysis");
    assert_eq!(analysis, "running");
}

#[tokio::test]
async fn publication_refuses_a_stale_worker_token() {
    let fixture = fixture(true).await;
    let artifact = artifact('a', '1');
    let token = publisher(&fixture, &artifact).await;
    for stale in 0..3 {
        let mut wrong = token.clone();
        match stale {
            0 => wrong.revision += 1,
            1 => wrong.fence += 1,
            _ => wrong.boot_id = uuid::Uuid::new_v4().to_string(),
        }
        let outcome = fixture
            .store
            .publish_copy_index_v3(publish(&fixture, &wrong, &artifact, NOW + 1))
            .await
            .expect("publish");
        assert!(
            matches!(outcome, JobPublishOutcome::LostOwnership),
            "case {stale}: {outcome:?}"
        );
    }
    let state: String = sql(&fixture)
        .query_row("SELECT state FROM copy_index_v3_heads", [], |row| {
            row.get(0)
        })
        .expect("head");
    assert_eq!(state, "pending");
}

#[tokio::test]
async fn publication_refuses_a_payload_that_differs_from_the_running_job() {
    let fixture = fixture(true).await;
    let enqueued = artifact('a', '1');
    let token = publisher(&fixture, &enqueued).await;
    let other_blob = artifact('a', '2');
    let outcome = fixture
        .store
        .publish_copy_index_v3(publish(&fixture, &token, &other_blob, NOW + 1))
        .await
        .expect("publish");
    assert!(
        matches!(outcome, JobPublishOutcome::ArtifactConflict),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn publication_refuses_a_changed_source_file() {
    let fixture = fixture(true).await;
    let artifact = artifact('a', '1');
    let token = publisher(&fixture, &artifact).await;
    // Raw edit bypasses the file triggers on purpose: the publication's own
    // current-file check must refuse even if nothing cancelled the attempt.
    let db = sql(&fixture);
    db.execute_batch("DROP TRIGGER IF EXISTS analysis_requests_supersede_source;")
        .ok();
    db.execute("UPDATE files SET mtime=78 WHERE id=100", [])
        .expect("moved source");
    let outcome = fixture
        .store
        .publish_copy_index_v3(publish(&fixture, &token, &artifact, NOW + 1))
        .await
        .expect("publish");
    assert!(
        matches!(outcome, JobPublishOutcome::SourceChanged),
        "{outcome:?}"
    );
    let state: String = db
        .query_row("SELECT state FROM copy_index_v3_heads", [], |row| {
            row.get(0)
        })
        .expect("head");
    assert_eq!(state, "pending");
}

#[tokio::test]
async fn a_quarantined_head_adopts_a_new_variant_but_never_the_quarantined_blob() {
    let mut fixture = fixture(true).await;
    let bad = artifact('a', '1');
    let token = publisher(&fixture, &bad).await;
    assert!(matches!(
        fixture
            .store
            .publish_copy_index_v3(publish(&fixture, &token, &bad, NOW + 1))
            .await
            .expect("publication"),
        JobPublishOutcome::Published { .. }
    ));
    let key = super::copy_index_v3_source_key(NAMESPACE, &fixture.source).expect("key");
    let epoch = fixture.head_epoch + 1;
    let successor =
        super::copy_index_v3_rebuild_generation(&key, &bad.blob_sha256, epoch).expect("successor");
    fixture.source.requested_generation = successor;
    fixture.claim = coordinate(&fixture.store, &fixture.source, NOW + 10).await;
    assert!(fixture
        .store
        .quarantine_copy_index_v3_head(super::background_jobs::QuarantineCopyIndexV3Head {
            catalog_namespace: NAMESPACE.into(),
            source: fixture.source.clone(),
            claim: fixture.claim.clone(),
            expected_epoch: epoch,
            bad_blob_sha256: bad.blob_sha256.clone(),
            code: "row_mismatch".into(),
            now_ms: NOW + 11,
        })
        .await
        .expect("quarantine"));
    fixture.head_epoch = epoch;
    let token = publisher(&fixture, &bad).await;
    let outcome = fixture
        .store
        .publish_copy_index_v3(publish(&fixture, &token, &bad, NOW + 12))
        .await
        .expect("re-publication");
    assert!(
        matches!(outcome, JobPublishOutcome::ArtifactConflict),
        "{outcome:?}"
    );
    let repaired = artifact('b', '3');
    let token = publisher(&fixture, &repaired).await;
    let outcome = fixture
        .store
        .publish_copy_index_v3(publish(&fixture, &token, &repaired, NOW + 13))
        .await
        .expect("repair publication");
    assert!(
        matches!(outcome, JobPublishOutcome::Published { .. }),
        "{outcome:?}"
    );
    let head = fixture
        .store
        .copy_index_v3_head(&key)
        .await
        .expect("head")
        .expect("head");
    assert_eq!(head.state, CopyIndexV3HeadState::Healthy);
    assert_eq!(
        head.blob_sha256.as_deref(),
        Some(repaired.blob_sha256.as_str())
    );
    assert_eq!(head.head_epoch, epoch + 1);
}

const HYDRATOR: &str = "hydrating-node";

async fn published(fixture: &Fixture, artifact: &CopyIndexV3Artifact) {
    let token = publisher(fixture, artifact).await;
    let outcome = fixture
        .store
        .publish_copy_index_v3(publish(fixture, &token, artifact, NOW + 1))
        .await
        .expect("publish");
    assert!(
        matches!(outcome, JobPublishOutcome::Published { .. }),
        "{outcome:?}"
    );
}

fn location(
    artifact: &CopyIndexV3Artifact,
    node_id: &str,
    at: i64,
) -> super::copy_index_v3::CopyIndexV3Location {
    super::copy_index_v3::CopyIndexV3Location {
        contract_key: artifact.contract_key.clone(),
        blob_sha256: artifact.blob_sha256.clone(),
        node_id: node_id.into(),
        bytes: artifact.bytes,
        verified_at_ms: at,
        last_seen_at_ms: at,
    }
}

#[tokio::test]
async fn a_consumer_reads_a_head_its_variant_and_its_own_holding_by_key() {
    let fixture = fixture(true).await;
    let artifact = artifact('a', '1');
    let key = super::copy_index_v3_source_key(NAMESPACE, &fixture.source).expect("key");
    let pending = fixture
        .store
        .copy_index_v3_variant(&key, NODE)
        .await
        .expect("pending variant")
        .expect("the seeded head");
    assert_eq!(pending.head.state, CopyIndexV3HeadState::Pending);
    assert_eq!(pending.artifact, None);
    assert!(!pending.held);
    assert!(
        pending.readable().is_none(),
        "a pending head is never readable"
    );
    assert!(
        fixture
            .store
            .copy_index_v3_healthy_heads(NAMESPACE, &fixture.source)
            .await
            .expect("pending heads")
            .is_empty(),
        "a pending head is never offered to a consumer"
    );
    published(&fixture, &artifact).await;
    let head = fixture
        .store
        .copy_index_v3_head(&key)
        .await
        .expect("head")
        .expect("published head");
    let variant = fixture
        .store
        .copy_index_v3_variant(&key, NODE)
        .await
        .expect("variant")
        .expect("published variant");
    assert_eq!(variant.head, head);
    assert_eq!(variant.readable(), Some(&artifact));
    assert!(variant.held, "the publisher holds what it published");
    assert!(
        !fixture
            .store
            .copy_index_v3_variant(&key, HYDRATOR)
            .await
            .expect("variant")
            .expect("published variant")
            .held,
        "another node holds nothing until it hydrates"
    );
    assert_eq!(
        fixture
            .store
            .copy_index_v3_variant(&"9".repeat(64), NODE)
            .await
            .expect("unknown key"),
        None
    );
    assert!(fixture
        .store
        .copy_index_v3_variant("not-a-digest", NODE)
        .await
        .is_err());

    // The per-file listing applies every identity filter in the statement.
    assert_eq!(
        fixture
            .store
            .copy_index_v3_healthy_heads(NAMESPACE, &fixture.source)
            .await
            .expect("healthy heads"),
        vec![head]
    );
    let mut other_origin = fixture.source.clone();
    other_origin.origin_node_id = HYDRATOR.into();
    other_origin.object_version = "dev:9:ino:9".into();
    assert_eq!(
        fixture
            .store
            .copy_index_v3_healthy_heads(NAMESPACE, &other_origin)
            .await
            .expect("another origin asks")
            .len(),
        1,
        "origin and object version are not part of the listing"
    );
    for field in 0..5 {
        let mut other = fixture.source.clone();
        let mut namespace = NAMESPACE;
        match field {
            0 => other.file_id += 1,
            1 => other.source_size += 1,
            2 => other.source_mtime += 1,
            3 => other.video_identity = "cd".repeat(16),
            _ => namespace = "another-catalogue",
        }
        assert!(
            fixture
                .store
                .copy_index_v3_healthy_heads(namespace, &other)
                .await
                .expect("unrelated heads")
                .is_empty(),
            "field {field}"
        );
    }
    let locations = fixture
        .store
        .copy_index_v3_locations(&artifact.contract_key, &artifact.blob_sha256)
        .await
        .expect("locations");
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].node_id, NODE);
    assert_eq!(locations[0].bytes, artifact.bytes);
}

#[tokio::test]
async fn a_hydrating_node_records_and_forgets_only_its_own_verified_location() {
    let fixture = fixture(true).await;
    let artifact = artifact('a', '1');
    let key = super::copy_index_v3_source_key(NAMESPACE, &fixture.source).expect("key");
    let put = |source_key: String, location: super::copy_index_v3::CopyIndexV3Location| {
        let store = &fixture.store;
        async move {
            store
                .put_copy_index_v3_location(&source_key, location)
                .await
                .expect("put location")
        }
    };
    assert!(
        !put(key.clone(), location(&artifact, HYDRATOR, NOW)).await,
        "an unpublished variant has no holders"
    );
    published(&fixture, &artifact).await;
    let mut short = location(&artifact, HYDRATOR, NOW + 2);
    short.bytes -= 1;
    assert!(
        !put(key.clone(), short).await,
        "the holder must have the variant's exact size"
    );
    let mut unknown = location(&artifact, HYDRATOR, NOW + 2);
    unknown.blob_sha256 = "9".repeat(64);
    assert!(
        !put(key.clone(), unknown).await,
        "the head does not point at this blob"
    );
    assert!(
        !put("9".repeat(64), location(&artifact, HYDRATOR, NOW + 2)).await,
        "no head has this source key"
    );
    assert!(put(key.clone(), location(&artifact, HYDRATOR, NOW + 2)).await);
    assert!(
        put(key.clone(), location(&artifact, HYDRATOR, NOW + 5)).await,
        "a later verification refreshes the same row"
    );
    let locations = fixture
        .store
        .copy_index_v3_locations(&artifact.contract_key, &artifact.blob_sha256)
        .await
        .expect("locations");
    assert_eq!(
        locations
            .iter()
            .map(|location| (location.node_id.as_str(), location.verified_at_ms))
            .collect::<Vec<_>>(),
        vec![(HYDRATOR, NOW + 5), (NODE, NOW + 1)],
        "most recently seen first"
    );
    assert!(fixture
        .store
        .put_copy_index_v3_location(&key, location(&artifact, "bad node", NOW))
        .await
        .is_err());
    assert!(fixture
        .store
        .forget_copy_index_v3_location(&artifact.contract_key, &artifact.blob_sha256, HYDRATOR)
        .await
        .expect("forget"));
    assert!(
        !fixture
            .store
            .forget_copy_index_v3_location(&artifact.contract_key, &artifact.blob_sha256, HYDRATOR)
            .await
            .expect("forget again"),
        "forgetting is idempotent"
    );
    let locations = fixture
        .store
        .copy_index_v3_locations(&artifact.contract_key, &artifact.blob_sha256)
        .await
        .expect("locations");
    assert_eq!(locations.len(), 1, "the publisher's row is untouched");
    assert_eq!(locations[0].node_id, NODE);
}

#[tokio::test]
async fn a_quarantined_variant_is_unreadable_and_gains_no_new_holder() {
    let mut fixture = fixture(true).await;
    let bad = artifact('a', '1');
    published(&fixture, &bad).await;
    let key = super::copy_index_v3_source_key(NAMESPACE, &fixture.source).expect("key");
    let epoch = fixture.head_epoch + 1;
    let published_source = fixture.source.clone();
    fixture.source.requested_generation =
        super::copy_index_v3_rebuild_generation(&key, &bad.blob_sha256, epoch).expect("successor");
    fixture.claim = coordinate(&fixture.store, &fixture.source, NOW + 10).await;
    assert!(fixture
        .store
        .quarantine_copy_index_v3_head(super::background_jobs::QuarantineCopyIndexV3Head {
            catalog_namespace: NAMESPACE.into(),
            source: fixture.source.clone(),
            claim: fixture.claim.clone(),
            expected_epoch: epoch,
            bad_blob_sha256: bad.blob_sha256.clone(),
            code: "row_mismatch".into(),
            now_ms: NOW + 11,
        })
        .await
        .expect("quarantine"));
    assert!(
        fixture
            .store
            .copy_index_v3_healthy_heads(NAMESPACE, &published_source)
            .await
            .expect("heads")
            .is_empty(),
        "a quarantined head is not offered to a consumer"
    );
    let variant = fixture
        .store
        .copy_index_v3_variant(&key, NODE)
        .await
        .expect("variant")
        .expect("the quarantined head");
    assert_eq!(variant.head.state, CopyIndexV3HeadState::Quarantined);
    assert_eq!(
        variant.head.blob_sha256.as_deref(),
        Some(bad.blob_sha256.as_str()),
        "the bad pointer stays visible so nothing else adopts it"
    );
    assert!(variant.readable().is_none());
    assert!(
        !fixture
            .store
            .put_copy_index_v3_location(&key, location(&bad, HYDRATOR, NOW + 12))
            .await
            .expect("put"),
        "an unreadable head's variant gains no holder"
    );
}

/// A queued coordination request for another generation of the fixture's
/// source, established through the real coordinator and never claimed.
async fn queued(store: &SqliteStore, generation: char, target: &str, created: i64) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    queued_as(store, &id, generation, target, created).await
}

/// [`queued`] under a chosen request id.
async fn queued_as(
    store: &SqliteStore,
    id: &str,
    generation: char,
    target: &str,
    created: i64,
) -> String {
    let mut source = source();
    source.requested_generation = generation.to_string().repeat(64);
    let request = store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: id.to_owned(),
            file_id: 100,
            source_size: 4096,
            source_mtime: 77,
            component: "fragment_index".into(),
            pipeline_version: source.pipeline_marker(),
            video_identity: source.video_marker(),
            requested_generation: source.requested_generation.clone(),
            priority: "normal".into(),
            trigger: "background".into(),
            force_rebuild: false,
            target_node_id: target.into(),
            not_before_ms: created,
            created_at_ms: created,
        })
        .await
        .expect("coordination request");
    store
        .establish_analysis_coordination(&request.request_id, source, created + 1)
        .await
        .expect("establish")
        .expect("role");
    request.request_id
}

/// Covers the SQLite backend of the stranded-coordinator retirement: only
/// this node's queued coordination requests created before the boot mark are
/// cancelled, with their own code, and the retired request's source and
/// generation can be coordinated again (the enqueue exemption and the role
/// release) by a request that inherits the retired one's attempts. A queued request whose role still names a physical custody is
/// left alone by the statement; that case is not constructed here.
#[tokio::test]
async fn a_previous_processs_queued_coordinators_are_retired_and_stay_coordinable() {
    let fixture = fixture(true).await;
    let boot = NOW + 200;
    let stranded = queued(&fixture.store, 'd', NODE, NOW + 100).await;
    let this_process = queued(&fixture.store, 'e', NODE, NOW + 300).await;
    let elsewhere = queued(&fixture.store, 'f', "other-node", NOW + 100).await;
    sql(&fixture)
        .execute(
            "UPDATE analysis_requests SET attempts=2 WHERE request_id=?1",
            [&stranded],
        )
        .expect("two charged attempts before the restart");
    let retire = || async {
        fixture
            .store
            .retire_stranded_analysis_coordination(NODE, boot, NOW + 400, &[])
            .await
            .expect("retire")
    };
    assert_eq!(retire().await, 1);
    assert_eq!(retire().await, 0, "retiring is idempotent");
    let db = sql(&fixture);
    let row = |id: &str| {
        db.query_row(
            "SELECT state,COALESCE(last_error_code,''),cancel_requested FROM analysis_requests WHERE request_id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("request")
    };
    assert_eq!(
        row(&stranded),
        (
            "cancelled".into(),
            super::analysis_coordination::STRANDED_CODE.into(),
            1
        )
    );
    assert_eq!(
        row(&this_process).0,
        "queued",
        "created after the boot mark"
    );
    assert_eq!(row(&elsewhere).0, "queued", "another node's work");
    assert_eq!(
        row(&fixture.claim.request_id).0,
        "running",
        "a claimed coordinator is never retired"
    );
    assert!(fixture
        .store
        .retire_stranded_analysis_coordination("bad node", boot, NOW, &[])
        .await
        .is_err());

    // The same source and generation is coordinated afresh: a new request
    // owns the role and is claimed.
    let mut again = source();
    again.requested_generation = "d".repeat(64);
    let claim = coordinate(&fixture.store, &again, NOW + 500).await;
    assert_ne!(claim.request_id, stranded);
    let attempts: i64 = db
        .query_row(
            "SELECT attempts FROM analysis_requests WHERE request_id=?1",
            [&claim.request_id],
            |row| row.get(0),
        )
        .expect("replacement");
    assert_eq!(
        attempts, 3,
        "the retired request's two attempts and this claim: the limit is not reset"
    );

    // A claim borrowed for one metadata write is ended the same way, with
    // its own attempt taken back, and leaves its generation free again.
    assert!(fixture
        .store
        .retire_analysis_coordination_claim(&claim, NOW + 600)
        .await
        .expect("end the claim"));
    assert_eq!(
        row(&claim.request_id),
        (
            "cancelled".into(),
            super::analysis_coordination::STRANDED_CODE.into(),
            1
        )
    );
    let mut stale = claim.clone();
    stale.fence += 5;
    assert!(
        !fixture
            .store
            .retire_analysis_coordination_claim(&stale, NOW + 601)
            .await
            .expect("another fence"),
        "only the exact claim ends its request"
    );
    let next = coordinate(&fixture.store, &again, NOW + 700).await;
    let attempts: i64 = db
        .query_row(
            "SELECT attempts FROM analysis_requests WHERE request_id=?1",
            [&next.request_id],
            |row| row.get(0),
        )
        .expect("next replacement");
    assert_eq!(attempts, 3, "the borrowed claim charged nothing");
}

/// A head quarantined on a doubt (no holder, a contradicting observation)
/// may publish the same bytes again: an identical reseal means the bytes
/// were right. A sealed-init mismatch bans its blob, because a reseal
/// repeats the seal's own promotion and disproves nothing about a served
/// init; so does every other code. The recorded code is readable.
#[tokio::test]
async fn a_quarantine_on_a_doubt_lets_the_same_bytes_publish_again() {
    for code in [
        super::COPY_INDEX_V3_NO_HOLDER_CODE,
        super::COPY_INDEX_V3_CONTRADICTION_CODE,
    ] {
        assert!(
            matches!(
                republish_after(code).await,
                JobPublishOutcome::Published { .. }
            ),
            "{code} is a doubt"
        );
    }
    assert!(
        matches!(
            republish_after(super::COPY_INDEX_V3_SEALED_INIT_CODE).await,
            JobPublishOutcome::ArtifactConflict
        ),
        "a sealed-init mismatch bans the bytes it quarantined"
    );
}

async fn republish_after(code: &str) -> JobPublishOutcome {
    let mut fixture = fixture(true).await;
    let lost = artifact('a', '1');
    published(&fixture, &lost).await;
    let key = super::copy_index_v3_source_key(NAMESPACE, &fixture.source).expect("key");
    let epoch = fixture.head_epoch + 1;
    fixture.source.requested_generation =
        super::copy_index_v3_rebuild_generation(&key, &lost.blob_sha256, epoch).expect("successor");
    fixture.claim = coordinate(&fixture.store, &fixture.source, NOW + 10).await;
    assert!(fixture
        .store
        .quarantine_copy_index_v3_head(super::background_jobs::QuarantineCopyIndexV3Head {
            catalog_namespace: NAMESPACE.into(),
            source: fixture.source.clone(),
            claim: fixture.claim.clone(),
            expected_epoch: epoch,
            bad_blob_sha256: lost.blob_sha256.clone(),
            code: code.into(),
            now_ms: NOW + 11,
        })
        .await
        .expect("quarantine"));
    fixture.head_epoch = epoch;
    let token = publisher(&fixture, &lost).await;
    let outcome = fixture
        .store
        .publish_copy_index_v3(publish(&fixture, &token, &lost, NOW + 12))
        .await
        .expect("re-publication");
    assert_eq!(
        fixture
            .store
            .copy_index_v3_quarantine_code(&key, epoch, &lost.blob_sha256)
            .await
            .expect("ledger")
            .as_deref(),
        Some(code)
    );
    assert_eq!(
        fixture
            .store
            .copy_index_v3_quarantine_code(&key, epoch + 1, &lost.blob_sha256)
            .await
            .expect("ledger"),
        None,
        "another epoch has its own record"
    );
    if matches!(outcome, JobPublishOutcome::Published { .. }) {
        let head = fixture
            .store
            .copy_index_v3_head(&key)
            .await
            .expect("head")
            .expect("head");
        assert_eq!(head.state, CopyIndexV3HeadState::Healthy);
        assert_eq!(head.head_epoch, epoch + 1);
        assert_eq!(head.blob_sha256.as_deref(), Some(lost.blob_sha256.as_str()));
        assert_eq!(
            head.published_generation.as_deref(),
            Some(fixture.source.requested_generation.as_str())
        );
    }
    outcome
}

/// Eviction's store half: every holder row of one node for one blob goes,
/// under every contract that names those bytes, and nobody else's.
#[tokio::test]
async fn forgetting_a_blob_holder_drops_that_nodes_rows_under_every_contract() {
    let fixture = fixture(true).await;
    let artifact = artifact('a', '1');
    published(&fixture, &artifact).await;
    let db = sql(&fixture);
    let hold = |contract: &str, node: &str| {
        db.execute(
            "INSERT INTO copy_index_v3_locations(contract_key,blob_sha256,node_id,bytes,verified_at_ms,last_seen_at_ms) VALUES(?1,?2,?3,4096,?4,?4)",
            rusqlite::params![contract, artifact.blob_sha256, node, NOW],
        )
        .expect("holder row");
    };
    hold(&artifact.contract_key, "node-two");
    hold(&"9".repeat(64), NODE);
    assert_eq!(
        fixture
            .store
            .forget_copy_index_v3_blob_holder(&artifact.blob_sha256, NODE)
            .await
            .expect("forget"),
        2
    );
    assert_eq!(
        fixture
            .store
            .forget_copy_index_v3_blob_holder(&artifact.blob_sha256, NODE)
            .await
            .expect("forget again"),
        0
    );
    let left: Vec<String> = db
        .prepare("SELECT node_id FROM copy_index_v3_locations")
        .expect("statement")
        .query_map([], |row| row.get(0))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("holders");
    assert_eq!(left, vec!["node-two".to_owned()]);
    assert!(fixture
        .store
        .forget_copy_index_v3_blob_holder("not-a-digest", NODE)
        .await
        .is_err());
}

/// A node is the last holder of a readable head's bytes when it is listed
/// and no other member is. A node the removal fence ejected is neither
/// listed to consumers nor counted.
#[tokio::test]
async fn the_last_member_holding_a_readable_heads_bytes_is_known_and_removed_nodes_hold_nothing() {
    let fixture = fixture(true).await;
    let artifact = artifact('a', '1');
    let last = |node: &'static str| {
        let (store, blob) = (&fixture.store, artifact.blob_sha256.clone());
        async move {
            store
                .copy_index_v3_last_blob_holder(&blob, node)
                .await
                .expect("last holder")
        }
    };
    assert!(!last(NODE).await, "a pending head has no readable bytes");
    published(&fixture, &artifact).await;
    assert!(last(NODE).await, "the publisher alone");
    assert!(
        !last(HYDRATOR).await,
        "an unlisted node holds nothing to keep"
    );

    let key = super::copy_index_v3_source_key(NAMESPACE, &fixture.source).expect("key");
    assert!(fixture
        .store
        .put_copy_index_v3_location(&key, location(&artifact, HYDRATOR, NOW + 2))
        .await
        .expect("second holder"));
    assert!(!last(NODE).await && !last(HYDRATOR).await);

    let db = sql(&fixture);
    db.execute(
        "INSERT INTO settings(key,value) VALUES(?1,'1')",
        [format!("internal.cluster_job_owner_removed.{HYDRATOR}")],
    )
    .expect("removal fence");
    assert!(last(NODE).await, "the removed node no longer counts");
    let listed = fixture
        .store
        .copy_index_v3_locations(&artifact.contract_key, &artifact.blob_sha256)
        .await
        .expect("holders");
    assert_eq!(
        listed
            .iter()
            .map(|location| location.node_id.as_str())
            .collect::<Vec<_>>(),
        vec![NODE],
        "a removed node is not offered as a holder"
    );
}

/// Covers the SQLite backend of durable demand: the coordination requests a
/// previous process left under one id prefix are listed (queued for this
/// node or running under it, not another node's, a later one or another
/// id), the ones taken up are kept by the stranded retirement, and a
/// dropped one is retired by id.
#[tokio::test]
async fn a_restarted_worker_lists_what_its_predecessor_left_and_keeps_what_it_takes_up() {
    let fixture = fixture(true).await;
    let boot = NOW + 200;
    let asked = queued_as(
        &fixture.store,
        "local-play.u7.playback-1.ab12",
        'd',
        NODE,
        NOW + 100,
    )
    .await;
    queued(&fixture.store, 'e', NODE, NOW + 100).await;
    queued_as(
        &fixture.store,
        "local-play.u7.playback-2.cd34",
        'f',
        "other-node",
        NOW + 100,
    )
    .await;
    queued_as(
        &fixture.store,
        "local-play.u7.playback-3.ef56",
        '9',
        NODE,
        NOW + 300,
    )
    .await;
    let listed = fixture
        .store
        .predecessor_analysis_coordination_with_prefix(NODE, boot, "local-play.")
        .await
        .expect("listing");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].request_id, asked);
    let mut expected = source();
    expected.requested_generation = "d".repeat(64);
    assert_eq!(listed[0].source, expected);

    // A claim the dead predecessor held is listed too: nothing holds it.
    let held = queued_as(
        &fixture.store,
        "local-play.u7.playback-9.aa00",
        '8',
        NODE,
        NOW + 100,
    )
    .await;
    let mut claimed = source();
    claimed.requested_generation = "8".repeat(64);
    fixture
        .store
        .claim_analysis_coordination(&held, claimed, NODE, 0, NOW + 150, NOW + 600_000)
        .await
        .expect("claim")
        .expect("the predecessor's claim");
    let mut both = fixture
        .store
        .predecessor_analysis_coordination_with_prefix(NODE, boot, "local-play.")
        .await
        .expect("listing")
        .into_iter()
        .map(|row| row.request_id)
        .collect::<Vec<_>>();
    both.sort();
    let mut expected_ids = vec![asked.clone(), held.clone()];
    expected_ids.sort();
    assert_eq!(
        both, expected_ids,
        "queued for the node, or running under it"
    );
    let db = sql(&fixture);

    // Taken up, it is kept by the stranded retirement; the rest go.
    assert_eq!(
        fixture
            .store
            .retire_stranded_analysis_coordination(
                NODE,
                boot,
                NOW + 400,
                std::slice::from_ref(&asked)
            )
            .await
            .expect("retire"),
        1,
        "the other queued coordinator of this node"
    );
    let state: String = db
        .query_row(
            "SELECT state FROM analysis_requests WHERE request_id=?1",
            [&asked],
            |row| row.get(0),
        )
        .expect("kept request");
    assert_eq!(state, "queued");

    // Dropped by its worker, it is retired by id; only a queued one is.
    assert!(fixture
        .store
        .retire_queued_analysis_coordination(&asked, NOW + 500)
        .await
        .expect("retire by id"));
    assert!(!fixture
        .store
        .retire_queued_analysis_coordination(&asked, NOW + 501)
        .await
        .expect("retire by id again"));
    fixture
        .store
        .retire_stranded_analysis_coordination(NODE, boot, NOW + 600, &[])
        .await
        .expect("retire");
    let left = fixture
        .store
        .predecessor_analysis_coordination_with_prefix(NODE, boot, "local-play.")
        .await
        .expect("listing");
    assert_eq!(
        left.iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        vec![held.as_str()],
        "the claimed one is never retired as queued"
    );
    assert!(fixture
        .store
        .predecessor_analysis_coordination_with_prefix(NODE, boot, "")
        .await
        .is_err());
}
