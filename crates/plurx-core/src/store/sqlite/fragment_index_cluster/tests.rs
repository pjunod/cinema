use crate::queue_fixture::QueueFixture;
use crate::store::cluster_fragment_index_key;
const MAX_ACTIVE_JOBS: i64 = 4_096;
const QUEUE_ELIGIBILITY_MS: i64 = 6 * 60 * 60 * 1_000;

/// Domain adapters no longer reopen execution rows. The shared queue's
/// per-interest ledger supplies both counters and history; runtime coverage
/// is `background_jobs_fragment_retry_keeps_its_window_history_and_configured_limit`.
#[test]
fn every_attempts_reset_resets_the_attempt_history() {
    const SOURCE: &str = include_str!("../fragment_index_cluster.rs");
    let production = SOURCE
        .split_once("\n#[cfg(test)]")
        .expect("test boundary")
        .0;
    assert!(
        !production.contains("cluster_fragment_index_jobs.attempts END"),
        "a domain adapter must not reset the common execution budget"
    );
    let shared = include_str!("../../background_jobs_schema.sql");
    assert!(shared.contains("SELECT MAX(interest.failed_attempts"));
    assert!(shared.contains("SELECT interest.attempt_errors"));
}

use std::collections::HashSet;

use super::*;
use crate::store::fragment_index_cluster::{
    analysis_backoff_ms, cluster_fragment_index_generation_key,
};
use crate::store::SettingsStore;

#[test]
fn full_migration_chain_bounds_both_fragment_prune_statements() {
    // Prefixes cross both analysis_requests rebuilds and the pre-index
    // upgrade. Never substitute a hand-written table fixture here.
    for prefix in [0, 40, 61, super::super::MIGRATIONS.len() - 1] {
        let conn = Connection::open_in_memory().expect("migration fixture");
        for migration in super::super::MIGRATIONS.iter().take(prefix) {
            conn.execute_batch(migration).expect("historical schema");
        }
        for migration in super::super::MIGRATIONS.iter().skip(prefix) {
            conn.execute_batch(migration).expect("remaining migration");
        }
        crate::store::fragment_prune_tests::assert_plans_and_work(
            &conn,
            crate::store::FRAGMENT_PRUNE_CANDIDATES,
            crate::store::FRAGMENT_PRUNE_TERMINAL_JOBS,
        );
    }
}

async fn seed_files(store: &SqliteStore) {
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO libraries (id, name, kind, paths) VALUES (1, 'films', 'movies', '[]')",
                [],
            )?;
            conn.execute(
                "INSERT INTO items (id, library_id, kind, title, sort_title)
                     VALUES (1, 1, 'movie', 'one', 'one'), (2, 1, 'movie', 'two', 'two')",
                [],
            )?;
            conn.execute(
                "INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (1, 1, '/one.mkv', 100, 10), (2, 2, '/two.mkv', 100, 20)",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("seed files");
}

async fn explain(store: &SqliteStore, sql: String) -> Vec<String> {
    store
        .with_read(move |conn| {
            let mut statement = conn.prepare(&sql)?;
            let details = statement
                .query_map([], |row| row.get::<_, String>(3))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(Into::into);
            details
        })
        .await
        .expect("query plan")
}

#[tokio::test]
async fn analysis_projections_use_the_result_key_history_index() {
    let store = SqliteStore::open_in_memory().expect("store");
    for (name, cte, relation) in [
        ("history", ANALYSIS_CANONICAL_CTE, "classified"),
        ("summary", ANALYSIS_SUMMARY_CTE, "summary_classified"),
    ] {
        let details = explain(
            &store,
            format!("EXPLAIN QUERY PLAN {cte} SELECT COUNT(*) FROM {relation}"),
        )
        .await;
        assert!(
            details.iter().any(|detail| {
                // The cleanup index is also a covering result-key/target probe.
                // Either index bounds this lookup after the append-only upgrade.
                (detail.contains("analysis_requests_result_history")
                    || detail.contains("analysis_requests_result_target_force"))
                    && detail.contains("result_cache_key=?")
            }),
            "{name} must probe standalone jobs by result cache key: {details:?}"
        );
    }
}

#[tokio::test]
async fn terminal_retention_uses_the_terminal_identity_index() {
    let store = SqliteStore::open_in_memory().expect("store");
    let details = explain(
        &store,
        "EXPLAIN QUERY PLAN
             SELECT candidate.request_id FROM analysis_requests candidate
              WHERE candidate.state IN ('ready', 'failed', 'cancelled')
                AND candidate.request_id <> 'new-request'
                AND (candidate.force_rebuild = 1
                  OR NOT EXISTS (SELECT 1 FROM files
                       WHERE files.id = candidate.file_id
                         AND files.size = candidate.source_size
                         AND files.mtime = candidate.source_mtime)
                  OR EXISTS (SELECT 1 FROM analysis_requests newer
                       WHERE newer.state IN ('ready', 'failed', 'cancelled')
                         AND newer.force_rebuild = 0
                         AND newer.file_id = candidate.file_id
                         AND newer.source_size = candidate.source_size
                         AND newer.source_mtime = candidate.source_mtime
                         AND newer.component = candidate.component
                         AND newer.pipeline_version = candidate.pipeline_version
                         AND newer.requested_generation = candidate.requested_generation
                         AND newer.target_node_id = candidate.target_node_id
                         AND (newer.updated_at_ms > candidate.updated_at_ms
                           OR (newer.updated_at_ms = candidate.updated_at_ms
                             AND newer.request_id > candidate.request_id))))
              ORDER BY candidate.updated_at_ms, candidate.request_id
              LIMIT MAX((SELECT COUNT(*) FROM analysis_requests
                          WHERE state IN ('ready', 'failed', 'cancelled')) - 8192, 0)"
            .to_owned(),
    )
    .await;
    assert!(
        details.iter().any(|detail| {
            detail.contains("analysis_requests_terminal_identity") && detail.contains("file_id=?")
        }),
        "terminal retention must probe newer generations by identity: {details:?}"
    );
}

fn job(file_id: i64, source_mtime: i64, created_at_ms: i64) -> NewClusterFragmentIndexJob {
    let source_sha256 = "a".repeat(64);
    let pipeline_sha256 = "b".repeat(64);
    NewClusterFragmentIndexJob {
        cache_key: cluster_fragment_index_key(
            file_id,
            100,
            source_mtime,
            &source_sha256,
            &pipeline_sha256,
        )
        .expect("valid key"),
        file_id,
        source_size: 100,
        source_mtime,
        source_sha256,
        pipeline_sha256,
        priority: "normal".to_owned(),
        trigger: "background".to_owned(),
        target_node_id: String::new(),
        not_before_ms: created_at_ms,
        created_at_ms,
    }
}

fn request(id: &str, force_rebuild: bool, created_at_ms: i64) -> NewAnalysisRequest {
    NewAnalysisRequest {
        request_id: id.to_owned(),
        file_id: 1,
        source_size: 100,
        source_mtime: 10,
        component: "fragment_index".to_owned(),
        pipeline_version: "test-pipeline".to_owned(),
        video_identity: String::new(),
        requested_generation: if force_rebuild {
            format!("generation-{id}")
        } else {
            "test-generation".to_owned()
        },
        priority: if force_rebuild { "forced" } else { "normal" }.to_owned(),
        trigger: "admin".to_owned(),
        force_rebuild,
        target_node_id: "node-a".to_owned(),
        not_before_ms: created_at_ms,
        created_at_ms,
    }
}

