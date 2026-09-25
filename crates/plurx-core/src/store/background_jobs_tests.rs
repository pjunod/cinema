//! Regression contracts compiled during construction and executed in the
//! final fast lane, after the main-bound adversarial review.

use super::background_jobs::*;
use super::sqlite::SqliteStore;

fn enqueue(now_ms: i64) -> EnqueueJob {
    EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: JobPayload::FragmentIndexBuild {
            file_id: 1,
            source_generation: "source:1".to_owned(),
            pipeline_digest: "a".repeat(64),
        },
        dedupe_key: "fragment:1".to_owned(),
        priority: 1,
        not_before_ms: now_ms,
        now_ms,
        request: JobRequest {
            scope: "user:1".to_owned(),
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: "b".repeat(64),
            consumer_kind: "analysis".to_owned(),
            consumer_ref: "analysis:1".to_owned(),
            deadline_ms: None,
        },
    }
}

fn claim(job_id: &str, revision: i64, now_ms: i64) -> ClaimJob {
    ClaimJob {
        job_id: job_id.to_owned(),
        expected_revision: revision,
        node_id: "node-a".to_owned(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        kind: JobKind::FragmentIndexBuild,
        payload_version: 1,
        now_ms,
        dispatched_at_ms: now_ms,
    }
}

async fn claimed(store: &SqliteStore, request: ClaimJob) -> BackgroundJob {
    match store.claim_job(request).await.expect("claim") {
        ClaimOutcome::Claimed { job } => *job,
        outcome => panic!("expected claim, got {outcome:?}"),
    }
}

#[tokio::test]
async fn background_jobs_dedupe_and_request_identity_are_distinct() {
    let store = SqliteStore::open_in_memory().expect("store");
    let request = enqueue(1_000);
    let original = store.enqueue_job(request.clone()).await.expect("enqueue");
    assert!(
        matches!(original, EnqueueOutcome::Accepted { ref job_id, .. } if job_id == &request.id)
    );
    let repeated = store.enqueue_job(request.clone()).await.expect("repeat");
    assert!(
        matches!(repeated, EnqueueOutcome::Existing { ref job_id, .. } if job_id == &request.id)
    );
    let mut second = enqueue(1_001);
    let joined = store.enqueue_job(second.clone()).await.expect("join");
    assert!(matches!(joined, EnqueueOutcome::Accepted { ref job_id, .. } if job_id == &request.id));
    second.request.request_digest = "c".repeat(64);
    assert!(matches!(
        store.enqueue_job(second).await.expect("conflict"),
        EnqueueOutcome::Conflict
    ));
    let page = store
        .list_jobs(JobQuery {
            state: None,
            kind: None,
            after_id: None,
            limit: 100,
        })
        .await
        .expect("page");
    assert_eq!(page.jobs.len(), 1);
}

#[tokio::test]
async fn background_jobs_replayed_claim_never_advances_fence_or_changes_boot() {
    let store = SqliteStore::open_in_memory().expect("store");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    let first = claim(&request.id, 0, 1_000);
    let initial = claimed(&store, first.clone()).await;
    let repeated = claimed(&store, first.clone()).await;
    assert_eq!(initial.token, repeated.token);
    let mut restarted = first;
    restarted.boot_id = uuid::Uuid::new_v4().to_string();
    assert!(matches!(
        store.claim_job(restarted).await.expect("old boot"),
        ClaimOutcome::ExpiredOrPruned
    ));
}

#[tokio::test]
async fn background_jobs_cancellation_wins_renewal_and_rejects_new_interest() {
    let store = SqliteStore::open_in_memory().expect("store");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    let first = claim(&request.id, 0, 1_000);
    let token = claimed(&store, first.clone()).await.token.expect("token");
    let cancelled = store
        .cancel_job(CancelJob {
            job_id: request.id.clone(),
            now_ms: 2_000,
        })
        .await
        .expect("cancel")
        .expect("row");
    assert_eq!(cancelled.state, JobState::Cancelling);
    let renewed = store
        .renew_jobs(RenewJobs {
            tokens: vec![token.clone()],
            now_ms: 2_001,
        })
        .await
        .expect("renew");
    assert!(matches!(renewed[0], RenewOutcome::LostOwnership { .. }));
    assert!(matches!(
        store.enqueue_job(enqueue(2_001)).await.expect("join"),
        EnqueueOutcome::JobCancelling { .. }
    ));
    let cleanup = store
        .resolve_claim(ResolveClaim {
            job_id: request.id,
            node_id: first.node_id,
            boot_id: first.boot_id,
            claim_id: first.claim_id,
            fence: Some(token.fence),
            dispatched_at_ms: 1_000,
            now_ms: 2_001,
        })
        .await
        .expect("resolve");
    let ClaimResolution::CancelRequested { cleanup_token } = cleanup else {
        panic!("cleanup token")
    };
    let settled = store
        .settle_job(SettleJob {
            token: cleanup_token,
            settlement: JobSettlement::Cancel,
            now_ms: 2_002,
        })
        .await
        .expect("settle")
        .expect("row");
    assert_eq!(settled.state, JobState::Cancelled);
}

#[tokio::test]
async fn background_jobs_priority_edits_preserve_renewal_and_takeover_rejects_old_owner() {
    let store = SqliteStore::open_in_memory().expect("store");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    let initial = claimed(&store, claim(&request.id, 0, 1_000)).await;
    let initial_token = initial.token.expect("token");
    let mut foreground = enqueue(2_000);
    foreground.priority = 3;
    store
        .enqueue_job(foreground)
        .await
        .expect("higher priority");
    let renewed = store
        .renew_jobs(RenewJobs {
            tokens: vec![initial_token.clone()],
            now_ms: 2_000,
        })
        .await
        .expect("renew");
    let RenewOutcome::Renewed { token } = &renewed[0] else {
        panic!("priority cannot invalidate ownership")
    };
    let successor = claimed(&store, claim(&request.id, token.revision, 32_001)).await;
    assert!(successor.fence > token.fence);
    let stale = store
        .settle_job(SettleJob {
            token: token.clone(),
            settlement: JobSettlement::Fail {
                error_code: "old_owner".to_owned(),
            },
            now_ms: 32_002,
        })
        .await
        .expect("stale settlement");
    assert!(stale.is_none());
}

#[tokio::test]
async fn background_jobs_reservations_bound_distinct_jobs_and_yield_releases_capacity() {
    let store = SqliteStore::open_in_memory().expect("store");
    let mut tokens = Vec::new();
    let mut third = None;
    for index in 0..3 {
        let mut request = enqueue(1_000);
        request.dedupe_key = format!("fragment:{index}");
        store.enqueue_job(request.clone()).await.expect("enqueue");
        let attempt = claim(&request.id, 0, 1_000);
        if index < 2 {
            tokens.push(claimed(&store, attempt).await.token.expect("token"));
        } else {
            third = Some(attempt);
        }
    }
    let third = third.expect("third claim");
    assert!(matches!(
        store.claim_job(third.clone()).await.expect("full"),
        ClaimOutcome::Contended
    ));
    store
        .settle_job(SettleJob {
            token: tokens.remove(0),
            settlement: JobSettlement::Yield {
                checkpoint: None,
                not_before_ms: 10_000,
            },
            now_ms: 2_000,
        })
        .await
        .expect("yield")
        .expect("settled");
    let job = claimed(&store, third).await;
    assert_eq!(job.fence, 1);
}

#[cfg(feature = "hiqlite-store")]
#[test]
fn background_jobs_sql_is_valid_for_replicated_execution() {
    use super::background_jobs::{CLAIM_SQL, ENQUEUE_SQL, JOB_JSON, SCHEMA};
    for sql in [
        SCHEMA.to_owned(),
        ENQUEUE_SQL.to_owned(),
        format!("{CLAIM_SQL} RETURNING {JOB_JSON} AS result_json"),
    ] {
        super::hiqlite::validate_sql(&sql).expect("replicated SQL");
    }
}
