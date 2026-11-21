//! The claim local verdict (docs/cluster/CLAIM-LOCAL-VERDICT-BUILD.md §5.2).
//!
//! An embedded voter reads its own applied replica before it claims: a claim
//! the replica shows the claim statement refusing is skipped, with the failing
//! predicate group named, and costs neither the authoritative pre-read nor a
//! proposal. The SQLite store is its own authority and keeps proposing; on the
//! same state its proposal is the statement itself refusing, which pairs every
//! blocked verdict with a real refusal.
#![cfg(feature = "hiqlite-store")]

use hiqlite::{params, Client, Node, NodeConfig};
use plurx_core::store::background_jobs::{
    BackgroundJobStore, CandidateQuery, ClaimBlock, ClaimJob, ClaimOutcome, ClaimPath, EnqueueJob,
    JobKind, JobPayload, JobRequest, MAX_PAGE_SIZE,
};
use plurx_core::store::{HiqliteAuthStore, SqliteStore};
use std::{borrow::Cow, path::Path, time::Duration};

const NOW: i64 = 10_000;

fn fragment_job(file_id: i64) -> EnqueueJob {
    EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: JobPayload::FragmentIndexBuild {
            file_id,
            source_generation: format!("source:{file_id}"),
            source_size: 100,
            source_mtime: 1,
            source_sha256: "d".repeat(64),
            cache_key: format!("{file_id:0>64}"),
            pipeline_digest: "a".repeat(64),
        },
        dedupe_key: format!("fragment:{file_id}"),
        priority: 1,
        not_before_ms: NOW - 100,
        now_ms: NOW - 100,
        request: JobRequest {
            scope: "user:1".to_owned(),
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: "b".repeat(64),
            consumer_kind: "analysis".to_owned(),
            consumer_ref: format!("analysis:{file_id}"),
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
    }
}

fn claim(job_id: &str, revision: i64, kind: JobKind) -> ClaimJob {
    ClaimJob {
        job_id: job_id.to_owned(),
        expected_revision: revision,
        node_id: "node-a".to_owned(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        kind,
        payload_version: 1,
        now_ms: NOW,
        dispatched_at_ms: NOW,
    }
}

/// The two stores under the same state. Raw rows go to both, through the
/// SQLite file and through the Raft log.
struct Pair {
    sqlite: SqliteStore,
    db: rusqlite::Connection,
    raft: Client,
    replica: HiqliteAuthStore,
}

impl Pair {
    async fn raw(&self, sql: &str) {
        self.db.execute_batch(sql).expect("SQLite raw state");
        self.raft
            .execute(sql.to_owned(), params!())
            .await
            .expect("replicated raw state");
    }

    /// One job enqueued identically on both stores.
    async fn job(&self, file_id: i64) -> String {
        let job = fragment_job(file_id);
        self.sqlite
            .enqueue_job(job.clone())
            .await
            .expect("SQLite enqueue");
        self.replica
            .enqueue_job(job.clone())
            .await
            .expect("replicated enqueue");
        job.id
    }

    async fn hold_source(&self, slot: i64, job: &str) {
        self.raw(&format!(
            "INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms)
             VALUES('source_io',{slot},'{job}',1,{})",
            NOW + 60_000
        ))
        .await;
    }

    async fn foreground_owner(&self, lease_expires_at_ms: i64) {
        self.raw(&format!(
            "INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,
               projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms)
             VALUES('viewer','{{}}','[\"source_io\"]',1,'startup','{{}}',{},{},{lease_expires_at_ms})",
            NOW - 1_000,
            NOW - 1_000
        ))
        .await;
    }

    /// The SQLite store proposes and the statement refuses; the replica
    /// skips with `block`. Both answer `Contended`.
    async fn assert_blocked(&self, request: &ClaimJob, block: ClaimBlock) {
        let authority = self
            .sqlite
            .claim_job_attempt(request.clone())
            .await
            .expect("SQLite claim");
        assert_eq!(authority.path, ClaimPath::Proposed, "{block:?}");
        assert!(
            matches!(authority.outcome, ClaimOutcome::Contended),
            "{block:?}: CLAIM_SQL must refuse, got {:?}",
            authority.outcome
        );
        let local = self
            .replica
            .claim_job_attempt(request.clone())
            .await
            .expect("replicated claim");
        assert_eq!(local.path, ClaimPath::Skipped(block));
        assert!(matches!(local.outcome, ClaimOutcome::Contended));
    }
}