#[tokio::test]
async fn playback_viewers_share_exact_analysis_and_retire_urgency_independently() {
    use crate::store::background_jobs::{AnalysisViewerInterest, BackgroundJobStore, CancelWaiter};
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let analysis = store
        .enqueue_analysis_request(&request("viewer-analysis", false, 1_000))
        .await
        .expect("analysis");
    let viewer = |user_id: i64| AnalysisViewerInterest {
        analysis_request_id: analysis.request_id.clone(),
        requested_generation: analysis.requested_generation.clone(),
        pipeline_version: analysis.pipeline_version.clone(),
        video_identity: analysis.video_identity.clone(),
        target_node_id: analysis.target_node_id.clone(),
        principal: crate::playback_principal::PlaybackPrincipal::LocalUser { user_id },
        playback_id: "playback-one".to_owned(),
        now_ms: 1_001,
    };
    assert!(store
        .join_analysis_viewer(viewer(1))
        .await
        .expect("first viewer"));
    assert!(store
        .join_analysis_viewer(viewer(2))
        .await
        .expect("second viewer"));
    let joined = store
        .enqueue_analysis_request(&request("same-generation", false, 1_002))
        .await
        .expect("join analysis");
    assert_eq!(joined.request_id, analysis.request_id);
    let claimed = store
        .claim_analysis_request("node-a", 1_003, 31_003)
        .await
        .expect("claim")
        .expect("viewer analysis first");
    assert_eq!(claimed.request_id, analysis.request_id);
    assert_eq!(claimed.priority, "foreground");
    assert_eq!(claimed.attempts, 1);

    store
        .cancel_waiter(CancelWaiter {
            scope: "playback-analysis".to_owned(),
            request_id: viewer(1).consumer_id(),
            now_ms: 1_004,
        })
        .await
        .expect("cancel first");
    let first = store.analysis_requests(10).await.expect("requests");
    assert_eq!(first[0].priority, "foreground");
    store
        .cancel_waiter(CancelWaiter {
            scope: "playback-analysis".to_owned(),
            request_id: viewer(2).consumer_id(),
            now_ms: 1_005,
        })
        .await
        .expect("cancel second");
    let retired = store.analysis_requests(10).await.expect("requests");
    assert_eq!(retired[0].priority, "normal");
    assert_eq!(retired[0].attempts, 1);
    assert_eq!(retired[0].fence, claimed.fence);
}

#[tokio::test]
async fn source_io_keeps_second_slot_for_playback_analysis() {
    use crate::store::background_jobs::{AnalysisViewerInterest, BackgroundJobStore};
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO files (id, item_id, path, size, mtime)
                VALUES (3, 2, '/three.mkv', 100, 30)",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("third file");
    let first = store
        .enqueue_analysis_request(&request("maintenance", false, 1_000))
        .await
        .expect("maintenance");
    let mut second_input = request("viewer", false, 1_001);
    second_input.file_id = 2;
    second_input.source_mtime = 20;
    let second = store
        .enqueue_analysis_request(&second_input)
        .await
        .expect("viewer request");
    let mut third_input = request("third", false, 1_002);
    third_input.file_id = 3;
    third_input.source_mtime = 30;
    let third = store
        .enqueue_analysis_request(&third_input)
        .await
        .expect("third request");
    let occupied = store
        .claim_analysis_request("node-a", 1_003, 31_003)
        .await
        .expect("first claim")
        .expect("maintenance claim");
    assert_eq!(occupied.request_id, first.request_id);
    assert!(store
        .claim_analysis_request("node-a", 1_004, 31_004)
        .await
        .expect("maintenance cannot borrow second slot")
        .is_none());
    let interest = |request: &AnalysisRequest, user_id| AnalysisViewerInterest {
        analysis_request_id: request.request_id.clone(),
        requested_generation: request.requested_generation.clone(),
        pipeline_version: request.pipeline_version.clone(),
        video_identity: request.video_identity.clone(),
        target_node_id: request.target_node_id.clone(),
        principal: crate::playback_principal::PlaybackPrincipal::LocalUser { user_id },
        playback_id: format!("viewer-{user_id}"),
        now_ms: 1_005,
    };
    assert!(store
        .join_analysis_viewer(interest(&second, 1))
        .await
        .expect("join second"));
    let admitted = store
        .claim_analysis_request("node-a", 1_006, 31_006)
        .await
        .expect("viewer claim")
        .expect("viewer gets second slot");
    assert_eq!(admitted.request_id, second.request_id);
    assert!(store
        .join_analysis_viewer(interest(&third, 2))
        .await
        .expect("join third"));
    assert!(store
        .claim_analysis_request("node-a", 1_007, 31_007)
        .await
        .expect("two-slot ceiling")
        .is_none());
}

#[tokio::test]
async fn analysis_history_is_canonical_paginated_and_classified() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let cache_key = cluster_fragment_index_key(1, 100, 10, &"a".repeat(64), &"b".repeat(64))
        .expect("valid cache key");
    let seeded_key = cache_key.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, target_node_id, state, fence, attempts, not_before_ms,
                       last_error_code, created_at_ms, updated_at_ms)
                     VALUES (?1, 1, 100, 10, ?2, ?3, 'node-a', 'failed', 1, 1, 0,
                             'source_unavailable', 10, 20)",
                params![seeded_key, "a".repeat(64), "b".repeat(64)],
            )?;
            conn.execute(
                "INSERT INTO analysis_requests
                      (request_id, file_id, source_size, source_mtime, component,
                       force_rebuild, target_node_id, state, fence, attempts,
                       not_before_ms, result_cache_key, created_at_ms, updated_at_ms)
                     VALUES ('old-generation', 1, 100, 10, 'fragment_index', 0,
                             'node-a', 'ready', 1, 1, 0, ?1, 1, 10),
                            ('new-generation', 1, 100, 10, 'fragment_index', 1,
                             'node-a', 'submitted', 1, 1, 0, ?1, 2, 20),
                            ('unsupported', 1, 100, 10, 'fragment_index', 0,
                             'node-a', 'failed', 1, 1, 0, NULL, 3, 30),
                            ('deleted', 1, 100, 10, 'fragment_index', 0,
                             'node-a', 'cancelled', 1, 1, 0, NULL, 4, 40),
                            ('superseded', 1, 100, 10, 'fragment_index', 0,
                             'node-a', 'cancelled', 1, 1, 0, NULL, 5, 50),
                            ('missing-ready', 999, 100, 10, 'fragment_index', 0,
                             'node-a', 'ready', 1, 1, 0, NULL, 6, 60)",
                params![seeded_key],
            )?;
            conn.execute(
                "UPDATE analysis_requests SET last_error_code = 'unsupported'
                      WHERE request_id = 'unsupported'",
                [],
            )?;
            conn.execute(
                "UPDATE analysis_requests SET last_error_code = 'source_deleted'
                      WHERE request_id = 'deleted'",
                [],
            )?;
            conn.execute(
                "UPDATE analysis_requests SET last_error_code = 'source_superseded'
                      WHERE request_id = 'superseded'",
                [],
            )?;
            for index in 0..25_i64 {
                conn.execute(
                    "INSERT INTO analysis_requests
                          (request_id, file_id, source_size, source_mtime, component,
                           force_rebuild, target_node_id, state, fence, attempts,
                           not_before_ms, created_at_ms, updated_at_ms)
                         VALUES (?1, 1, 100, 10, 'fragment_index', 0, 'node-a',
                                 'ready', 1, 1, 0, ?2, ?2)",
                    params![format!("ready-{index:02}"), 100 + index],
                )?;
            }
            Ok(())
        })
        .await
        .expect("seed analysis history");

    let mut cursor = None;
    let mut rows = Vec::new();
    loop {
        let page = store
            .analysis_history(&AnalysisHistoryQuery {
                limit: 10,
                cursor,
                filter: AnalysisHistoryFilter::All,
                search: String::new(),
                states: Vec::new(),
                now_ms: 0,
            })
            .await
            .expect("history page");
        assert_eq!(page.filtered_total, 31);
        rows.extend(page.rows);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(rows.len(), 31);
    let keys = rows.iter().map(|row| &row.row_key).collect::<HashSet<_>>();
    assert_eq!(keys.len(), rows.len(), "keyset pages must not overlap");
    let old = rows
        .iter()
        .find(|row| row.request_id == "old-generation")
        .expect("old generation");
    let new = rows
        .iter()
        .find(|row| row.request_id == "new-generation")
        .expect("new generation");
    assert!(old.job_id.is_empty(), "mutable job leaked into old history");
    assert_eq!(old.state, "ready");
    assert_eq!(new.job_id, cache_key);
    assert_eq!(new.state, "failed");
    assert_eq!(new.action, "retry");
    assert_eq!(old.action, "rebuild");
    assert_eq!(
        rows.iter()
            .find(|row| row.request_id == "superseded")
            .expect("superseded request")
            .action,
        "analyze_current"
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.request_id == "missing-ready")
            .expect("deleted ready file")
            .action,
        "none"
    );

    let expected = store
        .analysis_history(&AnalysisHistoryQuery {
            limit: 10,
            cursor: None,
            filter: AnalysisHistoryFilter::Expected,
            search: String::new(),
            states: Vec::new(),
            now_ms: 0,
        })
        .await
        .expect("expected outcomes");
    assert_eq!(expected.filtered_total, 3);
    assert!(expected
        .rows
        .iter()
        .all(|row| matches!(row.disposition.as_str(), "expected" | "unsupported")));

    let stale = store
        .analysis_history(&AnalysisHistoryQuery {
            limit: 10,
            cursor: None,
            filter: AnalysisHistoryFilter::All,
            search: String::new(),
            states: vec!["stale".to_owned()],
            now_ms: 1_000,
        })
        .await
        .expect("stale durable state");
    assert_eq!(stale.filtered_total, 1);
    assert_eq!(stale.rows[0].request_id, "superseded");

    let summary = store.analysis_status_summary().await.expect("summary");
    assert_eq!(summary.total, 31);
    assert_eq!(summary.attention, 1);
    assert_eq!(summary.expected, 3);
    assert_eq!(summary.ready, 27);
    assert_eq!(summary.latest_error_code, "source_unavailable");

    let past_end = store
        .analysis_history(&AnalysisHistoryQuery {
            limit: 10,
            cursor: Some(AnalysisHistoryCursor {
                sort_rank: 6,
                updated_at_ms: 0,
                row_key: "past-the-retained-tail".to_owned(),
            }),
            filter: AnalysisHistoryFilter::All,
            search: String::new(),
            states: Vec::new(),
            now_ms: 0,
        })
        .await
        .expect("valid cursor past retained tail");
    assert!(past_end.rows.is_empty());
    assert_eq!(past_end.filtered_total, 31);
}

