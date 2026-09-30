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
            source_size: 100,
            source_mtime: 1,
            source_sha256: "d".repeat(64),
            cache_key: "c".repeat(64),
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
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
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
            job_id: request.id.clone(),
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
        .expect("settle");
    assert!(settled);
    let settled = store
        .background_job(&request.id)
        .await
        .expect("read")
        .expect("job");
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
    assert!(!stale);
}

#[tokio::test]
async fn background_jobs_reservations_bound_distinct_jobs_and_yield_releases_capacity() {
    let store = SqliteStore::open_in_memory().expect("store");
    let mut tokens = Vec::new();
    let mut third = None;
    for index in 0..3 {
        let mut request = enqueue(1_000);
        request.dedupe_key = format!("fragment:{index}");
        if index == 1 {
            request.request.scope = "playback-artifact".to_owned();
            request.request.consumer_kind = "playback_fragment".to_owned();
            request.request.target_node_id = Some("node-a".to_owned());
            request.request.deadline_ms = Some(10_000);
        }
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
        .expect("yield");
    let job = claimed(
        &store,
        ClaimJob {
            now_ms: 2_001,
            dispatched_at_ms: 2_001,
            ..third
        },
    )
    .await;
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

#[tokio::test]
async fn background_jobs_one_cancelled_waiter_does_not_revoke_another_interest() {
    let store = SqliteStore::open_in_memory().expect("store");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    let second = enqueue(1_001);
    store.enqueue_job(second.clone()).await.expect("join");
    let job = claimed(&store, claim(&request.id, 0, 1_002)).await;
    store
        .cancel_waiter(CancelWaiter {
            scope: request.request.scope,
            request_id: request.request.request_id,
            now_ms: 1_003,
        })
        .await
        .expect("detach");
    let renewal = store
        .renew_jobs(RenewJobs {
            tokens: vec![job.token.expect("token")],
            now_ms: 1_004,
        })
        .await
        .expect("renew");
    assert!(matches!(renewal[0], RenewOutcome::Renewed { .. }));
    assert!(matches!(
        store.enqueue_job(second).await.expect("receipt"),
        EnqueueOutcome::Existing {
            cancelled: false,
            ..
        }
    ));
}

#[tokio::test]
async fn background_jobs_twenty_yields_compact_history_without_spending_failure_budget() {
    let store = SqliteStore::open_in_memory().expect("store");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    let mut revision = 0;
    for index in 0..20 {
        let now_ms = 1_000 + index * 180_000;
        let job = claimed(&store, claim(&request.id, revision, now_ms)).await;
        assert_eq!(job.fence, index + 1);
        let settled = store
            .settle_job(SettleJob {
                token: job.token.expect("token"),
                settlement: JobSettlement::Yield {
                    checkpoint: None,
                    not_before_ms: now_ms + 1,
                },
                now_ms: now_ms + 1,
            })
            .await
            .expect("yield");
        assert!(settled);
        let settled = store
            .background_job(&request.id)
            .await
            .expect("read")
            .expect("job");
        assert_eq!(settled.failed_attempts, 0);
        revision = settled.revision;
        store.maintain_jobs(now_ms + 2).await.expect("upkeep");
    }
    store.maintain_jobs(4_000_000).await.expect("final upkeep");
    let final_job = claimed(&store, claim(&request.id, revision, 4_000_001)).await;
    assert_eq!(final_job.fence, 21);
    assert_eq!(final_job.failed_attempts, 0);
    assert_eq!(final_job.yield_count, 4);
}

#[tokio::test]
async fn background_jobs_expired_cancellation_is_reaped_and_empty_queue_needs_no_write() {
    let store = SqliteStore::open_in_memory().expect("store");
    assert!(!store.maintain_jobs(1_000).await.expect("idle"));
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    claimed(&store, claim(&request.id, 0, 1_000)).await;
    store
        .cancel_job(CancelJob {
            job_id: request.id,
            now_ms: 2_000,
        })
        .await
        .expect("cancel");
    assert!(store.maintain_jobs(31_001).await.expect("reap"));
    let page = store
        .list_jobs(JobQuery {
            state: None,
            kind: None,
            after_id: None,
            limit: 100,
        })
        .await
        .expect("page");
    assert_eq!(page.jobs[0].state, JobState::Cancelled);
    assert!(page.jobs[0].token.is_none());
    assert!(!store.maintain_jobs(31_002).await.expect("idle again"));
}

#[tokio::test]
async fn background_jobs_receipts_survive_seven_days_and_domain_identity_outlives_details() {
    for retain_identity in [false, true] {
        let store = SqliteStore::open_in_memory().expect("store");
        let mut request = enqueue(1_000);
        request.request.retain_identity = retain_identity;
        store.enqueue_job(request.clone()).await.expect("enqueue");
        store
            .cancel_job(CancelJob {
                job_id: request.id.clone(),
                now_ms: 2_000,
            })
            .await
            .expect("cancel");
        store
            .maintain_jobs(REQUEST_RETENTION_MS)
            .await
            .expect("before expiry");
        assert!(matches!(
            store.enqueue_job(request.clone()).await.expect("retained"),
            EnqueueOutcome::Existing {
                cancelled: true,
                ..
            }
        ));
        let after_expiry = REQUEST_RETENTION_MS + 2_001;
        store
            .maintain_jobs(after_expiry)
            .await
            .expect("retire details");
        let page = store
            .list_jobs(JobQuery {
                state: None,
                kind: None,
                after_id: None,
                limit: 100,
            })
            .await
            .expect("page");
        assert!(page.jobs.is_empty());
        request.id = uuid::Uuid::new_v4().to_string();
        request.now_ms = after_expiry;
        let repeated = store.enqueue_job(request).await.expect("after retention");
        if retain_identity {
            assert!(matches!(
                repeated,
                EnqueueOutcome::Existing {
                    cancelled: true,
                    ..
                }
            ));
        } else {
            assert!(matches!(repeated, EnqueueOutcome::Accepted { .. }));
        }
    }
}

#[tokio::test]
async fn background_jobs_discovery_lease_advances_with_every_admission_verdict() {
    use crate::cluster::coordination::{unix_ms, LeaseClaim};
    use crate::store::CoordinationStore;
    let store = SqliteStore::open_in_memory().expect("store");
    let now = unix_ms().expect("clock");
    let LeaseClaim::Acquired(lease) = store
        .acquire_lease("candidate:test", "node-a", now, now + 90_000)
        .await
        .expect("lease")
    else {
        panic!("lease held")
    };
    let request = enqueue(now);
    let replacement = lease.publication_successor().expect("replacement");
    assert!(matches!(
        store
            .enqueue_job_fenced(request.clone(), lease.clone(), replacement.clone())
            .await
            .expect("admission"),
        EnqueueOutcome::Accepted { .. }
    ));
    assert!(matches!(
        store
            .enqueue_job_fenced(request.clone(), lease, replacement.clone())
            .await,
        Err(crate::error::StoreError::FenceRejected { .. })
    ));
    let third = replacement.publication_successor().expect("replacement");
    assert!(matches!(
        store
            .enqueue_job_fenced(request.clone(), replacement, third.clone())
            .await
            .expect("receipt"),
        EnqueueOutcome::Existing { .. }
    ));
    let mut collision = request;
    collision.request.request_digest = "d".repeat(64);
    let fourth = third.publication_successor().expect("replacement");
    assert!(matches!(
        store
            .enqueue_job_fenced(collision, third, fourth.clone())
            .await
            .expect("conflict"),
        EnqueueOutcome::Conflict
    ));
    let mut next = enqueue(now);
    next.dedupe_key = "fragment:2".into();
    assert!(matches!(
        store
            .enqueue_job_fenced(
                next,
                fourth.clone(),
                fourth.publication_successor().expect("replacement")
            )
            .await
            .expect("next admission"),
        EnqueueOutcome::Accepted { .. }
    ));
}

#[tokio::test]
async fn background_jobs_unknown_payloads_remain_visible_and_cancellable() {
    let store = SqliteStore::open_in_memory().expect("store");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    QueueSql::queue_sql(
        &store,
        "UPDATE background_jobs SET payload_version = 2,
        payload_json = '{\"kind\":\"future_operation\",\"future_field\":true}'
        WHERE id = json_extract($1, '$.id')"
            .into(),
        serde_json::json!({"id": request.id}).to_string(),
        true,
        true,
    )
    .await
    .expect("newer writer");
    let job = store
        .background_job(&request.id)
        .await
        .expect("read")
        .expect("visible");
    assert_eq!(job.payload["kind"], "future_operation");
    assert!(job.supported_payload().is_err());
    assert!(matches!(
        store
            .claim_job(claim(&request.id, 0, 1_000))
            .await
            .expect("claim"),
        ClaimOutcome::Unsupported
    ));
    assert_eq!(
        store
            .cancel_job(CancelJob {
                job_id: request.id,
                now_ms: 1_001
            })
            .await
            .expect("cancel")
            .expect("job")
            .state,
        JobState::Cancelled
    );
}

#[tokio::test]
async fn background_jobs_repeated_crashes_exhaust_budget_without_automatic_reset() {
    let store = SqliteStore::open_in_memory().expect("store");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    for crash in 0..5 {
        let job = claimed(
            &store,
            claim(&request.id, crash, 1_000 + crash * JOB_LEASE_MS),
        )
        .await;
        assert_eq!(job.failed_attempts, crash);
    }
    let now_ms = 1_000 + 5 * JOB_LEASE_MS;
    assert!(matches!(
        store
            .claim_job(claim(&request.id, 5, now_ms))
            .await
            .expect("bounded retry"),
        ClaimOutcome::Contended
    ));
    store.maintain_jobs(now_ms).await.expect("upkeep");
    let job = store
        .background_job(&request.id)
        .await
        .expect("read")
        .expect("job");
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.failed_attempts, 5);
    assert!(matches!(
        store.enqueue_job(request).await.expect("repeat request"),
        EnqueueOutcome::Existing { .. }
    ));
}

#[tokio::test]
async fn background_jobs_fragment_retry_keeps_its_window_history_and_configured_limit() {
    use crate::content_analysis::{
        IndexDiagnostic, IndexFailureCode, INDEX_RETRY_BASE_MS, INDEX_RETRY_WINDOW_MS,
    };
    use crate::store::SettingsStore;
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .put_setting(super::keys::ANALYSIS_MAX_ATTEMPTS, "2")
        .await
        .expect("policy");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    let job = claimed(&store, claim(&request.id, 0, 1_000)).await;
    assert_eq!(job.attempt_limit, 2);
    let retry = store
        .fail_fragment_job(FragmentJobFailure {
            token: job.token.expect("token"),
            code: IndexFailureCode::IndexBudgetExceeded,
            transient_allowlisted: false,
            diagnostic: IndexDiagnostic::default(),
            now_ms: 1_001,
        })
        .await
        .expect("failure");
    assert!(retry);
    let retry = store
        .background_job(&request.id)
        .await
        .expect("read")
        .expect("job");
    assert_eq!(retry.state, JobState::Queued);
    assert_eq!(retry.not_before_ms, 1_001 + INDEX_RETRY_BASE_MS);
    assert_eq!(retry.retry_deadline_ms, 1_001 + INDEX_RETRY_WINDOW_MS);
    let second = claimed(
        &store,
        claim(&request.id, retry.revision, retry.not_before_ms),
    )
    .await;
    let failed = store
        .fail_fragment_job(FragmentJobFailure {
            token: second.token.expect("token"),
            code: IndexFailureCode::IndexBudgetExceeded,
            transient_allowlisted: false,
            diagnostic: IndexDiagnostic::default(),
            now_ms: retry.not_before_ms + 1,
        })
        .await
        .expect("second failure");
    assert!(failed);
    let failed = store
        .background_job(&request.id)
        .await
        .expect("read")
        .expect("job");
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.last_error_code.as_deref(), Some("attempt_limit"));
    assert_eq!(
        failed.attempt_errors,
        "index_budget_exceeded,index_budget_exceeded"
    );
    assert_eq!(failed.retry_deadline_ms, retry.retry_deadline_ms);
    let diagnostic =
        IndexDiagnostic::decode_bounded(&failed.index_diagnostic_json).expect("bounded diagnostic");
    assert_eq!(diagnostic.claim_fence, 2);
    assert_eq!(diagnostic.attempt, 2);
    assert!(!diagnostic.retryable);
}

