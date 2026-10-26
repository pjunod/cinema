//! Real SQLite request/attempt transactions through the production coordinator.
//! No worker or native graph is registered: the absent-owner cancellation path
//! may Fence, but these tests never invent a worker Close or native receipt.
use super::*;
use plurx_core::{
    domain::MediaSessionRequestClaim,
    playback_principal::PlaybackPrincipal,
    store::adaptive_resource_attempts::{
        AttemptKey, PreparedAttempt, RequestKey, WinnerSelection, WorkerBinding,
    },
};
use uuid::Uuid;

async fn request(state: &AppState) -> (AdaptiveOriginContext, AttemptClaim) {
    let user = state
        .store
        .create_user(
            &format!("retry-{}", Uuid::new_v4()),
            "unused-fixture-hash",
            false,
        )
        .await
        .expect("actual local user");
    let now = unix_ms();
    let key = RequestKey {
        request_id: format!("retry-{}", Uuid::new_v4()),
        request_incarnation_id: Uuid::new_v4(),
    };
    let binding = RequestBinding {
        version: 1,
        key: key.clone(),
        principal: PlaybackPrincipal::LocalUser { user_id: user.id },
        request_fingerprint: "a".repeat(64),
        playback_id: Uuid::new_v4().to_string(),
        recovery_epoch: Uuid::new_v4(),
        planning_generation: 7,
        planning_source_digest: "b".repeat(64),
        settings_digest: "c".repeat(64),
        intent_digest: "d".repeat(64),
        original_deadline_ms: now + 60_000,
        expected_predecessor_incarnation_id: None,
        expected_predecessor_owner_epoch: None,
        expected_desired_revision: None,
    };
    assert!(matches!(
        state
            .store
            .claim_media_session_request(
                &binding.principal,
                &key.request_id,
                &binding.request_fingerprint,
                &binding.playback_id,
                &key.request_incarnation_id.to_string(),
                now,
                binding.original_deadline_ms
            )
            .await
            .expect("actual root request"),
        MediaSessionRequestClaim::Acquired { .. }
    ));
    let claim = AttemptClaim {
        context: MutationContext {
            operation_id: Uuid::new_v4(),
            expected_request_sequence: 0,
            expected_attempt_sequence: 0,
            now_ms: now,
        },
        binding: binding.clone(),
        key: AttemptKey {
            request: key,
            attempt_id: Uuid::new_v4(),
            route_incarnation_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            worker_node_id: state.node_id.clone(),
            owner_epoch: 1,
            ordinal: 1,
        },
        worker_instance_id: Uuid::new_v4(),
        serving_generation: 1,
        domains: vec![state.node_id.clone()],
    };
    (AdaptiveOriginContext::new(binding), claim)
}
fn candidate(original: &AttemptClaim, worker: &str) -> AttemptClaim {
    let mut claim = original.clone();
    claim.context.operation_id = Uuid::new_v4();
    claim.key.attempt_id = Uuid::new_v4();
    claim.key.route_incarnation_id = Uuid::new_v4();
    claim.key.session_id = Uuid::new_v4();
    claim.key.worker_node_id = worker.to_owned();
    claim.worker_instance_id = Uuid::new_v4();
    claim.domains = vec![worker.to_owned()];
    claim
}
async fn claim(
    state: &AppState,
    context: &mut AdaptiveOriginContext,
    claim: AttemptClaim,
    deadline: tokio::time::Instant,
) -> AttemptClaim {
    match claim_original(state, context, claim, deadline).await {
        Ok(claim) => claim,
        Err(_) => panic!("production coordinator must commit the exact original claim"),
    }
}