#[tokio::test]
async fn operator_requests_are_durable_coalesced_and_lease_fenced() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let first = store
        .enqueue_analysis_request(&request("request-1", false, 10))
        .await
        .expect("enqueue request");
    assert_eq!(first.request_id, "request-1");
    let joined = store
        .enqueue_analysis_request(&request("request-2", false, 11))
        .await
        .expect("join active request");
    assert_eq!(joined.request_id, first.request_id);

    assert!(store
        .claim_analysis_request("node-b", 20, 1_020)
        .await
        .expect("wrong-node claim")
        .is_none());
    let claimed = store
        .claim_analysis_request("node-a", 20, 1_020)
        .await
        .expect("claim")
        .expect("request claim");
    assert_eq!(claimed.state, "running");
    assert_eq!(claimed.fence, 1);
    assert!(!store
        .renew_analysis_request(&claimed.request_id, "node-a", 0, 21, 1_021)
        .await
        .expect("stale renewal"));
    assert!(store
        .renew_analysis_request(&claimed.request_id, "node-a", 1, 21, 1_021)
        .await
        .expect("current renewal"));
    assert!(store
        .fail_analysis_request(&claimed.request_id, "node-a", 1, "source_unavailable", 22)
        .await
        .expect("fail current claim"));
    let rows = store.analysis_requests(10).await.expect("list requests");
    assert_eq!(rows[0].state, "failed");
    assert_eq!(rows[0].last_error_code, "source_unavailable");

    let successor = store
        .enqueue_analysis_request(&request("request-3", true, 23))
        .await
        .expect("enqueue terminal successor");
    assert_eq!(successor.request_id, "request-3");
    assert!(successor.force_rebuild);
    let labels = store.analysis_file_labels(10).await.expect("file labels");
    assert_eq!(labels[0].title, "one");
}

#[tokio::test]
async fn submitted_request_follows_its_fenced_index_job_to_ready() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .enqueue_analysis_request(&request("request-1", false, 10))
        .await
        .expect("enqueue request");
    let claimed = store
        .claim_analysis_request("node-a", 10, 1_010)
        .await
        .expect("claim")
        .expect("request claim");
    let mut job = job(1, 10, 11);
    job.target_node_id = claimed.target_node_id.clone();
    job.priority = claimed.priority.clone();
    job.trigger = claimed.trigger.clone();
    assert!(store
        .submit_fragment_index_analysis(&claimed, &job, 12)
        .await
        .expect("submit request and job atomically"));
    let cache_key = job.cache_key.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
                            pipeline_sha256, ?2, 10, 'node-a', 13
                       FROM cluster_fragment_index_jobs WHERE cache_key = ?1",
                params![cache_key, "c".repeat(64)],
            )?;
            conn.execute(
                "UPDATE cluster_fragment_index_jobs SET state = 'ready', updated_at_ms = 13
                      WHERE cache_key = ?1",
                params![cache_key],
            )?;
            conn.execute(
                "INSERT INTO cluster_fragment_index_locations
                      (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                     VALUES (?1, 'node-a', 10, 13, 13)",
                params![cache_key],
            )?;
            conn.execute(
                "INSERT INTO cluster_fragment_index_heads
                      (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                     VALUES (?1, ?1, 'request-1', 13)",
                params![cache_key],
            )?;
            Ok(())
        })
        .await
        .expect("publish fixture");
    assert_eq!(
        store
            .settle_analysis_requests(14)
            .await
            .expect("settle requests"),
        1
    );
    let settled = store.analysis_requests(10).await.expect("list requests");
    assert_eq!(settled[0].state, "ready");
    assert_eq!(settled[0].result_cache_key, job.cache_key);
}

#[tokio::test]
async fn forced_successor_keeps_the_published_artifact_serving() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let job = job(1, 10, 10);
    let seeded = job.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 2, 2, 0, 10, 10)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                ],
            )?;
            conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, target_node_id, state, fence, attempts,
                       not_before_ms, last_error_code, index_diagnostic_json,
                       created_at_ms, updated_at_ms)
                     VALUES ('malformed-diagnostic-decoy', ?1, ?2, ?3, ?4, ?5, 'node-c',
                             'failed', 4, 5, 9223372036854775807, 'attempt_limit',
                             '{not-json', 1, 50)",
                params![
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                ],
            )?;
            conn.execute(
                "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                    "c".repeat(64),
                ],
            )?;
            Ok(())
        })
        .await
        .expect("seed ready generation");

    let mut successor = job.clone();
    successor.created_at_ms = 20;
    successor.not_before_ms = 20;
    store
        .enqueue_analysis_request(&request("forced-request", true, 20))
        .await
        .expect("enqueue forced request");
    let claimed = store
        .claim_analysis_request("node-a", 20, 1_020)
        .await
        .expect("claim forced request")
        .expect("forced request claim");
    successor.cache_key = cluster_fragment_index_generation_key(
        successor.file_id,
        successor.source_size,
        successor.source_mtime,
        &successor.source_sha256,
        &successor.pipeline_sha256,
        &claimed.requested_generation,
    )
    .expect("forced generation key");
    successor.target_node_id = claimed.target_node_id.clone();
    successor.priority = claimed.priority.clone();
    successor.trigger = claimed.trigger.clone();
    assert!(store
        .submit_fragment_index_analysis(&claimed, &successor, 21)
        .await
        .expect("force successor"));
    assert!(store
        .cluster_fragment_index_artifact(&job.cache_key)
        .await
        .expect("read serving artifact")
        .is_some());
    let reopened = store
        .cluster_fragment_index_job(&successor.cache_key, &successor.target_node_id)
        .await
        .expect("read reopened job")
        .expect("job");
    assert_eq!(reopened.state, "queued");
    assert_eq!(reopened.attempts, 0);
}

#[tokio::test]
async fn stale_request_owner_cannot_create_or_reopen_a_worker_job() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .enqueue_analysis_request(&request("request-1", true, 10))
        .await
        .expect("enqueue request");
    let stale = store
        .claim_analysis_request("node-a", 10, 20)
        .await
        .expect("first claim")
        .expect("first owner");
    let current = store
        .claim_analysis_request("node-a", 20, 1_020)
        .await
        .expect("replacement claim")
        .map(|_| panic!("expired lease must enter retry wait"));
    assert!(current.is_none());
    let retry_at = 20 + analysis_backoff_ms("request-1", 1, 5_000, 300_000);
    let current = store
        .claim_analysis_request("node-a", retry_at, retry_at + 1_000)
        .await
        .expect("replacement claim after retry wait")
        .expect("replacement owner");
    assert!(current.fence > stale.fence);

    let mut job = job(1, 10, 21);
    job.target_node_id = stale.target_node_id.clone();
    job.priority = stale.priority.clone();
    job.trigger = stale.trigger.clone();
    job.cache_key = cluster_fragment_index_generation_key(
        job.file_id,
        job.source_size,
        job.source_mtime,
        &job.source_sha256,
        &job.pipeline_sha256,
        &stale.requested_generation,
    )
    .expect("forced stale generation key");
    assert!(!store
        .submit_fragment_index_analysis(&stale, &job, 21)
        .await
        .expect("stale handoff is a fenced no-op"));
    assert!(store
        .cluster_fragment_index_job(&job.cache_key, &job.target_node_id)
        .await
        .expect("read worker job")
        .is_none());
    let requests = store.analysis_requests(10).await.expect("list requests");
    assert_eq!(requests[0].state, "running");
    assert_eq!(requests[0].fence, current.fence);
}