#[tokio::test]
async fn fragment_interests_keep_independent_budgets_when_joining_a_retry() {
    use super::SettingsStore;
    use crate::content_analysis::{IndexDiagnostic, IndexFailureCode};
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .put_setting(super::keys::ANALYSIS_MAX_ATTEMPTS, "2")
        .await
        .expect("limit");
    let first = enqueue(1_000);
    store.enqueue_job(first.clone()).await.expect("first");
    let mut now_ms = 1_000;
    for execution in 0..3 {
        let current = store
            .background_job(&first.id)
            .await
            .expect("read")
            .expect("job");
        now_ms = now_ms.max(current.not_before_ms);
        if execution == 1 {
            let later = enqueue(now_ms);
            assert!(
                matches!(store.enqueue_job(later).await.expect("join retry"),
                EnqueueOutcome::Accepted { ref job_id, .. } if job_id == &first.id)
            );
        }
        let job = claimed(&store, claim(&first.id, current.revision, now_ms)).await;
        assert!(store
            .fail_fragment_job(FragmentJobFailure {
                token: job.token.expect("token"),
                code: IndexFailureCode::IndexBudgetExceeded,
                transient_allowlisted: false,
                diagnostic: IndexDiagnostic::default(),
                now_ms: now_ms + 1,
            })
            .await
            .expect("failure"));
        let page = store
            .job_waiters(WaiterQuery {
                job_id: first.id.clone(),
                after: None,
                limit: 100,
            })
            .await
            .expect("ledgers");
        let original = page
            .waiters
            .iter()
            .find(|w| w.request_id == first.request.request_id)
            .expect("original");
        if execution == 1 {
            assert_eq!(original.state, "failed");
            assert_eq!(original.failed_attempts, 2);
            let later = page
                .waiters
                .iter()
                .find(|w| w.request_id != first.request.request_id)
                .expect("later");
            assert_eq!(later.state, "pending");
            assert_eq!(later.failed_attempts, 1);
            assert_eq!(
                store
                    .background_job(&first.id)
                    .await
                    .expect("read")
                    .expect("job")
                    .state,
                JobState::Queued
            );
        }
        if execution == 2 {
            assert!(page
                .waiters
                .iter()
                .all(|w| w.state == "failed" && w.failed_attempts == 2));
            let terminal = store
                .background_job(&first.id)
                .await
                .expect("read")
                .expect("job");
            assert_eq!(
                terminal.failed_attempts, 3,
                "execution totals are not a consumer's budget"
            );
            assert_eq!(terminal.state, JobState::Failed);
        }
    }
}