/// An embedded single voter and a SQLite store, side by side.
async fn pair(root: &Path) -> Pair {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listeners: Vec<_> = (0..2)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("owned port"))
        .collect();
    let addresses: Vec<_> = listeners
        .iter()
        .map(|listener| listener.local_addr().expect("listener address"))
        .collect();
    drop(listeners);
    let raft = Box::pin(hiqlite::start_node(NodeConfig {
        node_id: 1,
        nodes: vec![Node {
            id: 1,
            addr_raft: addresses[0].to_string(),
            addr_api: addresses[1].to_string(),
        }],
        listen_addr_api: Cow::Borrowed("127.0.0.1"),
        listen_addr_raft: Cow::Borrowed("127.0.0.1"),
        data_dir: Cow::Owned(root.join("node").to_string_lossy().into_owned()),
        filename_db: Cow::Borrowed("claim-verdict.db"),
        secret_api: "owned-claim-verdict-api".into(),
        secret_raft: "owned-claim-verdict-raft".into(),
        tls_api: None,
        tls_raft: None,
        ..plurx_core::cluster::migration::production_hiqlite_defaults_with_read_pool(1)
    }))
    .await
    .expect("start an embedded voter");
    let replica = Box::pin(HiqliteAuthStore::bootstrap(
        raft.clone(),
        "11111111-1111-4111-8111-111111111170",
        &root.join("telemetry.db"),
    ))
    .await
    .expect("replicated store");
    let path = root.join("standalone.db");
    let sqlite = SqliteStore::open(&path).expect("SQLite store");
    let db = rusqlite::Connection::open(&path).expect("SQLite raw connection");
    Pair {
        sqlite,
        db,
        raft,
        replica,
    }
}

async fn shutdown(pair: Pair) {
    let Pair { raft, replica, .. } = pair;
    drop(replica);
    raft.shutdown().await.expect("drain the embedded voter");
}