#[tokio::test]
async fn foreground_retry_yields_without_spending_an_attempt() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .enqueue_analysis_request(&request("request-1", false, 10))
        .await
        .expect("enqueue request");
    let claimed = store
        .claim_analysis_request("node-a", 10, 1_010)
        .await
        .expect("claim")
        .expect("request claim");
    assert_eq!(claimed.attempts, 1);
    assert!(store
        .retry_analysis_request(&claimed, "foreground_preempted", 11, 21, false,)
        .await
        .expect("yield request"));
    let retried = store.analysis_requests(10).await.expect("list requests");
    assert_eq!(retried[0].state, "queued");
    assert_eq!(retried[0].attempts, 0);
    assert_eq!(retried[0].not_before_ms, 21);
    assert_eq!(retried[0].last_error_code, "foreground_preempted");
    assert!(store
        .claim_analysis_request("node-a", 20, 1_020)
        .await
        .expect("early claim")
        .is_none());
}

#[tokio::test]
async fn source_replacement_cancels_old_generation_and_admits_new_generation() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .enqueue_analysis_request(&request("old-source", false, 10))
        .await
        .expect("enqueue old source");
    let old_claim = store
        .claim_analysis_request("node-a", 10, 1_010)
        .await
        .expect("claim old source")
        .expect("old source claim");
    assert!(store
        .record_analysis_request_phase(&old_claim, "hashing", None, 11)
        .await
        .expect("record old source phase"));
    store
        .with_conn(|conn| {
            conn.execute("UPDATE files SET size = 200, mtime = 30 WHERE id = 1", [])?;
            Ok(())
        })
        .await
        .expect("replace source generation");
    let mut replacement = request("new-source", false, 30);
    replacement.source_size = 200;
    replacement.source_mtime = 30;
    let admitted = store
        .enqueue_analysis_request(&replacement)
        .await
        .expect("enqueue new source");
    assert_eq!(admitted.request_id, "new-source");
    let requests = store.analysis_requests(10).await.expect("list requests");
    assert_eq!(requests.len(), 2);
    let old = requests
        .iter()
        .find(|row| row.request_id == "old-source")
        .expect("old generation");
    assert_eq!(old.state, "cancelled");
    assert_eq!(old.last_error_code, "source_superseded");
    let attempts = store
        .analysis_attempts("old-source", 10)
        .await
        .expect("old source attempts");
    assert_eq!(attempts[0].phase, "stale");
    assert_eq!(attempts[0].terminal_code, "source_superseded");
}

#[tokio::test]
async fn source_deletion_terminalizes_the_live_attempt_before_cascade() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .enqueue_analysis_request(&request("deleted-source", false, 10))
        .await
        .expect("enqueue source");
    let claim = store
        .claim_analysis_request("node-a", 10, 1_010)
        .await
        .expect("claim source")
        .expect("source claim");
    assert!(store
        .record_analysis_request_phase(&claim, "hashing", None, 11)
        .await
        .expect("record source phase"));
    store
        .with_conn(|conn| {
            conn.execute("DELETE FROM files WHERE id = 1", [])?;
            Ok(())
        })
        .await
        .expect("delete source");
    let request = store
        .analysis_request("deleted-source")
        .await
        .expect("read request")
        .expect("retained request");
    assert_eq!(request.state, "cancelled");
    assert_eq!(request.last_error_code, "source_deleted");
    let attempts = store
        .analysis_attempts("deleted-source", 10)
        .await
        .expect("deleted source attempts");
    assert_eq!(attempts[0].phase, "canceled");
    assert_eq!(attempts[0].terminal_code, "source_deleted");
}

#[tokio::test]
async fn scanner_generation_change_does_not_reuse_old_source_identity() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let ready = job(1, 10, 10);
    let seeded = ready.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 1, 1, 0, 10, 10)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                ],
            )?;
            conn.execute(
                "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                    "c".repeat(64),
                ],
            )?;
            conn.execute("UPDATE files SET mtime = 30 WHERE id = 1", [])?;
            Ok(())
        })
        .await
        .expect("seed ready artifact and replace scanner generation");

    let mut replacement = request("replacement", false, 30);
    replacement.source_mtime = 30;
    store
        .enqueue_analysis_request(&replacement)
        .await
        .expect("enqueue replacement generation");
    let claimed = store
        .claim_analysis_request("node-a", 30, 1_030)
        .await
        .expect("claim replacement")
        .expect("replacement claim");
    let mut replacement_job = ready.clone();
    replacement_job.source_mtime = 30;
    replacement_job.cache_key = cluster_fragment_index_key(
        replacement_job.file_id,
        replacement_job.source_size,
        replacement_job.source_mtime,
        &replacement_job.source_sha256,
        &replacement_job.pipeline_sha256,
    )
    .expect("replacement source identity");
    replacement_job.target_node_id = claimed.target_node_id.clone();
    replacement_job.priority = claimed.priority.clone();
    replacement_job.trigger = claimed.trigger.clone();
    replacement_job.created_at_ms = 31;
    replacement_job.not_before_ms = 31;
    assert!(store
        .submit_fragment_index_analysis(&claimed, &replacement_job, 31)
        .await
        .expect("submit replacement source identity"));
    assert_eq!(
        store
            .settle_analysis_requests(32)
            .await
            .expect("settle replacement"),
        0
    );
    let requests = store.analysis_requests(10).await.expect("list requests");
    assert_eq!(requests[0].state, "submitted");
    assert!(store
        .cluster_fragment_index_artifact(&ready.cache_key)
        .await
        .expect("read artifact")
        .is_some());
}

#[tokio::test]
async fn byte_identical_files_queue_independent_structural_work() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let old = job(1, 10, 1);
    let seeded = old.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'cancelled', 1, 3, 0, 1, 1)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                ],
            )?;
            conn.execute(
                "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 1)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                    "c".repeat(64),
                ],
            )?;
            Ok(())
        })
        .await
        .expect("seed cancelled generation");
    let mut replacement = request("rebind", false, 30_000_001);
    replacement.file_id = 2;
    replacement.source_mtime = 20;
    store
        .enqueue_analysis_request(&replacement)
        .await
        .expect("enqueue surviving file");
    let claimed = store
        .claim_analysis_request("node-a", 30_000_001, 30_001_001)
        .await
        .expect("claim surviving file")
        .expect("surviving claim");
    let mut rebound = old.clone();
    rebound.file_id = 2;
    rebound.source_mtime = 20;
    rebound.cache_key = cluster_fragment_index_key(
        rebound.file_id,
        rebound.source_size,
        rebound.source_mtime,
        &rebound.source_sha256,
        &rebound.pipeline_sha256,
    )
    .expect("independent file identity");
    rebound.target_node_id = claimed.target_node_id.clone();
    rebound.priority = claimed.priority.clone();
    rebound.trigger = claimed.trigger.clone();
    rebound.created_at_ms = 30_000_002;
    rebound.not_before_ms = 30_000_002;
    assert!(store
        .submit_fragment_index_analysis(&claimed, &rebound, 30_000_002)
        .await
        .expect("rebind cancelled content"));
    let job = store
        .cluster_fragment_index_job(&rebound.cache_key, &rebound.target_node_id)
        .await
        .expect("read rebound job")
        .expect("rebound job");
    assert_eq!(job.state, "queued");
    assert_eq!(job.file_id, 2);
    assert_eq!(job.created_at_ms, 30_000_002);
    assert!(store
        .cluster_fragment_index_artifact(&old.cache_key)
        .await
        .expect("read retained artifact")
        .is_some());
}

#[tokio::test]
async fn charged_retries_exhaust_to_a_terminal_attempt_limit() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .put_setting(crate::store::keys::ANALYSIS_MAX_ATTEMPTS, "2")
        .await
        .expect("configure attempt budget");
    store
        .enqueue_analysis_request(&request("attempts", false, 10))
        .await
        .expect("enqueue request");
    let mut now = 10;
    for expected_attempt in 1..=2 {
        let claimed = store
            .claim_analysis_request("node-a", now, now + 1_000)
            .await
            .expect("claim retry")
            .expect("retry claim");
        assert_eq!(claimed.attempts, expected_attempt);
        assert!(store
            .retry_analysis_request(
                &claimed,
                "source_attestation_failed",
                now + 1,
                now + 2,
                true,
            )
            .await
            .expect("queue charged retry"));
        // A failed reader can outlive its request transition. Its shared
        // source reservation remains until the original 1,000 ms lease
        // expires, so the retry must use a later physical-admission clock.
        now += 1_001;
    }
    assert!(store
        .claim_analysis_request("node-a", now, now + 1_000)
        .await
        .expect("settle attempt limit")
        .is_none());
    let requests = store.analysis_requests(10).await.expect("list requests");
    assert_eq!(requests[0].state, "failed");
    assert_eq!(requests[0].attempts, 2);
    assert_eq!(requests[0].last_error_code, "attempt_limit");
}