#[tokio::test]
async fn fragment_takeover_charges_only_due_interests_and_keeps_fresh_budget() {
    use super::SettingsStore;
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .put_setting(super::keys::ANALYSIS_MAX_ATTEMPTS, "1")
        .await
        .expect("limit");
    let first = enqueue(1_000);
    store.enqueue_job(first.clone()).await.expect("enqueue");
    let owner = claimed(&store, claim(&first.id, 0, 1_000)).await;
    let mut later = enqueue(2_000);
    later.not_before_ms = 32_000;
    store
        .enqueue_job(later.clone())
        .await
        .expect("future interest");
    let successor = claimed(&store, claim(&first.id, owner.revision, 32_000)).await;
    assert_eq!(successor.failed_attempts, 1);
    let interests = store
        .job_waiters(WaiterQuery {
            job_id: first.id.clone(),
            after: None,
            limit: 100,
        })
        .await
        .expect("ledgers");
    let original = interests
        .waiters
        .iter()
        .find(|w| w.request_id == first.request.request_id)
        .expect("original");
    assert_eq!(
        (original.state.as_str(), original.failed_attempts),
        ("failed", 1)
    );
    let fresh = interests
        .waiters
        .iter()
        .find(|w| w.request_id == later.request.request_id)
        .expect("fresh");
    assert_eq!(
        (fresh.state.as_str(), fresh.failed_attempts),
        ("pending", 0)
    );
    store
        .maintain_jobs(63_000)
        .await
        .expect("last abandoned owner");
    let terminal = store
        .background_job(&first.id)
        .await
        .expect("read")
        .expect("job");
    assert_eq!(terminal.state, JobState::Failed);
    let interests = store
        .job_waiters(WaiterQuery {
            job_id: first.id,
            after: None,
            limit: 100,
        })
        .await
        .expect("ledgers");
    assert!(interests
        .waiters
        .iter()
        .all(|w| w.state == "failed" && w.failed_attempts == 1));
}