async fn scenario(root: &Path) {
    let pair = pair(root).await;
    let fragment = JobKind::FragmentIndexBuild;
    let holders = [pair.job(1).await, pair.job(2).await];

    // Identity and state: a stale revision.
    let job = pair.job(10).await;
    pair.assert_blocked(&claim(&job, 7, fragment), ClaimBlock::State)
        .await;

    // Kind demand: a subtitle extraction whose analysis request is missing.
    let job = pair.job(11).await;
    pair.raw(&format!(
        "UPDATE background_jobs SET kind='subtitle_extract', payload_json=json_object('kind','subtitle_extract',
           'file_id',11,'source_generation','missing-request','track',NULL,'pipeline_digest','{}')
         WHERE id='{job}'",
        "a".repeat(64)
    ))
    .await;
    pair.assert_blocked(
        &claim(&job, 0, JobKind::SubtitleExtract),
        ClaimBlock::KindDemand,
    )
    .await;

    // A live foreground owner outside its permission window.
    let job = pair.job(12).await;
    pair.foreground_owner(NOW + 60_000).await;
    pair.assert_blocked(&claim(&job, 0, fragment), ClaimBlock::ForegroundWindow)
        .await;
    pair.raw("DELETE FROM foreground_source_owners").await;

    // Expired custody: the owner's lease lapsed while it still holds the source.
    pair.foreground_owner(NOW - 1).await;
    pair.assert_blocked(&claim(&job, 0, fragment), ClaimBlock::ForegroundWindow)
        .await;
    pair.raw("DELETE FROM foreground_source_owners").await;

    // Two holders.
    pair.hold_source(0, &holders[0]).await;
    pair.hold_source(1, &holders[1]).await;
    pair.assert_blocked(&claim(&job, 0, fragment), ClaimBlock::SourceHolders)
        .await;

    // One held maintenance reader: the second reader is reserved for live demand.
    pair.raw("DELETE FROM background_job_reservations WHERE slot=1")
        .await;
    pair.assert_blocked(&claim(&job, 0, fragment), ClaimBlock::MaintenanceReader)
        .await;

    // A worker can disappear with the physical reader still charged. Admission
    // must reserve the viewer slot even after its reservation row is pruned.
    pair.raw("UPDATE background_job_reservations SET expires_at_ms=0")
        .await;
    pair.raw(&format!(
        "INSERT INTO background_source_custody(custody_id,owner_key,owner_json,resource_keys_json,owner_kind,
           work_id,fence,phase,started_at_ms,updated_at_ms,lease_expires_at_ms)
         VALUES('lapsed','lost-reader','{{}}','[\"source_io\"]','job','{}',1,'active',0,0,0)",
        holders[0]
    )).await;
    pair.assert_blocked(&claim(&job, 0, fragment), ClaimBlock::MaintenanceReader)
        .await;
    pair.raw("DELETE FROM background_job_reservations").await;
    pair.assert_blocked(&claim(&job, 0, fragment), ClaimBlock::MaintenanceReader)
        .await;

    // Admissible: both propose, and both win.
    pair.raw("DELETE FROM background_source_custody WHERE custody_id='lapsed'")
        .await;
    let request = claim(&job, 0, fragment);
    for (backend, store) in [
        ("sqlite", &pair.sqlite as &dyn BackgroundJobStore),
        ("replicated", &pair.replica as &dyn BackgroundJobStore),
    ] {
        let admitted = store
            .claim_job_attempt(request.clone())
            .await
            .expect("admitted claim");
        assert_eq!(admitted.path, ClaimPath::Proposed, "{backend}");
        assert!(
            matches!(admitted.outcome, ClaimOutcome::Claimed { .. }),
            "{backend}: {:?}",
            admitted.outcome
        );
        // Its own replay is the pre-read's to answer, not the verdict's.
        let replay = store
            .claim_job_attempt(request.clone())
            .await
            .expect("replayed claim");
        assert_eq!(replay.path, ClaimPath::Settled, "{backend}");
        assert!(matches!(replay.outcome, ClaimOutcome::Claimed { .. }));
    }

    shutdown(pair).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_verdict_skips_exactly_the_claims_the_statement_refuses() {
    let root = tempfile::tempdir().expect("fixture directory");
    tokio::time::timeout(Duration::from_secs(90), Box::pin(scenario(root.path())))
        .await
        .expect("bounded scenario");
}