#[tokio::test]
async fn one_file_holds_one_request_per_copy_video_identity() {
    // The shape M5.2 exists for. A file has one to three copy-video
    // identities; before this, the first request became the dedup
    // tombstone for all of them, so a Dolby Vision title got its stripped
    // identity and never its converting one.
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let mut stripped = request("identity-stripped", false, 10);
    stripped.video_identity = "identity-a".to_owned();
    stripped.requested_generation = "generation-a".to_owned();
    let mut converting = request("identity-converting", false, 11);
    converting.video_identity = "identity-b".to_owned();
    converting.requested_generation = "generation-b".to_owned();

    assert_eq!(
        store
            .enqueue_analysis_request(&stripped)
            .await
            .expect("enqueue the stripped identity")
            .request_id,
        "identity-stripped"
    );
    assert_eq!(
        store
            .enqueue_analysis_request(&converting)
            .await
            .expect("enqueue the converting identity")
            .request_id,
        "identity-converting",
        "a second identity of the same file must not dedup against the first"
    );
    let live = store.analysis_requests(10).await.expect("requests");
    assert_eq!(live.len(), 2);
    let mut identities = live
        .iter()
        .map(|request| request.video_identity.as_str())
        .collect::<Vec<_>>();
    identities.sort_unstable();
    assert_eq!(identities, vec!["identity-a", "identity-b"]);

    // The identity survives the round trip, which is what lets the
    // resolver build the one that was asked for rather than the one that
    // happens to be missing first.
    let read = store
        .analysis_request("identity-converting")
        .await
        .expect("read back")
        .expect("the converting request");
    assert_eq!(read.video_identity, "identity-b");
}

#[tokio::test]
async fn forcing_one_identity_leaves_its_siblings_alone() {
    // The reason the identity had to be a column. The forced-successor
    // cancellation scopes on file, component and pipeline version; without
    // the identity it cancels every sibling, and `cancelled` is terminal,
    // so their generations are refused for good and the file never gets
    // its other indexes.
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    for (id, identity, generation) in [
        ("bg-a", "identity-a", "generation-a"),
        ("bg-b", "identity-b", "generation-b"),
    ] {
        let mut background = request(id, false, 10);
        background.video_identity = identity.to_owned();
        background.requested_generation = generation.to_owned();
        store
            .enqueue_analysis_request(&background)
            .await
            .unwrap_or_else(|error| panic!("enqueue {id}: {error}"));
    }

    let mut forced = request("forced-a", true, 20);
    forced.video_identity = "identity-a".to_owned();
    store
        .enqueue_analysis_request(&forced)
        .await
        .expect("force the first identity");

    let by_id = store
        .analysis_requests(10)
        .await
        .expect("requests")
        .into_iter()
        .map(|request| (request.request_id.clone(), request))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        by_id["bg-a"].state, "cancelled",
        "the forced identity's own background request is superseded"
    );
    assert_eq!(
        by_id["bg-b"].state, "queued",
        "a sibling identity must survive a force aimed at another one"
    );
    assert!(
        by_id["bg-b"].last_error_code.is_empty(),
        "{:?}",
        by_id["bg-b"]
    );
}

#[tokio::test]
async fn an_unidentified_force_still_supersedes_everything_it_used_to() {
    // Every row written before the column, and every `skip_markers` row,
    // carries an empty identity and means "whichever is next". A force
    // with no identity must keep superseding all of them, or the upgrade
    // changes the meaning of requests already in flight.
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .enqueue_analysis_request(&request("legacy", false, 10))
        .await
        .expect("enqueue a pre-column background request");
    store
        .enqueue_analysis_request(&request("forced-legacy", true, 20))
        .await
        .expect("force with no identity");
    let by_id = store
        .analysis_requests(10)
        .await
        .expect("requests")
        .into_iter()
        .map(|request| (request.request_id.clone(), request))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(by_id["legacy"].state, "cancelled");
    assert_eq!(by_id["legacy"].video_identity, "");
    assert_eq!(by_id["forced-legacy"].state, "queued");
}

/// The other half of the same clause: a force with no identity displaces an
/// ordinary blank request, and does not displace another blank force.
///
/// Without the second half, two clicks on Force rebuild are two admitted
/// requests for the same generation — neither cancels the other, both spend
/// a slot of the active budget, and both run a whole-file index build.
#[tokio::test]
async fn two_unidentified_forces_do_not_both_win() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .enqueue_analysis_request(&request("first-force", true, 10))
        .await
        .expect("the first force is admitted");
    let second = store
        .enqueue_analysis_request(&request("second-force", true, 20))
        .await
        .expect("a second click is answered, not refused");
    assert_eq!(
        second.request_id, "first-force",
        "the second click is handed the force already in flight",
    );
    let states = store
        .analysis_requests(10)
        .await
        .expect("requests")
        .into_iter()
        .map(|request| (request.request_id.clone(), request.state.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(states["first-force"], "queued");
    assert!(
        !states.contains_key("second-force"),
        "a second row would spend the budget twice and cancel nothing: {states:?}",
    );
}

#[tokio::test]
async fn a_terminal_generation_blocks_its_own_reuse_and_only_its_own() {
    // The fleet shape this pins: 1,939 rows sat `attempt_limit` after a
    // queue fault, and nothing could re-request those files, because
    // `enqueue_analysis_request` refuses a generation that already exists
    // in *any* state — terminal included. Moving the generation
    // fingerprint is the only way to reopen them without editing history,
    // and this is the assertion that says so.
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .put_setting(crate::store::keys::ANALYSIS_MAX_ATTEMPTS, "1")
        .await
        .expect("configure attempt budget");
    store
        .enqueue_analysis_request(&request("stranded", false, 10))
        .await
        .expect("enqueue original");
    let claimed = store
        .claim_analysis_request("node-a", 10, 1_010)
        .await
        .expect("claim")
        .expect("a queued request");
    assert!(store
        .retry_analysis_request(&claimed, "source_attestation_failed", 11, 12, true)
        .await
        .expect("charge the only attempt"));
    assert!(store
        .claim_analysis_request("node-a", 20, 1_020)
        .await
        .expect("settle attempt limit")
        .is_none());
    assert_eq!(
        store.analysis_requests(10).await.expect("requests")[0].last_error_code,
        "attempt_limit"
    );

    // Same generation: refused, however long ago it died. The refusal is
    // returning the tombstone itself rather than a new row.
    let mut same = request("stranded-again", false, 30);
    assert_eq!(same.requested_generation, "test-generation");
    let refused = store
        .enqueue_analysis_request(&same)
        .await
        .expect("enqueue under the dead generation");
    assert_eq!(
        refused.request_id, "stranded",
        "a terminal row blocks its own generation, which is what stranded the fleet"
    );
    assert_eq!(refused.state, "failed");

    // A new regime token is a new generation, and the file is requestable
    // again without a single row being deleted or edited.
    same.request_id = "stranded-reopened".to_owned();
    same.requested_generation = "test-generation/sampled-v1".to_owned();
    let reopened = store
        .enqueue_analysis_request(&same)
        .await
        .expect("enqueue under the new generation");
    assert_eq!(reopened.request_id, "stranded-reopened");
    assert_eq!(reopened.state, "queued");
    assert_eq!(
        store.analysis_requests(10).await.expect("requests").len(),
        2,
        "the tombstone stays as history beside its successor"
    );
}

#[tokio::test]
async fn terminal_analysis_history_is_age_and_generation_bounded() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .with_conn(|conn| {
            for generation in 0_i64..25 {
                conn.execute(
                    "INSERT INTO analysis_requests
                          (request_id, file_id, source_size, source_mtime, component,
                           force_rebuild, target_node_id, state, fence, attempts,
                           not_before_ms, created_at_ms, updated_at_ms)
                         VALUES (?1, 1, 100, 10, 'fragment_index', 0, 'node-a',
                                 'ready', 0, 1, 0, ?2, ?2)",
                    params![format!("history-{generation:02}"), generation],
                )?;
            }
            Ok(())
        })
        .await
        .expect("seed history");
    assert_eq!(
        store
            .prune_analysis_requests(-1, 100)
            .await
            .expect("prune generation cap"),
        5
    );
    assert_eq!(
        store.analysis_requests(100).await.expect("history").len(),
        20
    );
    assert_eq!(
        store
            .prune_analysis_requests(100, 100)
            .await
            .expect("prune by age"),
        19
    );
    assert_eq!(
        store.analysis_requests(100).await.expect("history").len(),
        1
    );
}