#[tokio::test]
async fn fragment_cancelled_due_interest_does_not_schedule_future_interest_early() {
    let store = SqliteStore::open_in_memory().expect("store");
    let first = enqueue(1_000);
    store.enqueue_job(first.clone()).await.expect("enqueue");
    let mut later = enqueue(2_000);
    later.not_before_ms = 90_000;
    store.enqueue_job(later).await.expect("future interest");
    store
        .cancel_waiter(CancelWaiter {
            scope: first.request.scope,
            request_id: first.request.request_id,
            now_ms: 3_000,
        })
        .await
        .expect("cancel");
    let job = store
        .background_job(&first.id)
        .await
        .expect("read")
        .expect("job");
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(job.not_before_ms, 90_000);
    assert!(store
        .job_candidates(CandidateQuery {
            node_id: "node-a".into(),
            kinds: vec![JobKind::FragmentIndexBuild],
            now_ms: 4_000,
            after: None,
            limit: 100
        })
        .await
        .expect("candidates")
        .jobs
        .is_empty());
}

#[tokio::test]
async fn background_jobs_global_history_pressure_preserves_resolution_and_restores_progress() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("pressure.db");
    let store = SqliteStore::open(&path).expect("store");
    let request = enqueue(1_000);
    store.enqueue_job(request.clone()).await.expect("enqueue");
    let connection = rusqlite::Connection::open(&path).expect("fixture connection");
    // Reproduce 2,500 accepted non-expiring interests with 16 prior yields
    // each. Seed historical rows in one transaction rather than 40,000 RPCs.
    let transaction = connection.unchecked_transaction().expect("transaction");
    transaction
        .execute(
            "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2499)
        INSERT INTO background_jobs (id, kind, payload_version, payload_json, dedupe_key, priority,
            state, fence, revision, not_before_ms, created_at_ms, updated_at_ms)
        SELECT 'pressure-'||i, kind, payload_version, payload_json, 'pressure-'||i, priority,
            'queued', 16, 32, 1000, 1000, 1000 FROM n, background_jobs WHERE id=?1",
            [&request.id],
        )
        .expect("jobs");
    transaction
        .execute(
            "INSERT INTO background_job_waiters
        (request_scope, request_id, request_digest, job_id, consumer_kind, consumer_ref, priority,
         state, receipt_expires_ms, created_at_ms, updated_at_ms)
        SELECT 'internal:pressure', id, ?2, id, 'analysis', id, 1, 'pending', 604801000, 1000, 1000
        FROM background_jobs WHERE id != ?1",
            rusqlite::params![request.id, "b".repeat(64)],
        )
        .expect("interests");
    transaction
        .execute(
            "UPDATE background_jobs SET fence=16, revision=32 WHERE id=?1",
            [&request.id],
        )
        .expect("original history fence");
    transaction.execute_batch("WITH RECURSIVE n(fence) AS (VALUES(1) UNION ALL SELECT fence+1 FROM n WHERE fence<16)
        INSERT INTO background_job_attempts
        (job_id, fence, claim_id, owner_node_id, owner_boot_id, started_at_ms, resolve_until_ms, finished_at_ms, outcome)
        SELECT id, n.fence, id||':'||n.fence, 'node-a', 'old-boot', 1000, 121000, 1001, 'yielded'
        FROM background_jobs CROSS JOIN n;").expect("40,000 attempts");
    transaction.commit().expect("commit fixture");
    assert!(matches!(
        store
            .claim_job(claim(&request.id, 32, 2000))
            .await
            .expect("full history"),
        ClaimOutcome::Contended
    ));
    assert!(!store
        .maintain_jobs(120_999)
        .await
        .expect("protect reconciliation window"));
    let count = || {
        connection
            .query_row("SELECT COUNT(*) FROM background_job_attempts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count")
    };
    assert_eq!(count(), MAX_ATTEMPTS as i64);
    assert!(store
        .maintain_jobs(121_001)
        .await
        .expect("pressure cleanup"));
    assert_eq!(count(), MAX_ATTEMPTS as i64 - 128, "one bounded page");
    let newest: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM background_job_attempts WHERE fence=16",
            [],
            |row| row.get(0),
        )
        .expect("newest attempts");
    assert_eq!(newest, 2500, "each job retains its newest attempt");
    let compacted: i64 = connection
        .query_row("SELECT SUM(yield_count) FROM background_jobs", [], |row| {
            row.get(0)
        })
        .expect("compacted totals");
    assert_eq!(compacted, 128, "compaction preserves lifetime counters");
    let job = claimed(&store, claim(&request.id, 32, 121_002)).await;
    assert_eq!(job.fence, 17);
    assert_eq!(job.failed_attempts, 0);
}

