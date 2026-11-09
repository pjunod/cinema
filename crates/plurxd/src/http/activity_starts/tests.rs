//! G1: the fleet read behind Activity → Starts.

use super::*;
use plurx_core::store::AnalysisRequest;

fn row(node: &str, attempt: &str, at: i64, outcome: &str) -> StartRow {
    StartRow {
        attempt: attempt.to_owned(),
        at_unix_ms: at,
        node_id: node.to_owned(),
        user_id: Some(1),
        file_id: Some(9),
        client_build: "web/Chrome".to_owned(),
        route: Some("rolling".to_owned()),
        refusal: None,
        refusal_message: None,
        retryable: false,
        phases: json!([]),
        owner_node: Some(node.to_owned()),
        elapsed_ms: Some(10),
        authority_reads: Some(0),
        local_reads: Some(0),
        mutations: Some(0),
        store_ms: Some(0),
        ttff_ms: None,
        outcome: outcome.to_owned(),
        failure: None,
    }
}

fn answered(node: &str, rows: Vec<StartRow>) -> NodeReport {
    NodeReport {
        node_id: node.to_owned(),
        unavailable: None,
        rows,
    }
}

fn unavailable(node: &str, reason: &'static str) -> NodeReport {
    NodeReport {
        node_id: node.to_owned(),
        unavailable: Some(reason),
        rows: Vec::new(),
    }
}

/// Two peers answer, a third does not: the rows of the two merge by time,
/// and the third is named, with its reason, in the page and in the count.
#[test]
fn two_peers_merge_by_time_and_the_one_that_did_not_answer_is_named() {
    let reports = [
        answered(
            "node-a",
            vec![
                row("node-a", "a2", 5_000, "presented"),
                row("node-a", "a1", 1_000, "refused"),
            ],
        ),
        answered("node-b", vec![row("node-b", "b1", 3_000, "unobserved")]),
        unavailable("node-c", "timed_out"),
    ];
    let page = assemble(&reports, 25, 10_000);
    let attempts: Vec<_> = page["rows"]
        .as_array()
        .expect("test value")
        .iter()
        .map(|row| row["attempt"].as_str().expect("test value"))
        .collect();
    assert_eq!(attempts, ["a2", "b1", "a1"]);
    assert_eq!(page["unavailable"], json!(["node-c"]));
    assert_eq!(
        page["nodes"][2],
        json!({"node_id": "node-c", "status": "unavailable", "reason": "timed_out"})
    );
    assert_eq!(page["nodes"][0]["status"], "answered");
    assert_eq!(page["observation"]["unavailable_nodes"], 1);
    assert_eq!(page["observation"]["attempts"], 3);
    assert_eq!(page["observation"]["unobserved"], 1);
    assert_eq!(page["observation"]["refused"], 1);
    assert_eq!(page["observation"]["presented"], 1);
    assert_eq!(page["next_before"], Value::Null, "nothing older was cut");
}

#[test]
fn a_page_that_is_cut_hands_back_a_cursor_that_loses_no_row() {
    let reports = [
        answered(
            "node-a",
            vec![
                row("node-a", "a3", 9_000, "presented"),
                row("node-a", "a2", 7_000, "presented"),
            ],
        ),
        answered(
            "node-b",
            vec![
                row("node-b", "b2", 7_000, "presented"),
                row("node-b", "b1", 2_000, "presented"),
            ],
        ),
    ];
    // limit 2 would cut between the two rows that share 7_000 ms; the page
    // keeps both so a strictly-older next page cannot skip one of them.
    let (rows, next) = merge_pages(&reports, 2);
    assert_eq!(
        rows.iter().map(|r| r.attempt.as_str()).collect::<Vec<_>>(),
        ["a3", "a2", "b2"]
    );
    assert_eq!(next, Some(7_000));
}