#[tokio::test]
async fn terminal_analysis_history_has_a_hard_global_cap() {
    let timing = std::env::var("PLURX_TEST_SETUP_TIMING").as_deref() == Ok("1");
    fn report_phase(phase: &str, started: Option<std::time::Instant>, operations: u64) {
        if let Some(started) = started {
            eprintln!(
                "PLURX_TEST_SETUP_TIMING {}",
                serde_json::json!({
                    "test": "store::sqlite::fragment_index_cluster::tests::terminal_analysis_history_has_a_hard_global_cap",
                    "phase": phase,
                    "elapsed_seconds": started.elapsed().as_secs_f64(),
                    "operations": operations,
                })
            );
        }
    }
    let started = timing.then(std::time::Instant::now);
    let store = SqliteStore::open_in_memory().expect("store");
    report_phase("open", started, 1);
    let started = timing.then(std::time::Instant::now);
    seed_files(&store).await;
    report_phase("seed_files", started, 1);
    store
        .with_conn(move |conn| {
            let started = timing.then(std::time::Instant::now);
            let transaction = conn.unchecked_transaction()?;
            report_phase("transaction_begin", started, 1);
            let started = timing.then(std::time::Instant::now);
            let mut inserted = 0;
            for generation in 0_i64..=8_192 {
                transaction.execute(
                    "INSERT INTO analysis_requests
                          (request_id, file_id, source_size, source_mtime, component,
                           force_rebuild, target_node_id, state, fence, attempts,
                           not_before_ms, created_at_ms, updated_at_ms)
                         VALUES (?1, 1, 100, ?2, 'fragment_index', 0, 'node-a',
                                 'failed', 0, 1, 0, ?2, ?2)",
                    params![format!("terminal-{generation:05}"), generation],
                )?;
                inserted += 1;
            }
            report_phase("terminal_insert_loop", started, inserted);
            let started = timing.then(std::time::Instant::now);
            transaction.execute(
                "INSERT INTO analysis_requests
                      (request_id, file_id, source_size, source_mtime, component,
                       force_rebuild, target_node_id, state, fence, attempts,
                       not_before_ms, created_at_ms, updated_at_ms)
                     VALUES ('cap-trigger', 2, 100, 20, 'fragment_index', 0,
                             'node-a', 'queued', 0, 0, 9000, 9000, 9000)",
                [],
            )?;
            report_phase("queued_insert", started, 1);
            let started = timing.then(std::time::Instant::now);
            transaction.execute(
                "UPDATE analysis_requests SET state = 'failed'
                      WHERE request_id = 'cap-trigger'",
                [],
            )?;
            report_phase("cap_update", started, 1);
            let started = timing.then(std::time::Instant::now);
            transaction.commit()?;
            report_phase("commit", started, 1);
            Ok(())
        })
        .await
        .expect("exercise terminal cap trigger");
    let started = timing.then(std::time::Instant::now);
    let count = store
        .with_read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM analysis_requests
                      WHERE state IN ('ready', 'failed', 'cancelled')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
        .await
        .expect("count terminal history");
    assert_eq!(count, 8_192);
    report_phase("final_read", started, 1);
}

#[tokio::test]
async fn duplicate_content_rebinds_a_cancelled_job_to_a_surviving_file() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let original = job(1, 10, 10);
    assert!(store
        .enqueue_cluster_fragment_index(&original)
        .await
        .expect("enqueue original"));
    store
        .with_conn(|conn| {
            conn.execute("DELETE FROM files WHERE id = 1", [])?;
            Ok(())
        })
        .await
        .expect("delete original");

    let replacement = job(2, 20, 20);
    assert!(store
        .enqueue_cluster_fragment_index(&replacement)
        .await
        .expect("rebind replacement"));
    let claimed = store
        .fixture_claim_cluster_fragment_index("node-b", &[], 20, 1_020)
        .await
        .expect("claim")
        .expect("replacement claim");
    assert_eq!(claimed.file_id, 2);
    assert_eq!(claimed.source_mtime, 20);
    assert_eq!(claimed.source_sha256, replacement.source_sha256);
}

#[tokio::test]
async fn byte_identical_replacement_publishes_an_independent_artifact() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let original = job(1, 10, 10);
    let original_key = original.cache_key.clone();
    let seeded = original.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 1, 1, 0, 10, 10)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                ],
            )?;
            conn.execute(
                "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                    "e".repeat(64),
                ],
            )?;
            conn.execute("DELETE FROM files WHERE id = 1", [])?;
            Ok(())
        })
        .await
        .expect("publish then delete original");

    let replacement = job(2, 20, 20);
    assert!(store
        .enqueue_cluster_fragment_index(&replacement)
        .await
        .expect("enqueue replacement"));
    let claimed = store
        .fixture_claim_cluster_fragment_index("node-b", &[], 20, 1_020)
        .await
        .expect("claim")
        .expect("replacement claim");
    assert_eq!(claimed.file_id, 2);

    let rebuilt = ClusterFragmentIndexArtifact {
        cache_key: replacement.cache_key.clone(),
        file_id: 2,
        source_size: 100,
        source_mtime: 20,
        source_sha256: replacement.source_sha256,
        pipeline_sha256: replacement.pipeline_sha256,
        blob_sha256: "e".repeat(64),
        bytes: 10,
        built_by_node_id: "node-b".to_owned(),
        built_at_ms: 21,
    };
    let location = ClusterFragmentIndexLocation {
        cache_key: replacement.cache_key.clone(),
        node_id: "node-b".to_owned(),
        bytes: 10,
        verified_at_ms: 21,
        last_seen_at_ms: 21,
    };
    assert!(store
        .fixture_complete_cluster_fragment_index(&claimed, &rebuilt, &location, 21)
        .await
        .expect("complete repair"));
    assert_eq!(
        store
            .cluster_fragment_index_artifact(&replacement.cache_key)
            .await
            .expect("artifact")
            .expect("published artifact")
            .blob_sha256,
        "e".repeat(64)
    );
    assert_eq!(
        store
            .cluster_fragment_index_locations(&replacement.cache_key)
            .await
            .expect("locations")
            .into_iter()
            .map(|location| location.node_id)
            .collect::<Vec<_>>(),
        vec!["node-b".to_owned()]
    );
    assert!(store
        .cluster_fragment_index_artifact(&original_key)
        .await
        .expect("original artifact")
        .is_some());
}

#[tokio::test]
async fn ready_artifact_at_the_attempt_limit_gets_a_fresh_repair_budget() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let original = job(1, 10, 10);
    let seeded = original.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 5, 5, 0, 1, 1)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                ],
            )?;
            conn.execute(
                "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 1)",
                params![
                    seeded.cache_key,
                    seeded.file_id,
                    seeded.source_size,
                    seeded.source_mtime,
                    seeded.source_sha256,
                    seeded.pipeline_sha256,
                    "e".repeat(64),
                ],
            )?;
            Ok(())
        })
        .await
        .expect("seed exhausted ready artifact");

    let mut repair = original;
    let now = QUEUE_ELIGIBILITY_MS.saturating_add(10);
    repair.not_before_ms = now;
    repair.created_at_ms = now;
    assert!(store
        .requeue_cluster_fragment_index(&repair)
        .await
        .expect("requeue"));
    let claimed = store
        .fixture_claim_cluster_fragment_index("node-b", &[], now, now.saturating_add(1_000))
        .await
        .expect("claim")
        .expect("fresh repair claim");
    assert_eq!(claimed.attempts, 1);
    assert_eq!(claimed.created_at_ms, now);
}

#[tokio::test]
async fn node_local_exclusions_skip_an_unreadable_head_job() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let first = job(1, 10, 10);
    let mut second = job(2, 20, 20);
    second.source_sha256 = "c".repeat(64);
    second.cache_key = cluster_fragment_index_key(
        second.file_id,
        second.source_size,
        second.source_mtime,
        &second.source_sha256,
        &second.pipeline_sha256,
    )
    .expect("second key");
    assert!(store
        .enqueue_cluster_fragment_index(&first)
        .await
        .expect("enqueue first"));
    assert!(store
        .enqueue_cluster_fragment_index(&second)
        .await
        .expect("enqueue second"));

    let claimed = store
        .fixture_claim_cluster_fragment_index(
            "node-b",
            std::slice::from_ref(&first.cache_key),
            20,
            1_020,
        )
        .await
        .expect("claim")
        .expect("later runnable job");
    assert_eq!(claimed.cache_key, second.cache_key);
}

