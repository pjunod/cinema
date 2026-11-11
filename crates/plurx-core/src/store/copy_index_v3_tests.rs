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