#[tokio::test]
async fn original_retry_requires_durable_fence_then_reuses_binding_deadline_and_fresh_ordinal() {
    let dir = crate::test_tempdir().expect("state dir");
    let fixture = crate::transcode::HlsDeliveryFixture::publish_without_process(
        dir.path(),
        &Uuid::new_v4().to_string(),
    )
    .await;
    let state = &fixture.state;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let (mut context, first) = request(state).await;
    let first = claim(state, &mut context, first, deadline).await;
    let binding = first.binding.clone();
    let before = current_status(state, &binding, deadline)
        .await
        .expect("status")
        .expect("original row");
    assert_eq!(before.attempts.len(), 1);
    assert_eq!(before.attempts[0].phase, AttemptPhase::Provisional);
    assert!(
        matches!(
            claim_original(
                state,
                &mut context,
                candidate(&first, "second-worker"),
                deadline
            )
            .await,
            Err(AdaptiveOriginFailure::Hold(_))
        ),
        "a second native candidate needs a durable predecessor fence"
    );
    assert_eq!(
        current_status(state, &binding, deadline)
            .await
            .expect("status"),
        Some(before)
    );
    // No original worker exists in the real registry, so cancellation must
    // fall back to the SAME retained Store Fence and leave logical cleanup debt.
    fence_original(state, &mut context, &first, deadline)
        .await
        .expect("original absent-worker Fence");
    let fenced_status = current_status(state, &binding, deadline)
        .await
        .expect("status")
        .expect("fenced row");
    assert_eq!(fenced_status.attempts[0].phase, AttemptPhase::Cleanup);
    let fence_id = context.attempts[0]
        .fence
        .as_ref()
        .expect("retained Fence")
        .context
        .operation_id;
    assert_eq!(fenced_status.attempts[0].last_operation_id, fence_id);
    fence_original(state, &mut context, &first, deadline)
        .await
        .expect("same confirmed Fence");
    assert_eq!(
        current_status(state, &binding, deadline)
            .await
            .expect("status"),
        Some(fenced_status.clone())
    );
    let second = claim(
        state,
        &mut context,
        candidate(&first, "second-worker"),
        deadline,
    )
    .await;
    assert_eq!(second.binding, binding);
    assert_eq!(
        second.context.expected_request_sequence,
        fenced_status.sequence
    );
    assert_eq!(second.context.expected_attempt_sequence, 0);
    assert_eq!(second.key.ordinal, fenced_status.next_ordinal);
    assert_eq!(second.key.owner_epoch, i64::from(second.key.ordinal));
    assert_ne!(first.key.attempt_id, second.key.attempt_id);
    assert_ne!(
        first.key.route_incarnation_id,
        second.key.route_incarnation_id
    );
    assert_ne!(first.key.session_id, second.key.session_id);
    let before_skip = current_status(state, &binding, deadline)
        .await
        .expect("status");
    assert!(
        matches!(
            claim_original(
                state,
                &mut context,
                candidate(&first, &state.node_id),
                deadline
            )
            .await,
            Err(AdaptiveOriginFailure::NextWorker(_))
        ),
        "already-used worker is skipped without an ordinal"
    );
    assert_eq!(
        current_status(state, &binding, deadline)
            .await
            .expect("status"),
        before_skip
    );
}

#[tokio::test]
async fn original_retry_cannot_claim_or_fence_after_actual_store_winner() {
    let dir = crate::test_tempdir().expect("state dir");
    let fixture = crate::transcode::HlsDeliveryFixture::publish_without_process(
        dir.path(),
        &Uuid::new_v4().to_string(),
    )
    .await;
    let state = &fixture.state;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let (mut context, first) = request(state).await;
    let first = claim(state, &mut context, first, deadline).await;
    let status = current_status(state, &first.binding, deadline)
        .await
        .expect("status")
        .expect("claim");
    // This is only the Store protocol's selected dormant-worker binding;
    // it creates no native owner and is never used as a physical receipt.
    let worker = WorkerBinding {
        node_id: first.key.worker_node_id.clone(),
        worker_instance_id: first.worker_instance_id,
        serving_generation: first.serving_generation,
        planning_generation: first.binding.planning_generation,
        planning_source_digest: first.binding.planning_source_digest.clone(),
        selected_recipe_digest: "e".repeat(64),
        prepared_handle_digest: "f".repeat(64),
        output_digest: "0".repeat(64),
        prepared_expires_at_ms: first.binding.original_deadline_ms - 1,
    };
    let prepared = PreparedAttempt {
        context: MutationContext {
            operation_id: Uuid::new_v4(),
            expected_request_sequence: status.sequence,
            expected_attempt_sequence: status.attempts[0].sequence,
            now_ms: unix_ms(),
        },
        binding: first.binding.clone(),
        key: first.key.clone(),
        worker: worker.clone(),
    };
    let sealed = match state
        .store
        .record_adaptive_prepared(&prepared)
        .await
        .expect("actual Prepared transaction")
    {
        AttemptMutation::Applied(status) => status,
        other => panic!("Prepared refused: {other:?}"),
    };
    let selection = WinnerSelection {
        context: MutationContext {
            operation_id: Uuid::new_v4(),
            expected_request_sequence: sealed.sequence,
            expected_attempt_sequence: sealed.attempts[0].sequence,
            now_ms: unix_ms(),
        },
        binding: first.binding.clone(),
        key: first.key.clone(),
        worker,
    };
    assert!(matches!(
        state
            .store
            .select_adaptive_winner(&selection)
            .await
            .expect("actual Winner"),
        AttemptMutation::Applied(_)
    ));
    let winner = current_status(state, &first.binding, deadline)
        .await
        .expect("status");
    assert!(matches!(
        claim_original(
            state,
            &mut context,
            candidate(&first, "second-worker"),
            deadline
        )
        .await,
        Err(AdaptiveOriginFailure::Hold(_))
    ));
    assert!(fence_original(state, &mut context, &first, deadline)
        .await
        .is_err());
    assert_eq!(
        current_status(state, &first.binding, deadline)
            .await
            .expect("status"),
        winner
    );
    assert!(
        context.attempts[0].fence.is_none(),
        "winner never creates a retry Fence"
    );
}