#[tokio::test]
async fn exclusions_skip_more_than_128_unreadable_head_jobs() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let mut exclusions = Vec::new();
    let mut runnable = String::new();
    for sequence in 1_i64..=130 {
        let mut candidate = job(1, 10, sequence);
        candidate.source_sha256 = format!("{sequence:064x}");
        candidate.cache_key = cluster_fragment_index_key(
            candidate.file_id,
            candidate.source_size,
            candidate.source_mtime,
            &candidate.source_sha256,
            &candidate.pipeline_sha256,
        )
        .expect("candidate key");
        assert!(store
            .enqueue_cluster_fragment_index(&candidate)
            .await
            .expect("enqueue candidate"));
        if sequence < 130 {
            exclusions.push(candidate.cache_key);
        } else {
            runnable = candidate.cache_key;
        }
    }

    let claimed = store
        .fixture_claim_cluster_fragment_index("node-b", &exclusions, 130, 1_130)
        .await
        .expect("claim")
        .expect("later runnable job");
    assert_eq!(claimed.cache_key, runnable);
}

#[tokio::test]
async fn expired_unreadable_queue_cannot_wedge_later_admission() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    store
        .with_conn(|conn| {
            let mut insert = conn.prepare(
                "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, owner_node_id, fence, lease_expires_ms,
                       attempts, not_before_ms, created_at_ms, updated_at_ms)
                     VALUES (?1, 1, 100, 10, ?2, ?3, ?4, ?5, 0, ?6, ?7, 1, 1, 1)",
            )?;
            for sequence in 0_i64..MAX_ACTIVE_JOBS {
                let crashed = sequence >= MAX_ACTIVE_JOBS / 2;
                insert.execute(params![
                    format!("{sequence:064x}"),
                    format!("{:064x}", sequence.saturating_add(1)),
                    "b".repeat(64),
                    if crashed { "running" } else { "queued" },
                    crashed.then_some("lost-node"),
                    crashed.then_some(1_i64),
                    if crashed {
                        crate::store::fragment_index_cluster::DEFAULT_ANALYSIS_MAX_ATTEMPTS
                    } else {
                        0
                    },
                ])?;
            }
            Ok(())
        })
        .await
        .expect("fill unreadable queue");

    let now = QUEUE_ELIGIBILITY_MS.saturating_add(10);
    let mut readable = job(2, 20, now);
    readable.source_sha256 = "f".repeat(64);
    readable.pipeline_sha256 = "e".repeat(64);
    readable.cache_key = cluster_fragment_index_key(
        readable.file_id,
        readable.source_size,
        readable.source_mtime,
        &readable.source_sha256,
        &readable.pipeline_sha256,
    )
    .expect("readable key");
    assert!(store
        .enqueue_cluster_fragment_index(&readable)
        .await
        .expect("admit after eligibility window"));
    let claimed = store
        .fixture_claim_cluster_fragment_index("node-b", &[], now, now.saturating_add(1_000))
        .await
        .expect("claim")
        .expect("later readable job");
    assert_eq!(claimed.cache_key, readable.cache_key);
}

#[tokio::test]
async fn failed_repair_cannot_bypass_the_active_queue_cap() {
    // Repair is automatic work and cannot consume the foreground reserve.
    const MAX_ACTIVE_JOBS: i64 = (crate::store::background_jobs::MAX_ACTIVE_JOBS
        - crate::store::background_jobs::FOREGROUND_RESERVED_JOBS)
        as i64;
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let mut repair = job(2, 20, 100);
    repair.source_sha256 = "f".repeat(64);
    repair.pipeline_sha256 = "e".repeat(64);
    repair.cache_key = cluster_fragment_index_key(
        repair.file_id,
        repair.source_size,
        repair.source_mtime,
        &repair.source_sha256,
        &repair.pipeline_sha256,
    )
    .expect("repair key");
    let seeded_repair = repair.clone();
    store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms, last_error_code)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'failed', 1, 0, 1, 1, 1,
                       'queue_expired')",
                    params![
                        seeded_repair.cache_key,
                        seeded_repair.file_id,
                        seeded_repair.source_size,
                        seeded_repair.source_mtime,
                        seeded_repair.source_sha256,
                        seeded_repair.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 1)",
                    params![
                        seeded_repair.cache_key,
                        seeded_repair.file_id,
                        seeded_repair.source_size,
                        seeded_repair.source_mtime,
                        seeded_repair.source_sha256,
                        seeded_repair.pipeline_sha256,
                        "d".repeat(64),
                    ],
                )?;
                let mut insert = conn.prepare(
                    "INSERT INTO background_jobs
                      (id, kind, payload_version, payload_json, dedupe_key, priority,
                       state, not_before_ms, created_at_ms, updated_at_ms)
                     VALUES (?1, 'fragment_index_build', 1, ?2, ?3, 0, 'queued', 100, 100, 100)",
                )?;
                for sequence in 0_i64..MAX_ACTIVE_JOBS {
                    insert.execute(params![
                        uuid::Uuid::from_u128(sequence as u128 + 1).to_string(),
                        serde_json::json!({"kind": "fragment_index_build", "file_id": 1,
                            "source_generation": "fixture:1", "source_size": 100, "source_mtime": 10,
                            "source_sha256": "a".repeat(64), "cache_key": format!("{sequence:064x}"),
                            "pipeline_digest": "b".repeat(64)}).to_string(),
                        format!("capacity:{sequence}"),
                    ])?;
                }
                Ok(())
            })
            .await
            .expect("seed full queue and failed repair");

    assert!(!store
        .requeue_cluster_fragment_index(&repair)
        .await
        .expect("full requeue"));
    let active = store
        .with_read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM background_jobs
                      WHERE state IN ('queued', 'running')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
        .await
        .expect("active count");
    assert_eq!(active, MAX_ACTIVE_JOBS);

    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE background_jobs SET state = 'failed'
                      WHERE id = ?1",
                params![uuid::Uuid::from_u128(1).to_string()],
            )?;
            Ok(())
        })
        .await
        .expect("free one slot");
    assert!(store
        .requeue_cluster_fragment_index(&repair)
        .await
        .expect("requeue with capacity"));
}