#[test]
fn a_full_page_from_any_node_means_there_may_be_more() {
    let reports = [
        answered("node-a", vec![row("node-a", "a1", 4_000, "presented")]),
        unavailable("node-b", "unreachable"),
    ];
    let (_, next) = merge_pages(&reports, 1);
    assert_eq!(
        next,
        Some(4_000),
        "a node that returned `limit` rows may hold older ones"
    );
    let (_, none) = merge_pages(&reports, 5);
    assert_eq!(none, None);
}

fn job(file: i64, request: &str, state: &str, error: &str) -> AnalysisRequest {
    AnalysisRequest {
        request_id: request.to_owned(),
        file_id: file,
        source_size: 0,
        source_mtime: 0,
        component: "fragment_index".to_owned(),
        pipeline_version: String::new(),
        video_identity: String::new(),
        requested_generation: String::new(),
        expected_predecessor_generation: String::new(),
        priority: String::new(),
        trigger: String::new(),
        force_rebuild: false,
        target_node_id: String::new(),
        state: state.to_owned(),
        owner_node_id: String::new(),
        fence: 0,
        lease_expires_ms: 0,
        attempts: 0,
        not_before_ms: 0,
        result_cache_key: String::new(),
        last_error_code: error.to_owned(),
        cancel_requested: false,
        created_at_ms: 0,
        updated_at_ms: 0,
    }
}

/// `index now` is the retry for a terminal job, an enqueue when there is no
/// job, and a join (not a second job) when one is already working.
#[test]
fn index_now_retries_enqueues_or_joins() {
    // One request per file, as `fragment_index_requests_for_files` answers
    // (an active one wins over an older terminal one inside the Store).
    let jobs = jobs_by_file(&[
        job(2, "failed-2", "failed", "attempt_limit"),
        job(3, "working-3", "running", ""),
        job(4, "stale-4", "failed", "source_superseded"),
        job(6, "done-6", "ready", ""),
    ]);
    let none = IndexJob::None;
    let action = |file: i64, status: Option<&str>| {
        index_action(file, status, jobs.get(&file).unwrap_or(&none))
    };
    assert_eq!(
        action(2, Some("refused")),
        json!({"kind": "retry", "request_id": "failed-2", "state": "failed", "method": "POST",
               "path": "/api/v1/analysis/jobs/failed-2/retry"})
    );
    assert_eq!(
        action(3, Some("pending")),
        json!({"kind": "joining", "request_id": "working-3", "state": "running"}),
        "active work is joined"
    );
    assert_eq!(
        action(4, Some("pending")),
        json!({"kind": "enqueue", "method": "POST", "path": "/api/v1/files/4/analysis",
               "body": {"components": ["fragment_index"]}}),
        "a superseded job cannot be retried; the file is asked afresh"
    );
    assert_eq!(action(5, Some("partial"))["kind"], "enqueue");
    assert_eq!(
        action(6, Some("pending"))["kind"],
        "enqueue",
        "a finished request whose index is gone is asked afresh"
    );
    assert_eq!(action(5, Some("indexed")), json!({"kind": "none"}));
    assert_eq!(action(5, None), json!({"kind": "none"}));
}