#[tokio::test]
async fn background_subtitles_reconcile_historical_ready_demand_without_overwriting_history() {
    use super::{ClusterFragmentIndexStore, LibraryStore, MediaStore};
    use crate::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    let dir = tempfile::tempdir().expect("subtitle reconciliation fixture");
    let path = dir.path().join("subtitle-reconcile.db");
    let store = SqliteStore::open(&path).expect("subtitle reconciliation fixture");
    let library = store
        .create_library(&NewLibrary {
            name: "Subtitles".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/media".into()],
            anime: false,
        })
        .await
        .expect("subtitle reconciliation fixture");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Source".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("subtitle reconciliation fixture");
    let file_id = store
        .upsert_file(item, "/media/source.mkv", 100, 1, &ProbeResult::default())
        .await
        .expect("subtitle reconciliation fixture");
    let request = store
        .enqueue_or_promote_subtitle_source(
            &super::SubtitleSourceStamp {
                file_id,
                source_size: 100,
                source_mtime: 1,
                pipeline_version: "subtitle-source-v1".into(),
            },
            "foreground",
            1_000,
        )
        .await
        .expect("subtitle reconciliation fixture")
        .expect("subtitle reconciliation fixture");
    let EnqueueOutcome::Accepted { job_id, .. } = store
        .enqueue_subtitle_job(request.clone(), 1_000)
        .await
        .expect("subtitle reconciliation fixture")
    else {
        panic!("admission")
    };
    let mut claim = claim(&job_id, 0, 1_001);
    claim.kind = JobKind::SubtitleExtract;
    let ClaimOutcome::Claimed { job } = store
        .claim_artifact_job(claim)
        .await
        .expect("subtitle reconciliation fixture")
    else {
        panic!("claim")
    };
    let token = job.token.expect("subtitle reconciliation fixture");
    let query = CandidateQuery {
        node_id: "node-b".into(),
        kinds: vec![JobKind::SubtitleExtract],
        after: None,
        now_ms: token.lease_expires_ms,
        limit: 100,
    };
    // A valid abandoned owner remains reclaimable; upkeep must not discard it.
    assert!(!store
        .maintain_jobs(token.lease_expires_ms)
        .await
        .expect("subtitle reconciliation fixture"));
    assert_eq!(
        store
            .job_candidates(query.clone())
            .await
            .expect("subtitle reconciliation fixture")
            .jobs
            .len(),
        1
    );
    // Reproduce historical independent publication without modifying the common
    // lease, then prove retirement preserves both the result and its history.
    let body = serde_json::json!({"request_id":request.request_id}).to_string();
    store.queue_transaction(vec![
        ("UPDATE analysis_requests SET state='ready', result_cache_key='published-source' WHERE request_id=json_extract($1,'$.request_id')".into(), body.clone()),
        ("UPDATE analysis_attempts SET phase='published', terminal_code=NULL WHERE request_id=json_extract($1,'$.request_id')".into(), body.clone()),
    ]).await.expect("subtitle reconciliation fixture");
    // Reopen the predecessor database with its old projection trigger so this
    // also exercises the SQLite upgrade, not only a fresh bootstrap.
    drop(store);
    {
        let connection =
            rusqlite::Connection::open(&path).expect("subtitle reconciliation fixture");
        connection
            .execute_batch("DROP TRIGGER background_subtitle_settled;")
            .expect("subtitle reconciliation fixture");
        connection
            .execute_batch(super::background_jobs_subtitle::SCHEMA)
            .expect("subtitle reconciliation fixture");
        // A literal, not `SQLITE_SCHEMA_VERSION - 1`: the fixture is the v83
        // shape, and every later migration (v84's own trigger, v85's queue
        // retention) must replay from there.
        connection
            .pragma_update(None, "user_version", 83)
            .expect("subtitle reconciliation fixture");
    }
    let store = SqliteStore::open(&path).expect("subtitle reconciliation fixture");
    assert!(
        !store
            .maintain_jobs(2_000)
            .await
            .expect("subtitle reconciliation fixture"),
        "live owner retained"
    );
    assert!(store
        .job_candidates(query)
        .await
        .expect("subtitle reconciliation fixture")
        .jobs
        .is_empty());
    let mut retry = claim_for_subtitle(&job_id, job.revision, token.lease_expires_ms);
    retry.node_id = "node-b".into();
    assert!(!matches!(
        store
            .claim_artifact_job(retry)
            .await
            .expect("subtitle reconciliation fixture"),
        ClaimOutcome::Claimed { .. }
    ));
    assert!(store
        .maintain_jobs(token.lease_expires_ms)
        .await
        .expect("subtitle reconciliation fixture"));
    let retired = store
        .background_job(&job_id)
        .await
        .expect("subtitle reconciliation fixture")
        .expect("subtitle reconciliation fixture");
    assert_eq!(retired.state, JobState::Cancelled);
    assert!(retired.token.is_none());
    assert_eq!(
        store
            .job_attempts(&job_id)
            .await
            .expect("subtitle reconciliation fixture")[0]
            .outcome
            .as_deref(),
        Some("cancelled")
    );
    let preserved = store
        .analysis_request(&request.request_id)
        .await
        .expect("subtitle reconciliation fixture")
        .expect("subtitle reconciliation fixture");
    assert_eq!(preserved.state, "ready");
    assert_eq!(preserved.result_cache_key, "published-source");
    let rows = store.queue_sql("SELECT json_object('phase',phase,'terminal_code',terminal_code) AS result_json FROM analysis_attempts WHERE request_id=json_extract($1,'$.request_id')".into(), body, false, false).await.expect("subtitle reconciliation fixture");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[0])
            .expect("subtitle reconciliation fixture"),
        serde_json::json!({"phase":"published","terminal_code":null})
    );
    assert!(
        !store
            .maintain_jobs(token.lease_expires_ms + 1)
            .await
            .expect("subtitle reconciliation fixture"),
        "idle upkeep stays read-only"
    );
}