#[tokio::test]
async fn retention_prunes_only_terminal_artifacts_without_recent_holders() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let stale = job(1, 10, 10);
    let mut current = job(2, 20, 10);
    current.source_sha256 = "c".repeat(64);
    current.pipeline_sha256 = "d".repeat(64);
    current.cache_key = cluster_fragment_index_key(
        current.file_id,
        current.source_size,
        current.source_mtime,
        &current.source_sha256,
        &current.pipeline_sha256,
    )
    .expect("current key");
    let stale_key = stale.cache_key.clone();
    let current_key = current.cache_key.clone();
    let stale_seed = stale.clone();
    let current_seed = current.clone();
    store
        .with_conn(move |conn| {
            for candidate in [&stale_seed, &current_seed] {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                          (cache_key, file_id, source_size, source_mtime, source_sha256,
                           pipeline_sha256, state, fence, attempts, not_before_ms,
                           created_at_ms, updated_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 1, 1, 0, 10, 10)",
                    params![
                        candidate.cache_key,
                        candidate.file_id,
                        candidate.source_size,
                        candidate.source_mtime,
                        candidate.source_sha256,
                        candidate.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                          (cache_key, file_id, source_size, source_mtime, source_sha256,
                           pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                    params![
                        candidate.cache_key,
                        candidate.file_id,
                        candidate.source_size,
                        candidate.source_mtime,
                        candidate.source_sha256,
                        candidate.pipeline_sha256,
                        "e".repeat(64),
                    ],
                )?;
            }
            conn.execute(
                "INSERT INTO cluster_fragment_index_locations
                      (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                     VALUES (?1, 'node-a', 10, 20, 20), (?2, 'node-a', 10, 200, 200)",
                params![stale_key, current_key],
            )?;
            Ok(())
        })
        .await
        .expect("seed artifacts");

    assert_eq!(
        store
            .prune_cluster_fragment_indexes(100, 128)
            .await
            .expect("prune"),
        vec![stale.cache_key.clone()]
    );
    assert!(store
        .cluster_fragment_index_artifact(&stale.cache_key)
        .await
        .expect("stale lookup")
        .is_none());
    assert!(store
        .cluster_fragment_index_artifact(&current.cache_key)
        .await
        .expect("current lookup")
        .is_some());
}

#[tokio::test]
async fn content_analysis_retry_survives_foreground_rediscovery_and_six_hours() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let original = job(1, 10, 1);
    assert!(store
        .enqueue_cluster_fragment_index(&original)
        .await
        .expect("enqueue"));
    let claimed = store
        .fixture_claim_cluster_fragment_index("node-a", &[], 10, 1_000)
        .await
        .expect("claim")
        .expect("claimed job");
    assert_eq!(claimed.attempts, 1);
    assert!(store
        .fixture_fail_cluster_fragment_index_typed(&crate::queue_fixture::FragmentFailureFixture {
            cache_key: claimed.cache_key.clone(),
            target_node_id: claimed.target_node_id.clone(),
            node_id: "node-a".to_owned(),
            fence: claimed.fence,
            code: crate::content_analysis::IndexFailureCode::IndexProbeTimeout,
            transient_allowlisted: false,
            diagnostic: crate::content_analysis::IndexDiagnostic::default(),
            now_ms: 20,
        },)
        .await
        .expect("typed failure"));

    let mut rediscovered = original.clone();
    rediscovered.priority = "foreground".to_owned();
    rediscovered.trigger = "foreground".to_owned();
    rediscovered.not_before_ms = 30;
    rediscovered.created_at_ms = 30;
    assert!(store
        .enqueue_cluster_fragment_index(&rediscovered)
        .await
        .expect("foreground rediscovery"));

    let (attempts, not_before_ms, deadline_ms, code, history) = store
        .with_read(move |conn| {
            conn.query_row(
                "SELECT attempts, not_before_ms, index_retry_deadline_ms,
                            COALESCE(last_error_code, ''), attempt_errors
                       FROM cluster_fragment_index_jobs
                      WHERE cache_key = ?1 AND target_node_id = ?2",
                params![claimed.cache_key, claimed.target_node_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(Into::into)
        })
        .await
        .expect("retry state");
    assert_eq!(attempts, 1);
    assert_eq!(
        not_before_ms,
        20 + crate::content_analysis::INDEX_RETRY_BASE_MS
    );
    assert_eq!(
        deadline_ms,
        20 + crate::content_analysis::INDEX_RETRY_WINDOW_MS
    );
    assert_eq!(code, "index_probe_timeout");
    assert_eq!(history, "index_probe_timeout");

    let six_hours = 6 * 60 * 60 * 1_000;
    let retried = store
        .fixture_claim_cluster_fragment_index("node-a", &[], six_hours, six_hours + 1_000)
        .await
        .expect("claim after six hours")
        .expect("retry cycle was not queue-expired");
    assert_eq!(retried.attempts, 2);
    assert_eq!(retried.index_retry_deadline_ms, deadline_ms);
}

#[tokio::test]
async fn mkv_hls_terminal_completion_repair_is_exact_and_idempotent() {
    let store = SqliteStore::open_in_memory().expect("store");
    seed_files(&store).await;
    let predecessor = job(1, 10, 1);
    let seeded = predecessor.clone();
    let diagnostic = crate::content_analysis::IndexDiagnostic {
        version: 1,
        code: crate::content_analysis::IndexFailureCode::IndexCompletionUnverified
            .as_str()
            .to_owned(),
        retryable: false,
        claim_fence: 4,
        attempt: 5,
        recorded_at_ms: 50,
        ..Default::default()
    }
    .encode_bounded()
    .expect("bounded terminal diagnostic")
    .replace("\"code\":", "\"code\" : ");
    store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, target_node_id, state, fence, attempts,
                       not_before_ms, last_error_code, index_diagnostic_json,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'failed', 4, 5,
                             9223372036854775807, 'attempt_limit', ?8, 1, 50)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                        seeded.target_node_id,
                        diagnostic,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, target_node_id, state, fence, attempts,
                       not_before_ms, last_error_code, index_diagnostic_json,
                       created_at_ms, updated_at_ms)
                     VALUES ('nested-code-decoy', ?1, ?2, ?3, ?4, ?5, 'node-b',
                             'failed', 4, 5, 9223372036854775807, 'attempt_limit',
                             '{\"version\":1,\"code\":\"index_process_failed\",\"stderr_tail\":[\"{\\\"code\\\":\\\"index_completion_unverified\\\"}\"]}',
                             1, 50)",
                    params![
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO analysis_requests
                      (request_id, file_id, source_size, source_mtime, component,
                       pipeline_version, video_identity, requested_generation,
                       priority, trigger, force_rebuild, target_node_id, state,
                       fence, attempts, not_before_ms, result_cache_key,
                       last_error_code, created_at_ms, updated_at_ms)
                     VALUES ('legacy-request', ?1, ?2, ?3, 'fragment_index',
                             'engine-v1', 'video-a', 'legacy-generation',
                             'normal', 'background', 0, ?4, 'failed', 1, 1, 50,
                             ?5, 'truncated', 1, 50)",
                    params![
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.target_node_id,
                        seeded.cache_key,
                    ],
                )?;
                Ok(())
            })
            .await
            .expect("seed legacy failure");

    let candidates = store
        .preview_analysis_index_repairs(None, 10)
        .await
        .expect("preview");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].eligibility, "eligible");
    assert_eq!(candidates[0].video_identity, "video-a");
    assert_eq!(candidates[0].cause, "index_completion_unverified");
    let first = store
        .apply_analysis_index_repair(&candidates[0], "repair-one", 100)
        .await
        .expect("first apply");
    assert_eq!(first.status, "created");
    let repeated = store
        .apply_analysis_index_repair(&candidates[0], "repair-two", 101)
        .await
        .expect("repeated apply");
    assert_eq!(repeated.status, "already_created");
    assert_eq!(repeated.successor_request_id, "repair-one");

    let (receipts, successors, predecessor_state) = store
        .with_read(move |conn| {
            Ok((
                conn.query_row("SELECT COUNT(*) FROM analysis_index_repairs", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                conn.query_row(
                    "SELECT COUNT(*) FROM analysis_requests
                          WHERE request_id IN ('repair-one','repair-two')",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                conn.query_row(
                    "SELECT state FROM cluster_fragment_index_jobs WHERE cache_key = ?1",
                    params![predecessor.cache_key],
                    |row| row.get::<_, String>(0),
                )?,
            ))
        })
        .await
        .expect("repair counts");
    assert_eq!(receipts, 1);
    assert_eq!(successors, 1);
    assert_eq!(predecessor_state, "failed");
}

/// The subtitle-source store's miss reason reads this: which nodes built
/// the artifacts a node holds for one file's current source.
#[tokio::test]
async fn fragment_index_builders_held_by_names_who_built_what_a_node_holds() {
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .with_conn(|conn| {
            for (key, file_id, size, mtime, builder) in [
                ("k-built-here", 7_i64, 100_i64, 10_i64, "node-a"),
                ("k-hydrated", 7, 100, 10, "node-b"),
                ("k-older-source", 7, 99, 9, "node-c"),
                ("k-other-file", 8, 100, 10, "node-d"),
            ] {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                          (cache_key, file_id, source_size, source_mtime, source_sha256,
                           pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?5, 10, ?6, 10)",
                    params![key, file_id, size, mtime, "e".repeat(64), builder],
                )?;
            }
            for (key, node) in [
                ("k-built-here", "node-a"),
                ("k-hydrated", "node-a"),
                ("k-older-source", "node-a"),
                ("k-other-file", "node-a"),
                ("k-hydrated", "node-z"),
            ] {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_locations
                          (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                         VALUES (?1, ?2, 10, 10, 10)",
                    params![key, node],
                )?;
            }
            Ok(())
        })
        .await
        .expect("seed artifacts and locations");

    assert_eq!(
        store
            .fragment_index_builders_held_by("node-a", 7, 100, 10)
            .await
            .expect("read"),
        vec!["node-a".to_owned(), "node-b".to_owned()],
        "this source only, built here and hydrated from node-b"
    );
    assert_eq!(
        store
            .fragment_index_builders_held_by("node-z", 7, 100, 10)
            .await
            .expect("read"),
        vec!["node-b".to_owned()],
        "a node that only hydrated"
    );
    assert!(store
        .fragment_index_builders_held_by("node-q", 7, 100, 10)
        .await
        .expect("read")
        .is_empty());
}