/// A worker pass behind a held source slot (plan §5.3): the candidate
/// listing is assembled from the claim statement's fragments, so a deep
/// queue the statement would refuse lists nothing and the pass proposes
/// nothing; once the slot is released, one pass claims one job.
async fn blocked_queue(root: &Path) {
    // `PLURX_CLAIM_VERDICT_DEPTH` sizes the queue for a cost measurement.
    let depth: i64 = std::env::var("PLURX_CLAIM_VERDICT_DEPTH")
        .ok()
        .and_then(|depth| depth.parse().ok())
        .unwrap_or(300);
    let pair = pair(root).await;
    let fragment = JobKind::FragmentIndexBuild;
    // The holder: a claimed maintenance reader of the one source slot.
    let holder = pair.job(1).await;
    let request = claim(&holder, 0, fragment);
    for store in [
        &pair.sqlite as &dyn BackgroundJobStore,
        &pair.replica as &dyn BackgroundJobStore,
    ] {
        let held = store
            .claim_job_attempt(request.clone())
            .await
            .expect("holder claim");
        assert!(matches!(held.outcome, ClaimOutcome::Claimed { .. }));
    }
    let mut queued = Vec::new();
    for n in 0..depth {
        queued.push(pair.job(100 + n).await);
    }
    let query = || CandidateQuery {
        node_id: "node-a".into(),
        kinds: vec![fragment],
        after: None,
        now_ms: NOW + 1,
        limit: MAX_PAGE_SIZE,
        admissible_only: true,
    };
    for (backend, store) in [
        ("sqlite", &pair.sqlite as &dyn BackgroundJobStore),
        ("replicated", &pair.replica as &dyn BackgroundJobStore),
    ] {
        let started = std::time::Instant::now();
        let page = store.job_candidates(query()).await.expect("candidates");
        let listing = started.elapsed();
        let started = std::time::Instant::now();
        let verdict = store
            .claim_job_attempt(ClaimJob {
                now_ms: NOW + 1,
                dispatched_at_ms: NOW + 1,
                ..claim(&queued[0], 0, fragment)
            })
            .await
            .expect("blocked claim");
        let claim_cost = started.elapsed();
        eprintln!(
            "{backend}: depth {depth} behind a held slot: candidate page {listing:?} \
             ({} jobs), one claim {claim_cost:?} ({:?})",
            page.jobs.len(),
            verdict.path
        );
        let unfiltered = store
            .job_candidates(CandidateQuery {
                admissible_only: false,
                ..query()
            })
            .await
            .expect("unfiltered candidates");
        assert!(
            !unfiltered.jobs.is_empty(),
            "{backend}: a listing that did not ask still sees the blocked queue"
        );
        if backend == "sqlite" {
            // Its own authority: it lists, and its claims propose, as before.
            assert!(!page.jobs.is_empty(), "SQLite never filters");
            assert_eq!(verdict.path, ClaimPath::Proposed);
        } else {
            assert!(
                page.jobs.is_empty(),
                "{backend}: a queue the statement refuses lists no admissible candidate"
            );
        }
    }
    pair.raw(&format!(
        "DELETE FROM background_job_reservations WHERE job_id='{holder}'"
    ))
    .await;
    for (backend, store) in [
        ("sqlite", &pair.sqlite as &dyn BackgroundJobStore),
        ("replicated", &pair.replica as &dyn BackgroundJobStore),
    ] {
        let started = std::time::Instant::now();
        let page = store.job_candidates(query()).await.expect("candidates");
        eprintln!(
            "{backend}: depth {depth} released: candidate page {:?} ({} jobs)",
            started.elapsed(),
            page.jobs.len()
        );
        assert!(
            !page.jobs.is_empty(),
            "{backend}: released, the queue lists again"
        );
        let first = &page.jobs[0];
        let claimed = store
            .claim_job_attempt(ClaimJob {
                now_ms: NOW + 1,
                dispatched_at_ms: NOW + 1,
                ..claim(&first.id, first.revision, fragment)
            })
            .await
            .expect("released claim");
        assert_eq!(claimed.path, ClaimPath::Proposed, "{backend}");
        assert!(
            matches!(claimed.outcome, ClaimOutcome::Claimed { .. }),
            "{backend}"
        );
        // One reader now holds the slot again: the rest list nothing.
        if backend != "sqlite" {
            let after = store.job_candidates(query()).await.expect("candidates");
            assert!(after.jobs.is_empty(), "{backend}: {}", after.jobs.len());
        }
    }
    shutdown(pair).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queue_behind_a_held_source_slot_lists_nothing_until_it_is_released() {
    let root = tempfile::tempdir().expect("fixture directory");
    // A deep measurement queue enqueues through the log one job at a time.
    tokio::time::timeout(
        Duration::from_secs(1_200),
        Box::pin(blocked_queue(root.path())),
    )
    .await
    .expect("bounded scenario");
}

/// A manual scan queued behind a held maintenance reader keeps its library
/// turn (adversarial review B1): the library pass's listing is unfiltered,
/// so the scan is still listed and advertised, its `dispatch_turn_ready` row
/// exists, and when the reader is released a competing maintenance claim
/// yields to it on both backends.
async fn scan_turn(root: &Path) {
    use plurx_core::domain::{LibraryKind, NewLibrary};
    use plurx_core::store::background_jobs::{EnqueueOutcome, LibraryTurnCapacity, NewLibraryWork};
    use plurx_core::store::background_jobs_library::{LibraryTrigger, LibraryWorkInput};
    use plurx_core::store::LibraryStore;
    let pair = pair(root).await;
    let fragment = JobKind::FragmentIndexBuild;
    let holder = pair.job(1).await;
    let competitor = pair.job(2).await;
    let holding = claim(&holder, 0, fragment);
    let mut scans = Vec::new();
    for (backend, store, libraries) in [
        (
            "sqlite",
            &pair.sqlite as &dyn BackgroundJobStore,
            &pair.sqlite as &dyn LibraryStore,
        ),
        (
            "replicated",
            &pair.replica as &dyn BackgroundJobStore,
            &pair.replica as &dyn LibraryStore,
        ),
    ] {
        let held = store
            .claim_job_attempt(holding.clone())
            .await
            .expect("holder claim");
        assert!(
            matches!(held.outcome, ClaimOutcome::Claimed { .. }),
            "{backend}"
        );
        let library = libraries
            .create_library(&NewLibrary {
                name: "Scan".into(),
                paths: vec!["/scan".into()],
                kind: LibraryKind::Movies,
                anime: false,
            })
            .await
            .expect("library");
        let EnqueueOutcome::Accepted { job_id, .. } = store
            .enqueue_library_work(NewLibraryWork {
                request_id: "manual".into(),
                library_id: library.id,
                input: LibraryWorkInput::Full {
                    refresh: false,
                    trigger: LibraryTrigger::Manual,
                },
                now_ms: NOW,
            })
            .await
            .expect("manual scan")
        else {
            panic!("{backend}: expected a new scan")
        };
        let listed = store
            .job_candidates(CandidateQuery {
                node_id: "node-a".into(),
                kinds: vec![JobKind::LibraryScan, JobKind::MetadataRefresh],
                after: None,
                now_ms: NOW + 1,
                limit: MAX_PAGE_SIZE,
                admissible_only: false,
            })
            .await
            .expect("library candidates");
        assert!(
            listed.jobs.iter().any(|job| job.id == job_id),
            "{backend}: the blocked scan is still listed for its turn"
        );
        assert!(
            store
                .advertise_library_turn(
                    &job_id,
                    0,
                    "node-a",
                    &library.paths,
                    LibraryTurnCapacity::Available,
                    NOW + 1,
                )
                .await
                .expect("advertise"),
            "{backend}: the turn is advertised"
        );
        scans.push((backend, job_id));
    }
    let ready = "SELECT count(*) FROM dispatch_turn_ready";
    let sqlite_ready: i64 = pair
        .db
        .query_row(ready, [], |row| row.get(0))
        .expect("SQLite ready rows");
    assert!(sqlite_ready > 0, "sqlite: the scan holds a ready turn");
    pair.raw(&format!(
        "DELETE FROM background_job_reservations WHERE job_id='{holder}'"
    ))
    .await;
    for ((backend, scan), store) in scans.iter().zip([
        &pair.sqlite as &dyn BackgroundJobStore,
        &pair.replica as &dyn BackgroundJobStore,
    ]) {
        let competing = store
            .claim_job_attempt(ClaimJob {
                now_ms: NOW + 2,
                dispatched_at_ms: NOW + 2,
                ..claim(&competitor, 0, fragment)
            })
            .await
            .expect("competing claim");
        assert!(
            matches!(competing.outcome, ClaimOutcome::Contended),
            "{backend}: maintenance yields to the advertised scan: {competing:?}"
        );
        let scanned = store
            .claim_job_attempt(ClaimJob {
                now_ms: NOW + 3,
                dispatched_at_ms: NOW + 3,
                ..claim(scan, 0, JobKind::LibraryScan)
            })
            .await
            .expect("scan claim");
        assert!(
            matches!(scanned.outcome, ClaimOutcome::Claimed { .. }),
            "{backend}: the scan takes its turn: {scanned:?}"
        );
    }
    shutdown(pair).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_behind_a_held_reader_keeps_its_turn_and_wins_the_freed_slot() {
    let root = tempfile::tempdir().expect("fixture directory");
    tokio::time::timeout(Duration::from_secs(120), Box::pin(scan_turn(root.path())))
        .await
        .expect("bounded scenario");
}
