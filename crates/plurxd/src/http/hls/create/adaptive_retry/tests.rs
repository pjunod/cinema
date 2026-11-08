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

#[test]
fn explicit_prepare_auth_and_protocol_refusals_hold_instead_of_retrying_as_capacity() {
    for status in [400, 401, 403, 404, 405, 413, 415, 422, 428] {
        assert!(
            matches!(
                prepare_http_refusal(reqwest::StatusCode::from_u16(status).expect("status")),
                AdaptivePrepareFailure::Hold(_)
            ),
            "explicit auth/protocol status {status} must hold"
        );
    }
    for status in [409, 429, 500, 503] {
        assert!(matches!(prepare_http_refusal(reqwest::StatusCode::from_u16(status).expect("status")),
            AdaptivePrepareFailure::Retry(_)), "ambiguous capacity/ownership status {status} still requires the original Fence before retry");
    }
}

#[test]
fn original_fence_budget_counts_unknown_worker_offer_and_all_stale_reply_traces() {
    // Exhaust bounded success/unknown-vs-definite-stale reply histories using
    // the SAME admission counter guarding every production Store Fence call.
    for offered in [false, true] {
        for replies in 0u8..128 {
            let mut budget = FenceApplications::new(offered);
            let mut applications = [usize::from(offered), 0];
            let mut operation = 0;
            for step in 0..7 {
                if !budget.take() {
                    break;
                }
                applications[operation] += 1;
                assert!(
                    applications[operation] <= 2,
                    "worker offer plus direct calls exceeds the original operation budget"
                );
                if replies & (1 << step) != 0 {
                    if !budget.correct_after_definite_stale() {
                        break;
                    }
                    operation += 1;
                    assert!(
                        operation <= 1,
                        "only one definite stale correction is allowed"
                    );
                }
            }
            while budget.take() {
                applications[operation] += 1;
                assert!(applications[operation] <= 2);
            }
            assert!(!budget.take());
            if budget.corrected {
                assert!(!budget.correct_after_definite_stale());
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn remote_candidate_with_unknown_cluster_clock_is_skipped_before_claim() {
    use crate::media_pool::{remote_placement_count, CreateStartupBudget, RemotePlacementOutcome};
    // Replicated, roster never proved: the grant-time policy is Unknown, as
    // with one voter down or just after a restart.
    let unproved = Arc::new(plurx_core::cluster::clock::ClusterClockGuard::new(true));
    let budget = CreateStartupBudget::new_with_clock(5_000, &unproved);
    let deadline = budget.deadline;
    budget
        .scope(async {
            let before = remote_placement_count(RemotePlacementOutcome::ClockSkipped);
            assert!(
                remote_candidate_refusal("node-a", "node-a", deadline).is_none(),
                "local placement never needs a cross-node grant"
            );
            let Some(ApiError::ServiceUnavailable(reason)) =
                remote_candidate_refusal("node-a", "node-b", deadline)
            else {
                panic!("an ungrantable peer is passed over before Claim");
            };
            assert!(
                reason.contains("node-b") && reason.contains("cluster clock"),
                "the refusal names the cluster clock: {reason}"
            );
            assert!(remote_placement_count(RemotePlacementOutcome::ClockSkipped) > before);
        })
        .await;
    let standalone = Arc::new(plurx_core::cluster::clock::ClusterClockGuard::new(false));
    let budget = CreateStartupBudget::new_with_clock(5_000, &standalone);
    let deadline = budget.deadline;
    budget
        .scope(async {
            assert!(remote_candidate_refusal("node-a", "node-b", deadline).is_none());
        })
        .await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn grant_time_clock_refusal_retries_the_next_candidate_and_other_refusals_keep_their_class() {
    use crate::media_pool::RemoteGrantRefusal;
    use axum::response::IntoResponse;
    use plurx_core::cluster::clock::ClockRefusal;
    assert!(matches!(
        original_grant_refusal(RemoteGrantRefusal::ClusterClock(ClockRefusal::Unknown)),
        AdaptivePrepareFailure::Retry(_)
    ));
    assert!(matches!(
        original_grant_refusal(RemoteGrantRefusal::Spent),
        AdaptivePrepareFailure::Hold(_)
    ));
    assert!(matches!(
        original_grant_refusal(RemoteGrantRefusal::NoOriginalCutoff),
        AdaptivePrepareFailure::Hold(_)
    ));
    // The worker's own typed answer, as its Grant handler renders it.
    let worker = ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        crate::media_sessions::ADAPTIVE_GRANT_CLOCK_REFUSED,
        "cluster_clock: refused",
    )
    .into_response();
    let body = axum::body::to_bytes(worker.into_body(), 4096)
        .await
        .expect("typed worker body");
    let AdaptivePrepareFailure::Retry(ApiError::ServiceUnavailable(reason)) =
        grant_http_refusal("node-b", reqwest::StatusCode::SERVICE_UNAVAILABLE, &body)
    else {
        panic!("a worker's cluster-clock grant refusal retries the next candidate");
    };
    assert!(reason.contains("node-b") && reason.contains("cluster clock"));
    assert!(matches!(
        grant_http_refusal("node-b", reqwest::StatusCode::SERVICE_UNAVAILABLE, b""),
        AdaptivePrepareFailure::Retry(_)
    ));
    assert!(matches!(
        grant_http_refusal("node-b", reqwest::StatusCode::PRECONDITION_REQUIRED, &body),
        AdaptivePrepareFailure::Hold(_)
    ));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn trial_policy_refusal_keeps_local_and_remote_reason_without_inventing_evidence() {
    use axum::response::IntoResponse;
    use plurx_core::resource_policy::TrialRefusal;
    let refusal = crate::transcode::adaptive_attempts::AdaptivePrepareError {
        message: "actual finite capacity refused".into(),
        trial_refusal: Some(TrialRefusal::UnknownMemory),
    };
    let AdaptivePrepareFailure::Retry(local) = local_prepare_refusal(refusal) else {
        panic!("known local policy refusal is a retry candidate, pending cleanup");
    };
    assert_eq!(
        local.code(),
        Some(crate::media_sessions::ADAPTIVE_TRIAL_REFUSED)
    );
    let body = axum::body::to_bytes(local.into_response().into_body(), 1024)
        .await
        .expect("bounded policy reply");
    let decoded = crate::media_sessions::AdaptiveTrialRefusalV1::decode(&body)
        .expect("strict worker version1 reply");
    assert_eq!(decoded.reason, TrialRefusal::UnknownMemory);
    let AdaptivePrepareFailure::Retry(remote) =
        grant_http_refusal("node-b", reqwest::StatusCode::SERVICE_UNAVAILABLE, &body)
    else {
        panic!("known remote policy refusal is a retry candidate");
    };
    assert_eq!(
        remote.code(),
        Some(crate::media_sessions::ADAPTIVE_TRIAL_REFUSED)
    );
    assert!(matches!(
        grant_http_refusal("node-b", reqwest::StatusCode::UNAUTHORIZED, &body),
        AdaptivePrepareFailure::Hold(_)
    ));

    let valid: serde_json::Value = serde_json::from_slice(&body).expect("worker JSON");
    for (key, value) in [
        ("version", serde_json::json!(2)),
        ("reason", serde_json::json!("unknown_future_reason")),
        ("code", serde_json::json!("unknown_future_code")),
        ("extra", serde_json::json!(true)),
    ] {
        let mut malformed = valid.clone();
        malformed[key] = value;
        let body = serde_json::to_vec(&malformed).expect("test wire");
        assert!(crate::media_sessions::AdaptiveTrialRefusalV1::decode(&body).is_none());
        let refusal = grant_http_refusal("node-b", reqwest::StatusCode::SERVICE_UNAVAILABLE, &body);
        if key == "code" {
            assert!(
                matches!(refusal, AdaptivePrepareFailure::Retry(error) if error.code().is_none()),
                "generic 503 without a policy code keeps its previous classification"
            );
        } else {
            assert!(
                matches!(refusal, AdaptivePrepareFailure::Hold(error) if error.code().is_none()),
                "advertised but invalid policy evidence holds the original owner"
            );
        }
    }
    for missing in ["version", "code", "reason"] {
        let mut malformed = valid.clone();
        malformed.as_object_mut().expect("object").remove(missing);
        let body = serde_json::to_vec(&malformed).expect("missing field wire");
        assert!(crate::media_sessions::AdaptiveTrialRefusalV1::decode(&body).is_none());
        if missing != "code" {
            assert!(matches!(
                grant_http_refusal("node-b", reqwest::StatusCode::SERVICE_UNAVAILABLE, &body),
                AdaptivePrepareFailure::Hold(_)
            ));
        }
    }
    assert!(crate::media_sessions::AdaptiveTrialRefusalV1::decode(
        br#"{"version":1,"code":"adaptive_trial_refused","message":"refused","reason":"unknown_memory","reason":"unknown_bounds"}"#,
    ).is_none(), "duplicate evidence fields are malformed");
    for body in [b"".as_slice(), b"{}", b"{", &[b' '; 1025]] {
        assert!(crate::media_sessions::AdaptiveTrialRefusalV1::decode(body).is_none());
    }
    for body in [
        br#"{"code":"adaptive_trial_refused","version":2}"#.as_slice(),
        br#"{"code":"adaptive_trial_refused","reason":"unknown_memory""#,
        br#"{"code":"adaptive_\u0074rial_refused","version":2}"#,
        br#"{"code":"adaptive_\u0074rial_refused","reason":"#,
        br#"{"code":"adaptive_\u0074rial_refused","code":"generic"}"#,
        br#"{"code":"generic","code":"adaptive_\u0074rial_refused"}"#,
        &[b' '; 1025],
    ] {
        assert!(
            matches!(grant_http_refusal("node-b", reqwest::StatusCode::SERVICE_UNAVAILABLE, body),
            AdaptivePrepareFailure::Hold(error) if error.code().is_none()),
            "invalid/oversized advertised policy reply is protocol uncertainty"
        );
    }
    for body in [
        br#"{"error":"generic unavailable"}"#.as_slice(),
        br#"{"code":"generic","error":"unavailable"}"#,
        br#"{"code":42,"error":"unavailable"}"#,
    ] {
        assert!(
            matches!(grant_http_refusal("node-b", reqwest::StatusCode::SERVICE_UNAVAILABLE, body),
            AdaptivePrepareFailure::Retry(error) if error.code().is_none()),
            "valid unadvertised generic JSON preserves its existing retry classification"
        );
    }
    let generic = local_prepare_refusal("ordinary preparation failure".into());
    assert!(matches!(generic, AdaptivePrepareFailure::Retry(error) if error.code().is_none()));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn trial_policy_retry_holds_cleanup_debt_and_requires_exact_original_closed_identity() {
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
    let original_binding = first.binding.clone();
    assert!(!policy_refusal_retry_ready(state, &first, deadline).await);
    fence_original(state, &mut context, &first, deadline)
        .await
        .expect("logical fence");
    assert!(
        !policy_refusal_retry_ready(
            state,
            &first,
            tokio::time::Instant::now() + Duration::from_millis(120)
        )
        .await,
        "absent original worker and logical Cleanup never certify native closure"
    );
    assert!(
        !policy_refusal_retry_ready(state, &first, tokio::time::Instant::now()).await,
        "the spent original cutoff never obtains a new allowance"
    );
    let status = current_status(state, &first.binding, deadline)
        .await
        .expect("status")
        .expect("row");
    assert_eq!(status.attempts[0].phase, AttemptPhase::Cleanup);
    assert!(!policy_refusal_cleanup_confirmed(&status, &first));
    assert_eq!(status.binding, original_binding);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn trial_policy_retry_waits_for_original_join_and_close_within_same_cutoff() {
    let dir = crate::test_tempdir().expect("state dir");
    let fixture = crate::transcode::HlsDeliveryFixture::publish_without_process(
        dir.path(),
        &Uuid::new_v4().to_string(),
    )
    .await;
    let state = &fixture.state;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let (mut context, first) = request(state).await;
    let first = claim(state, &mut context, first, deadline).await;
    let original_binding = first.binding.clone();
    let (release, _attempt_dir, _attempt_runtime, _attempt_root) = state
        .transcode
        .retain_empty_failed_adaptive_coordinator_for_test(first.clone())
        .await;
    fence_original(state, &mut context, &first, deadline)
        .await
        .expect("logical original fence");
    let ready = policy_refusal_retry_ready(state, &first, deadline);
    tokio::pin!(ready);
    assert!(
        tokio::time::timeout(Duration::from_millis(75), &mut ready)
            .await
            .is_err(),
        "the running original coordinator prevents Closed and keeps this waiter pending"
    );
    assert_eq!(
        current_status(state, &first.binding, deadline)
            .await
            .expect("status")
            .expect("row")
            .attempts[0]
            .phase,
        AttemptPhase::Cleanup
    );
    release.send(()).expect("release same original task");
    // Existing production reconciliation performs the actual JoinHandle join,
    // seals the empty P state and publishes its retained original Close. This
    // fixture proves task/protocol gating; no native work ever entered.
    let drive_original_close = async {
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "original close cutoff"
            );
            state
                .transcode
                .reconcile_failed_adaptive_coordinator_for_test()
                .await;
            let status = current_status(state, &first.binding, deadline)
                .await
                .expect("status")
                .expect("row");
            if policy_refusal_cleanup_confirmed(&status, &first) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let (ready, status) = tokio::join!(ready, drive_original_close);
    assert!(
        ready,
        "positive original close releases the already waiting retry"
    );
    assert_eq!(status.binding, original_binding);
    assert!(next_claim_slot(Some(&status), "next-worker")
        .expect("next slot")
        .is_some());
    let next = claim(
        state,
        &mut context,
        candidate(&first, "next-worker"),
        deadline,
    )
    .await;
    assert_eq!(
        next.binding, original_binding,
        "same original deadline and quality/intent binding"
    );
    for changed in ["worker", "generation", "attempt", "phase"] {
        let mut wrong = status.clone();
        match changed {
            "worker" => wrong.attempts[0].worker_instance_id = Uuid::new_v4(),
            "generation" => wrong.attempts[0].serving_generation += 1,
            "attempt" => wrong.attempts[0].key.attempt_id = Uuid::new_v4(),
            _ => wrong.attempts[0].phase = AttemptPhase::Losing,
        }
        assert!(
            !policy_refusal_cleanup_confirmed(&wrong, &first),
            "{changed}"
        );
    }
}