fn claim_for_subtitle(id: &str, revision: i64, now: i64) -> ClaimJob {
    let mut request = claim(id, revision, now);
    request.kind = JobKind::SubtitleExtract;
    request
}

/// The seeded settled rows are UUID-shaped, as the job id validator requires.
fn history_id(i: usize) -> String {
    format!("00000000-0000-4000-8000-{i:012}")
}

#[tokio::test]
async fn background_jobs_settled_history_yields_to_new_work_at_the_bound() {
    // 2026-09-28: one day of embedding and subtitle work settled 10,000 jobs,
    // and from then on every enqueue — library scans, Monarr's targeted
    // scans — answered QueueFull for what would have been a week, with five
    // jobs actually running. The bound is a table bound; history must yield.
    let store = SqliteStore::open_in_memory().expect("store");
    let template = enqueue(1_000);
    store.enqueue_job(template.clone()).await.expect("enqueue");
    store
        .cancel_job(CancelJob {
            job_id: template.id.clone(),
            now_ms: 1_001,
        })
        .await
        .expect("settle the template");
    async fn count(store: &SqliteStore) -> usize {
        QueueSql::queue_sql(
            store,
            "SELECT CAST(COUNT(*) AS TEXT) FROM background_jobs WHERE $1 IS NOT NULL".into(),
            "{}".into(),
            false,
            false,
        )
        .await
        .expect("count")[0]
            .parse()
            .expect("integer")
    }
    // Settled history up to the bound, oldest first: history-1 settled at
    // 2,001 ms, history-9999 at 11,999 ms — all younger than the template.
    QueueSql::queue_sql(
        &store,
        "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<9999)
        INSERT INTO background_jobs (id, kind, payload_version, payload_json, dedupe_key, priority,
            state, not_before_ms, created_at_ms, updated_at_ms)
        SELECT '00000000-0000-4000-8000-'||printf('%012d', i), kind, payload_version, payload_json, 'history-'||i, priority,
            'succeeded', 1000, 1000, 2000+i FROM n, background_jobs WHERE id = json_extract($1, '$.id')"
            .into(),
        serde_json::json!({"id": template.id}).to_string(),
        true,
        true,
    )
    .await
    .expect("seed history");
    assert_eq!(count(&store).await, MAX_RETAINED_JOBS);

    let fresh = enqueue(20_000);
    assert!(
        matches!(
            store.enqueue_job(fresh.clone()).await.expect("admit"),
            EnqueueOutcome::Accepted { .. }
        ),
        "settled history must not refuse live work"
    );
    // The 128 oldest evictable rows went: the template and history-1..127.
    assert_eq!(count(&store).await, MAX_RETAINED_JOBS - MAX_PAGE_SIZE + 1);
    assert!(store
        .background_job(&template.id)
        .await
        .expect("read")
        .is_none());
    assert!(store
        .background_job(&history_id(127))
        .await
        .expect("read")
        .is_none());
    assert!(store
        .background_job(&history_id(128))
        .await
        .expect("read")
        .is_some());
    assert!(store
        .background_job(&fresh.id)
        .await
        .expect("read")
        .is_some());

    // Upkeep drains history from the 9,000 watermark, 128 per pass, then
    // goes idle without a consensus write once it is below.
    assert!(store.maintain_jobs(20_001).await.expect("pressure upkeep"));
    assert_eq!(
        count(&store).await,
        MAX_RETAINED_JOBS - 2 * MAX_PAGE_SIZE + 1
    );
    let mut passes = 0;
    while store.maintain_jobs(20_002 + passes).await.expect("upkeep") {
        passes += 1;
        assert!(passes < 16, "upkeep must converge below the watermark");
    }
    let settled_below_watermark = count(&store).await;
    assert!(settled_below_watermark < 9_000);
    assert!(settled_below_watermark >= 9_000 - MAX_PAGE_SIZE);
    assert!(store
        .background_job(&fresh.id)
        .await
        .expect("read")
        .is_some());
    assert!(!store.maintain_jobs(30_000).await.expect("idle"));
}