/// The sidecar half and the page: a create, its first frame and a refusal
/// come back as the rows the Activity tab explains.
#[tokio::test]
async fn the_local_page_joins_creates_with_what_the_client_reported() {
    use crate::playstart::{presented_event, StartAttemptRecord};

    let state = crate::http::tests::test_state_without_router();
    let now = 1_700_000_100_000;
    let record = |attempt: &str, at: i64, session: Option<&str>, refusal: Option<&str>| {
        StartAttemptRecord {
            at_unix_ms: at,
            user_id: 1,
            file_id: 9,
            attempt: attempt.to_owned(),
            session_id: session.map(str::to_owned),
            route: session.map(|_| "rolling"),
            refusal: refusal.map(str::to_owned),
            refusal_message: None,
            client_build: "ios/86".to_owned(),
            owner_node: Some("node-b".to_owned()),
            phases: "plan:3ms/1a/0l/0w".to_owned(),
            authority_reads: 1,
            local_reads: 0,
            mutations: 0,
            store_ms: 2,
            elapsed_ms: 9,
        }
        .into_event()
    };
    let events = vec![
        record("slow", now - 90_000, Some("s-slow"), None),
        record("failed", now - 60_000, Some("s-failed"), None),
        record(
            "refused",
            now - 50_000,
            None,
            Some("transcode_capacity_pending"),
        ),
        record("silent", now - 40_000, Some("s-silent"), None),
        presented_event(
            "slow",
            (1, 9),
            Some("s-slow"),
            Some("remux"),
            Some(31_000),
            Some("cold-start"),
            now - 59_000,
        ),
        PlaybackEvent {
            event: "hls_fatal".to_owned(),
            attempt: Some("failed".to_owned()),
            session_id: Some(crate::transcode::session_log_id("s-failed")),
            at_unix_ms: now - 58_000,
            ..PlaybackEvent::default()
        },
    ];
    state
        .store
        .record_playback_batch(&events, &[])
        .await
        .expect("write");

    let rows = local_page(&state.store, "node-a", None, 10, now)
        .await
        .expect("page");
    let by = |attempt: &str| {
        rows.iter()
            .find(|row| row.attempt == attempt)
            .expect("test value")
    };
    assert_eq!(by("slow").outcome, "presented");
    assert_eq!(by("slow").ttff_ms, Some(31_000));
    assert_eq!(by("failed").outcome, "failed");
    assert_eq!(by("failed").failure.as_deref(), Some("hls_fatal"));
    assert_eq!(by("refused").outcome, "refused");
    assert!(
        by("refused").retryable,
        "transcode_capacity_pending is a retried code"
    );
    assert_eq!(
        by("silent").outcome,
        "pending",
        "40 s old: the client may still report"
    );
    let later = local_page(&state.store, "node-a", None, 10, now + 200_000)
        .await
        .expect("later page");
    assert_eq!(
        later
            .iter()
            .find(|row| row.attempt == "silent")
            .expect("test value")
            .outcome,
        "unobserved",
        "past the start deadline an unreported create is a missing observation"
    );
    assert_eq!(by("slow").client_build, "ios/86");
    assert_eq!(by("slow").owner_node.as_deref(), Some("node-b"));
    assert_eq!(by("slow").phases[0]["name"], "plan");
    assert_eq!(by("slow").node_id, "node-a");

    // Paged: the second page is strictly older.
    let first = local_page(&state.store, "node-a", None, 2, now)
        .await
        .expect("test value");
    assert_eq!(first.len(), 2);
    let second = local_page(&state.store, "node-a", Some(first[1].at_unix_ms), 2, now)
        .await
        .expect("test value");
    assert!(second
        .iter()
        .all(|row| row.at_unix_ms < first[1].at_unix_ms));
    assert_eq!(first.len() + second.len(), 4);
}

