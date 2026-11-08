//! G1: the fleet read behind Activity → Starts.

use super::*;
use plurx_core::store::AnalysisHistoryRow;

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

fn job(file: i64, request: &str, state: &str, error: &str) -> AnalysisHistoryRow {
    AnalysisHistoryRow {
        row_key: format!("request:{request}"),
        request_id: request.to_owned(),
        job_id: format!("job-{request}"),
        file_id: file,
        item_id: 0,
        title: String::new(),
        component: "fragment_index".to_owned(),
        force_rebuild: false,
        target_node_id: String::new(),
        request_state: state.to_owned(),
        job_state: state.to_owned(),
        state: state.to_owned(),
        disposition: String::new(),
        action: String::new(),
        owner_node_id: String::new(),
        claim_epoch: 0,
        lease_expires_ms: 0,
        attempts: 0,
        not_before_ms: 0,
        request_error_code: String::new(),
        job_error_code: error.to_owned(),
        created_at_ms: 0,
        updated_at_ms: 0,
        pipeline_version: String::new(),
        requested_generation: String::new(),
        priority: String::new(),
        trigger: String::new(),
        cancel_requested: false,
        phase: String::new(),
        source_size: 0,
        job_attempt_errors: String::new(),
        index_retry_deadline_ms: 0,
        index_diagnostic_json: String::new(),
    }
}

/// `index now` is the retry for a terminal job, an enqueue when there is no
/// job, and a join (not a second job) when one is already working.
#[test]
fn index_now_retries_enqueues_or_joins() {
    let jobs = jobs_by_file(
        &[job(3, "working-3", "running", "")],
        &[
            job(2, "failed-2", "failed", "attempt_limit"),
            job(3, "failed-3", "failed", "attempt_limit"),
            job(4, "stale-4", "failed", "source_superseded"),
        ],
    );
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
        "active work wins over an older terminal row for the same file"
    );
    assert_eq!(
        action(4, Some("pending")),
        json!({"kind": "enqueue", "method": "POST", "path": "/api/v1/files/4/analysis",
               "body": {"components": ["fragment_index"]}}),
        "a superseded job cannot be retried; the file is asked afresh"
    );
    assert_eq!(action(5, Some("partial"))["kind"], "enqueue");
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