#[tokio::test]
async fn scheduled_library_ticks_share_only_pending_equivalent_intents() {
    use super::background_jobs_library::{LibraryTrigger, LibraryWorkInput};
    use super::LibraryStore;
    use crate::domain::{LibraryKind, NewLibrary};

    let store = SqliteStore::open_in_memory().expect("store");
    let library = store
        .create_library(&NewLibrary {
            name: "DVR".into(),
            kind: LibraryKind::Recordings,
            paths: vec!["/dvr".into()],
            anime: false,
        })
        .await
        .expect("library");
    for refresh in [false, true] {
        let input = NewLibraryWork {
            request_id: uuid::Uuid::new_v4().to_string(),
            library_id: library.id,
            input: LibraryWorkInput::Full {
                refresh,
                trigger: LibraryTrigger::Scheduled,
            },
            now_ms: 1_000,
        };
        let EnqueueOutcome::Accepted { job_id, .. } = store
            .enqueue_library_work(input.clone())
            .await
            .expect("first tick")
        else {
            panic!("first tick must queue work")
        };
        let mut next = input.clone();
        next.request_id = uuid::Uuid::new_v4().to_string();
        next.now_ms += 60_000;
        let mut peer = next.clone();
        peer.request_id = uuid::Uuid::new_v4().to_string();
        let (one, two) = tokio::join!(
            store.enqueue_library_work(next.clone()),
            store.enqueue_library_work(peer)
        );
        for outcome in [one, two] {
            assert!(
                matches!(outcome.expect("competing ticks"), EnqueueOutcome::Existing { job_id: id, .. } if id == job_id)
            );
        }
        let pending = store
            .library_work_requests(LibraryWorkQuery {
                job_id: Some(job_id.clone()),
                pending_only: true,
                limit: 256,
                ..Default::default()
            })
            .await
            .expect("pending");
        assert_eq!(pending.len(), 1, "ticks must not accumulate interests");
        assert_eq!(pending[0].request_id, input.request_id);

        let mut manual = next.clone();
        manual.request_id = uuid::Uuid::new_v4().to_string();
        manual.input = LibraryWorkInput::Full {
            refresh,
            trigger: LibraryTrigger::Manual,
        };
        assert!(matches!(
            store.enqueue_library_work(manual).await.expect("manual"),
            EnqueueOutcome::Accepted { .. }
        ));
        let pending = store
            .library_work_requests(LibraryWorkQuery {
                job_id: Some(job_id.clone()),
                pending_only: true,
                limit: 256,
                ..Default::default()
            })
            .await
            .expect("manual identity");
        assert_eq!(pending.len(), 2, "manual intent remains separate");

        let mut conflicting = input.clone();
        conflicting.input = LibraryWorkInput::Full {
            refresh: !refresh,
            trigger: LibraryTrigger::Scheduled,
        };
        assert!(matches!(
            store
                .enqueue_library_work(conflicting)
                .await
                .expect("original identity"),
            EnqueueOutcome::Conflict
        ));

        store
            .cancel_job(CancelJob {
                job_id: job_id.clone(),
                now_ms: next.now_ms + 1,
            })
            .await
            .expect("cancel queued job");
        next.now_ms += 2;
        assert!(
            matches!(store.enqueue_library_work(next).await.expect("later tick"), EnqueueOutcome::Accepted { job_id: id, .. } if id != job_id),
            "terminal scheduled receipts cannot suppress a future run"
        );
    }
}

#[test]
fn receipt_pressure_literals_match_constants() {
    let migration = super::background_jobs::RECEIPT_PRESSURE_SCHEMA;
    let needed = super::background_jobs_maintenance::MAINTENANCE_NEEDED;
    for sql in [migration, needed] {
        assert!(
            sql.contains(&format!(
                "(SELECT COUNT(*) FROM background_job_waiters) >= {WAITERS_PRESSURE}"
            )),
            "waiter pressure literal drifted from WAITERS_PRESSURE"
        );
    }
    assert!(ENQUEUE_SQL.contains(&format!(
        "(SELECT COUNT(*) FROM background_job_waiters) >= {MAX_WAITERS}"
    )));
    assert_eq!(WAITERS_PRESSURE, 15_360);
    // v86 replaces v85's maintenance trigger, so it must carry every
    // statement of that trigger; and it must be the last migration that
    // creates the trigger.
    let trigger = |source: &str| {
        let start = source
            .find("CREATE TRIGGER IF NOT EXISTS background_job_maintenance_command\n")
            .expect("maintenance trigger");
        let end = source[start..].find("\nEND;\n").expect("trigger end") + start;
        source[start..end].to_owned()
    };
    let (previous, current) = (
        trigger(super::background_jobs::RETENTION_SCHEMA),
        trigger(migration),
    );
    for line in previous
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        assert!(
            current.contains(line),
            "v85 upkeep statement missing from v86: {line}"
        );
    }
    let creators: Vec<usize> = super::sqlite::MIGRATIONS
        .iter()
        .enumerate()
        .filter(|(_, sql)| {
            sql.contains("CREATE TRIGGER IF NOT EXISTS background_job_maintenance_command")
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        creators.len(),
        3,
        "another migration redefines the maintenance trigger"
    );
    assert!(std::ptr::eq(
        super::sqlite::MIGRATIONS[creators[2]],
        migration
    ));
    assert_eq!(creators[2], super::sqlite::MIGRATIONS.len() - 1);
}