/// Review 2 of G1 (S3): a node's page that ends inside a millisecond it holds
/// more creates at comes back with all of them, so the next (strictly older)
/// page loses none.
#[tokio::test]
async fn a_full_page_carries_every_create_of_its_last_millisecond() {
    use crate::playstart::StartAttemptRecord;

    let state = crate::http::tests::test_state_without_router();
    let now = 1_700_000_100_000;
    let record = |attempt: &str, at: i64| {
        StartAttemptRecord {
            at_unix_ms: at,
            user_id: 1,
            file_id: 9,
            attempt: attempt.to_owned(),
            session_id: Some(format!("s-{attempt}")),
            route: Some("rolling"),
            refusal: None,
            refusal_message: None,
            client_build: "web/Chrome".to_owned(),
            owner_node: None,
            phases: String::new(),
            authority_reads: 0,
            local_reads: 0,
            mutations: 0,
            store_ms: 0,
            elapsed_ms: 1,
        }
        .into_event()
    };
    let tie = now - 10_000;
    let events = vec![
        record("newest", now - 1_000),
        record("tie-a", tie),
        record("tie-b", tie),
        record("tie-c", tie),
        record("older", now - 20_000),
    ];
    state
        .store
        .record_playback_batch(&events, &[])
        .await
        .expect("write");
    let first = local_page(&state.store, "node-a", None, 2, now)
        .await
        .expect("page");
    let mut attempts = first
        .iter()
        .map(|row| row.attempt.as_str())
        .collect::<Vec<_>>();
    attempts.sort_unstable();
    assert_eq!(attempts, ["newest", "tie-a", "tie-b", "tie-c"]);
    let (merged, next) = merge_pages(&[answered("node-a", first)], 2);
    assert_eq!(merged.len(), 4, "the merge keeps the tie whole");
    assert_eq!(next, Some(tie));
    let second = local_page(&state.store, "node-a", next, 2, now)
        .await
        .expect("page");
    assert_eq!(
        second
            .iter()
            .map(|row| row.attempt.as_str())
            .collect::<Vec<_>>(),
        ["older"],
        "nothing at the boundary is skipped or repeated"
    );
}

/// Review 2 of G1 (S6, S8): the fleet read end to end, on a real committed
/// cluster. The ingress (the leader) asks every committed member through the
/// signed internal-peer transport: a learner that serves the production peer
/// mount answers with its own rows, signed; a second learner whose HTTP is
/// gone is named unavailable instead of left out; and the merged page is
/// decorated with the title and its index state. Learners appear at all only
/// because the read uses the committed-member directory (`activity_peers`
/// lists voters only).
#[cfg(feature = "cluster")]
mod fleet {
    use super::*;
    use axum::extract::DefaultBodyLimit;
    use futures_util::FutureExt;
    use plurx_core::cluster::migration::{select_daemon_store_observing, SelectedStore};
    use plurx_core::config::Config;
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    use std::path::PathBuf;

