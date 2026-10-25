// Uses the parent module's actual SQLite claim/heartbeat fixtures. These are
// logical job/application lifetime tests, never native-close qualification.

#[tokio::test]
async fn encoded_outer_payloads_drop_before_original_controller_and_store_close_receipt() {
    let (store, id, active) = active().await;
    let job = store.background_job(&id).await.unwrap().unwrap();
    let original = job.token.clone().unwrap();
    let controller = BackgroundJobController::new(job, active);
    let fence = controller.fence();
    assert!(!fence.outer_payloads_dropped());
    let bytes = fence.controller_requested_heap_bytes().await.unwrap();
    assert!(bytes >= std::mem::size_of::<Inner>());
    let receipt = controller.finish().await;
    assert!(
        fence.outer_payloads_dropped(),
        "actual rich values precede the marker"
    );
    assert!(receipt.closes_original(&original));
    assert_eq!(
        store.background_job(&id).await.unwrap().unwrap().state,
        JobState::Queued
    );
    assert!(fence.0.heartbeat.lock().await.task.is_none());
}

#[derive(Default)]
struct BlockedRenewal {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[plurx_core::cluster::coordination::cluster_job_async_trait]
impl ClusterJobAuthority for BlockedRenewal {
    async fn may_run_cluster_jobs(&self) -> bool {
        true
    }
    async fn may_execute_job(&self, _: JobKind) -> bool {
        self.entered.notify_one();
        self.release.notified().await;
        true
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_original_controller_join_keeps_the_same_pending_heartbeat_handle() {
    let authority = Arc::new(BlockedRenewal::default());
    let (store, id, active) = active_with_authority(None, authority.clone()).await;
    let job = store.background_job(&id).await.unwrap().unwrap();
    let original = job.token.clone().unwrap();
    let controller = BackgroundJobController::new(job, active);
    let fence = controller.fence();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(JOB_RENEW_INTERVAL_MS as u64)).await;
    authority.entered.notified().await;
    drop(controller);
    assert!(fence.outer_payloads_dropped());
    assert!(
        tokio::time::timeout(Duration::from_millis(1), fence.finish_original_controller())
            .await
            .is_err()
    );
    {
        let retained = fence.0.heartbeat.lock().await;
        assert!(
            retained.task.is_some(),
            "cancelled join did not detach original task"
        );
        assert!(!retained.failed);
    }
    authority.release.notify_one();
    let receipt = fence.finish_original_controller().await;
    assert!(receipt.closes_original(&original));
    assert!(fence.0.heartbeat.lock().await.task.is_none());
}

#[tokio::test]
async fn failed_original_heartbeat_join_remains_unknown_on_every_retry() {
    let (store, id, active) = active().await;
    let job = store.background_job(&id).await.unwrap().unwrap();
    let original = job.token.clone().unwrap();
    let controller = BackgroundJobController::new(job, active);
    let fence = controller.fence();
    fence
        .0
        .heartbeat
        .lock()
        .await
        .task
        .as_ref()
        .unwrap()
        .abort();
    drop(controller);
    assert!(fence.outer_payloads_dropped());
    assert!(!fence
        .finish_original_controller()
        .await
        .closes_original(&original));
    assert!(!fence
        .finish_original_controller()
        .await
        .closes_original(&original));
    assert!(fence.0.heartbeat.lock().await.failed);
    assert_eq!(
        store.background_job(&id).await.unwrap().unwrap().state,
        JobState::Running
    );
}

#[tokio::test]
async fn lost_store_close_reply_reconciles_only_the_same_original_attempt() {
    let (store, id, active) = active().await;
    let job = store.background_job(&id).await.unwrap().unwrap();
    let original = job.token.clone().unwrap();
    let controller = BackgroundJobController::new(job, active);
    let fence = controller.fence();
    let (result, dropped) = faults::dropping(
        faults::SETTLE,
        fence.settle(JobSettlement::Yield {
            error_code: Some("fixture_yield".into()),
            checkpoint: None,
            not_before_ms: unix_ms().unwrap(),
        }),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(dropped, faults::SETTLE);
    assert!(
        !fence.0.state.lock().await.logical_closed,
        "lost RPC reply is unknown locally"
    );
    assert!(
        fence.0.state.lock().await.token.is_some(),
        "same original token remains resumable"
    );
    drop(controller);
    let receipt = fence.finish_original_controller().await;
    assert!(
        receipt.closes_original(&original),
        "exact Store resolution supplies logical Close"
    );
    assert_eq!(
        store.background_job(&id).await.unwrap().unwrap().state,
        JobState::Queued
    );
    let mut another = original;
    another.claim_id = uuid::Uuid::new_v4().to_string();
    assert!(!receipt.closes_original(&another));
}

#[tokio::test]
async fn successor_claim_or_pruned_authority_never_becomes_original_store_close() {
    let (store, id, active) = active().await;
    let job = store.background_job(&id).await.unwrap().unwrap();
    let original = job.token.clone().unwrap();
    let next_ms = original.lease_expires_ms + 1;
    let replacement = claim_with_resolution(
        store.as_ref(),
        &job,
        ClaimJob {
            job_id: id.clone(),
            expected_revision: job.revision,
            node_id: "successor-node".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::FragmentIndexBuild,
            payload_version: 1,
            now_ms: next_ms,
            dispatched_at_ms: next_ms,
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(
        replacement.0.token.as_ref().unwrap().claim_id,
        original.claim_id
    );
    let controller = BackgroundJobController::new(job, active);
    let fence = controller.fence();
    drop(controller);
    let receipt = fence.finish_original_controller().await;
    assert!(
        !receipt.closes_original(&original),
        "loss of ownership is not a Close receipt"
    );
    assert!(fence.outer_payloads_dropped());
    assert!(
        fence.0.heartbeat.lock().await.task.is_none(),
        "join alone is insufficient"
    );
}