/// Internal terminal receipts compact under waiter pressure, oldest first;
/// user-scoped receipts, identity-retaining receipts and receipts of jobs
/// that are still active keep their full window.
#[tokio::test]
async fn waiter_pressure_compacts_internal_receipts_and_spares_protected_ones() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("waiters.db");
    let store = SqliteStore::open(&path).expect("store");
    let template = enqueue(1_000);
    store.enqueue_job(template.clone()).await.expect("template");
    let connection = rusqlite::Connection::open(&path).expect("fixture connection");
    let transaction = connection.unchecked_transaction().expect("transaction");
    // One active job whose old cancelled receipt must survive, plus one
    // terminal job carrying every other receipt.
    transaction
        .execute(
            "INSERT INTO background_jobs (id, kind, payload_version, payload_json, dedupe_key, priority,
            state, fence, revision, not_before_ms, created_at_ms, updated_at_ms)
        SELECT 'terminal', kind, payload_version, payload_json, 'terminal', priority,
            'succeeded', 1, 2, 1000, 1000, 1000 FROM background_jobs WHERE id=?1",
            [&template.id],
        )
        .expect("terminal job");
    let seed = |scope: &str,
                prefix: &str,
                job: &str,
                state: &str,
                retain: i64,
                n: i64,
                base: i64| {
        transaction
            .execute(
                "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<?1)
            INSERT INTO background_job_waiters
            (request_scope, request_id, request_digest, job_id, consumer_kind, consumer_ref, priority,
             state, retain_identity, receipt_expires_ms, created_at_ms, updated_at_ms)
            SELECT ?2, ?3||printf('%05d', i), ?4, ?5, 'analysis', ?3||i, 1, ?6, ?7,
                ?8 + i + 604800000, ?8 + i, ?8 + i FROM n",
                rusqlite::params![n, scope, prefix, "b".repeat(64), job, state, retain, base],
            )
            .expect("receipts");
    };
    // Oldest of all: 8 user receipts, 8 identity-retaining receipts and 8
    // receipts of the still-active template job — all protected.
    seed("user:7", "user-", "terminal", "succeeded", 0, 8, 0);
    seed("analysis", "keep-", "terminal", "succeeded", 1, 8, 100);
    seed("subtitle", "live-", &template.id, "cancelled", 0, 8, 200);
    // Then enough internal terminal receipts to reach the cap exactly.
    let internal = MAX_WAITERS as i64 - 1 - 24;
    seed(
        "semantic",
        "done-",
        "terminal",
        "succeeded",
        0,
        internal,
        1_000,
    );
    transaction.commit().expect("commit fixture");
    let count = |sql: &str| {
        connection
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .expect("count")
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM background_job_waiters"),
        MAX_WAITERS as i64
    );

    let mut fresh = enqueue(5_000);
    fresh.dedupe_key = "fragment:2".to_owned();
    assert!(matches!(
        store.enqueue_job(fresh.clone()).await.expect("closed"),
        EnqueueOutcome::QueueFull
    ));
    assert!(store.maintain_jobs(5_001).await.expect("pressure upkeep"));
    assert_eq!(
        count("SELECT COUNT(*) FROM background_job_waiters"),
        MAX_WAITERS as i64 - MAX_PAGE_SIZE as i64,
        "one bounded page"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM background_job_waiters WHERE request_scope = 'user:7'"),
        8
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM background_job_waiters WHERE request_id LIKE 'keep-%'"),
        8
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM background_job_waiters WHERE request_id LIKE 'live-%'"),
        8
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM background_job_waiters WHERE request_id LIKE 'done-%' AND request_id <= 'done-00128'"),
        0,
        "the oldest internal receipts went first"
    );
    fresh.now_ms = 5_002;
    fresh.not_before_ms = 5_002;
    assert!(matches!(
        store.enqueue_job(fresh).await.expect("reopened"),
        EnqueueOutcome::Accepted { .. }
    ));
    let mut remaining = count("SELECT COUNT(*) FROM background_job_waiters");
    let mut ticks = 0;
    while remaining >= WAITERS_PRESSURE as i64 {
        assert!(store.maintain_jobs(5_010 + ticks).await.expect("paging"));
        let next = count("SELECT COUNT(*) FROM background_job_waiters");
        assert!(next < remaining);
        remaining = next;
        ticks += 1;
        assert!(ticks < 16, "pressure paging must converge");
    }
    assert!(!store.maintain_jobs(5_100).await.expect("settled"));
}