    fn config(root: &std::path::Path) -> Config {
        let held: Vec<_> = (0..3)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("owned port"))
            .collect();
        let addresses: Vec<_> = held
            .iter()
            .map(|socket| socket.local_addr().expect("owned fixture invariant"))
            .collect();
        let mut config = Config::default();
        config.storage.data_dir = root.into();
        config.storage.mode = Some(plurx_core::storage::mode::StorageMode::Cluster);
        config.server.bind = addresses[0];
        config.cluster.raft_bind = addresses[1];
        config.cluster.api_bind = addresses[2];
        config.cluster.advertise_host = "localhost".into();
        config.cluster.join_url = format!("http://{}", addresses[0]);
        config.cluster.artwork_url = config.cluster.join_url.clone();
        config
    }

    /// A learner admitted through the production token-file join.
    async fn join_learner(
        leader: &plurx_core::cluster::membership::MembershipManager,
    ) -> (
        tempfile::TempDir,
        Arc<crate::StartupObservationHttp>,
        SelectedStore,
    ) {
        let token = leader
            .issue_learner_token(Duration::from_secs(120))
            .await
            .expect("real learner token");
        let root = crate::test_tempdir().expect("owned learner root");
        let token_path = root.path().join("join.token");
        tokio::fs::write(&token_path, format!("{}\n", token.token))
            .await
            .expect("owned learner token file");
        let mut learner_config = config(root.path());
        learner_config.cluster.join_token_file = token_path;
        let startup = Arc::new(crate::StartupObservationHttp::new(
            learner_config.server.bind,
        ));
        let selected = tokio::time::timeout(
            Duration::from_secs(45),
            Box::pin(select_daemon_store_observing(
                &learner_config,
                Some(startup.as_ref()),
            )),
        )
        .await
        .expect("unchanged 45-second learner budget")
        .expect("actual finalized learner");
        (root, startup, selected)
    }

    fn record(attempt: &str, at: i64, file_id: i64) -> PlaybackEvent {
        crate::playstart::StartAttemptRecord {
            at_unix_ms: at,
            user_id: 1,
            file_id,
            attempt: attempt.to_owned(),
            session_id: Some(format!("s-{attempt}")),
            route: Some("rolling"),
            refusal: None,
            refusal_message: None,
            client_build: "web/Chrome".to_owned(),
            owner_node: None,
            phases: "plan:3ms/1a/0l/0w".to_owned(),
            authority_reads: 1,
            local_reads: 0,
            mutations: 0,
            store_ms: 1,
            elapsed_ms: 3,
        }
        .into_event()
    }

    #[test]
    fn the_fleet_read_asks_every_member_and_names_the_one_that_is_down() {
        let worker = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .thread_stack_size(8 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .expect("fleet fixture runtime")
                    .block_on(Box::pin(fleet_fixture()));
            })
            .expect("fleet fixture thread");
        if let Err(panic) = worker.join() {
            std::panic::resume_unwind(panic);
        }
    }

    async fn fleet_fixture() {
        let source_root = crate::test_tempdir().expect("owned leader root");
        let source_config = config(source_root.path());
        drop(
            plurx_core::store::SqliteStore::open(&source_root.path().join("plurx.db"))
                .expect("schema"),
        );
        let startup = Arc::new(crate::StartupObservationHttp::new(
            source_config.server.bind,
        ));
        let selected = select_daemon_store_observing(&source_config, Some(startup.as_ref()))
            .await
            .expect("actual singleton activation");
        let leader = selected.membership_manager();
        let mut ingress = crate::http::tests::test_state_without_router();
        ingress.node_id = selected.identity.node_id.clone();
        ingress.membership = Some(leader.clone());
        ingress.storage_mode = plurx_core::storage::mode::StorageMode::Cluster;
        ingress.cluster_advertisement = true;
        {
            let mut routes = startup.router.write().expect("leader routes");
            *routes = routes.clone().merge(
                axum::Router::new()
                    .route(
                        "/api/v1/cluster/learner/join/redeem",
                        axum::routing::post(crate::http::cluster::redeem_learner_join),
                    )
                    .route(
                        "/api/v1/cluster/learner/join/finalize",
                        axum::routing::post(crate::http::cluster::finalize_learner_join),
                    )
                    .with_state(ingress.clone()),
            );
        }
        leader
            .activate_learner_protocol()
            .await
            .expect("activate actual learner admission");
        let (answering_root, answering_startup, answering) = join_learner(&leader).await;
        let (silent_root, silent_startup, silent) = join_learner(&leader).await;
        let answering_id = answering.identity.node_id.clone();
        let silent_id = silent.identity.node_id.clone();

        // The answering learner serves the peer route as production mounts
        // it, from its own sidecar.
        let mut peer_state = crate::http::tests::test_state_without_router();
        peer_state.node_id = answering_id.clone();
        peer_state.membership = Some(answering.membership_manager());
        peer_state.storage_mode = plurx_core::storage::mode::StorageMode::Cluster;
        {
            let mut routes = answering_startup.router.write().expect("learner routes");
            *routes = routes.clone().merge(
                axum::Router::new()
                    .route(
                        PEER_PATH,
                        axum::routing::post(peer).layer(DefaultBodyLimit::max(1_024)),
                    )
                    .with_state(peer_state.clone()),
            );
        }
        // The silent learner's HTTP goes away.
        silent_startup.stop_and_drain().await;

        let now = crate::state::clock_ms();
        let library = ingress
            .store
            .create_library(&NewLibrary {
                name: "Fleet".into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/fleet")],
                anime: false,
            })
            .await
            .expect("library");
        let item = ingress
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Fleet Movie".into(),
                year: Some(2024),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file_id = ingress
            .store
            .upsert_file(
                item,
                "/fleet/movie.mkv",
                10_000,
                1,
                &ProbeResult {
                    duration_ms: Some(7_200_000),
                    container: Some("mkv".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        ingress
            .store
            .record_playback_batch(&[record("leader-start", now - 2_000, file_id)], &[])
            .await
            .expect("leader rows");
        peer_state
            .store
            .record_playback_batch(&[record("learner-start", now - 1_000, file_id)], &[])
            .await
            .expect("learner rows");
        let admin = ingress
            .store
            .create_user("fleet-admin", "hash", true)
            .await
            .expect("admin");

        let checks = std::panic::AssertUnwindSafe(async {
            // The answering learner's heartbeat makes it reachable in the
            // committed directory the read asks.
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    answering
                        .membership_manager()
                        .heartbeat()
                        .await
                        .expect("learner heartbeat");
                    let peers = leader.operations_peers().await.expect("directory");
                    if peers
                        .iter()
                        .any(|p| p.node_id == answering_id && p.reachable)
                        && peers.iter().any(|p| p.node_id == silent_id)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            })
            .await
            .expect("both learners are committed members");
            assert!(
                leader
                    .activity_peers()
                    .await
                    .expect("voters")
                    .iter()
                    .all(|p| p.node_id != answering_id),
                "the voters-only directory would have dropped the learner"
            );

            let Json(page) = starts(
                AdminUser(admin.clone()),
                State(ingress.clone()),
                Query(StartsParams::default()),
            )
            .await
            .expect("the fleet page");
            let attempts = page["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| row["attempt"].as_str().expect("attempt").to_owned())
                .collect::<Vec<_>>();
            assert_eq!(attempts, ["learner-start", "leader-start"], "{page}");
            assert_eq!(page["rows"][0]["node_id"], json!(answering_id), "{page}");
            assert_eq!(page["unavailable"], json!([silent_id]), "{page}");
            let silent_report = page["nodes"]
                .as_array()
                .expect("nodes")
                .iter()
                .find(|node| node["node_id"] == json!(silent_id))
                .cloned()
                .expect("the silent learner is named");
            assert_eq!(silent_report["status"], "unavailable", "{page}");
            assert_eq!(page["observation"]["unavailable_nodes"], 1, "{page}");
            assert_eq!(page["directory_unavailable"], false, "{page}");
            // Decorated at the ingress: the title and its index state.
            assert_eq!(page["rows"][1]["title"], "Fleet Movie", "{page}");
            assert!(page["rows"][1]["index"].get("action").is_some(), "{page}");

            // The peer route refuses an unsigned request and an oversized body.
            let client = reqwest::Client::new();
            let base = answering_startup_base(&leader, &answering_id).await;
            let unsigned = client
                .post(format!("{base}{PEER_PATH}"))
                .body(r#"{"limit":10,"before_ms":null}"#)
                .send()
                .await
                .expect("unsigned request");
            assert_eq!(unsigned.status(), reqwest::StatusCode::UNAUTHORIZED);
            let oversized = client
                .post(format!("{base}{PEER_PATH}"))
                .body(vec![b' '; 4_096])
                .send()
                .await
                .expect("oversized request");
            assert_eq!(oversized.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
        })
        .catch_unwind()
        .await;
        startup.stop_and_drain().await;
        answering_startup.stop_and_drain().await;
        let _ = answering.shutdown().await;
        let _ = silent.shutdown().await;
        let _ = selected.shutdown().await;
        drop((answering_root, silent_root));
        if let Err(panic) = checks {
            std::panic::resume_unwind(panic);
        }
    }

    async fn answering_startup_base(
        leader: &plurx_core::cluster::membership::MembershipManager,
        node_id: &str,
    ) -> String {
        leader
            .operations_peers()
            .await
            .expect("directory")
            .into_iter()
            .find(|peer| peer.node_id == node_id)
            .and_then(|peer| peer.http_base)
            .expect("the learner's HTTP base")
    }
}
