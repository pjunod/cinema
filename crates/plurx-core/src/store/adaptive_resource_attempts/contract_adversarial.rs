//! Actual backend race, old-writer, rollback and uncertain-commit fixtures.
use super::*;

fn ordinary(fixture: &AttemptFixture) -> MediaSessionActivation {
    let binding = &fixture.claim.binding;
    MediaSessionActivation {
        incarnation_id: Uuid::new_v4().to_string(),
        session_id: Uuid::new_v4().to_string(),
        principal: binding.principal.clone(),
        playback_id: binding.playback_id.clone(),
        recovery_epoch: binding.recovery_epoch.to_string(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: true,
        request_id: None,
        request_fingerprint: binding.request_fingerprint.clone(),
        owner_node_id: "ordinary-owner".into(),
        recipe_json: "{}".into(),
        response_json: "{}".into(),
        publication_ready_at_ms: 0,
        media_origin_ms: 0,
        now_ms: 10_000,
        lease_expires_at_ms: 30_000,
        expected_desired_revision: None,
    }
}

#[tokio::test]
async fn simultaneous_winners_preserve_one_winner_and_all_cleanup_debt() {
    for_each_backend(|store, _| async move {
        let first = AttemptFixture::new(store.as_ref()).await;
        let claimed = applied(
            store
                .claim_adaptive_attempt(&first.claim)
                .await
                .expect("first"),
        );
        let second = AttemptFixture {
            claim: first.second(&claimed, false),
        };
        let claimed = applied(
            store
                .claim_adaptive_attempt(&second.claim)
                .await
                .expect("second"),
        );
        let first_prepared = first.prepared(&claimed);
        let sealed = applied(
            store
                .record_adaptive_prepared(&first_prepared)
                .await
                .expect("first seal"),
        );
        let second_prepared = second.prepared(&sealed);
        let sealed = applied(
            store
                .record_adaptive_prepared(&second_prepared)
                .await
                .expect("second seal"),
        );
        let selection = |prepared: PreparedAttempt| WinnerSelection {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: sealed.sequence,
                expected_attempt_sequence: sealed
                    .attempts
                    .iter()
                    .find(|a| a.key == prepared.key)
                    .expect("attempt")
                    .sequence,
                now_ms: 10_003,
            },
            binding: prepared.binding,
            key: prepared.key,
            worker: prepared.worker,
        };
        let a = selection(first_prepared);
        let b = selection(second_prepared);
        let (a_result, b_result) = tokio::join!(
            store.select_adaptive_winner(&a),
            store.select_adaptive_winner(&b)
        );
        assert_eq!(
            [&a_result, &b_result]
                .iter()
                .filter(|result| matches!(result, Ok(AttemptMutation::Applied(_))))
                .count(),
            1,
            "exactly one serialized winner: {a_result:?}, {b_result:?}"
        );
        let status = store
            .adaptive_request_status(&first.claim.binding.key, &first.claim.binding.principal)
            .await
            .expect("authority")
            .expect("request");
        assert_eq!(
            status
                .attempts
                .iter()
                .filter(|a| a.phase == AttemptPhase::Winner)
                .count(),
            1
        );
        assert_eq!(
            status
                .attempts
                .iter()
                .filter(|a| a.phase == AttemptPhase::Losing)
                .count(),
            1
        );
        let mut chosen = if status.winner_attempt_id == Some(a.key.attempt_id) {
            a
        } else {
            b
        };
        chosen.context.operation_id = Uuid::new_v4();
        chosen.context.now_ms = 50_000; // read-only replay cannot renew authority
        assert!(matches!(
            store
                .select_adaptive_winner(&chosen)
                .await
                .expect("same selected winner"),
            AttemptMutation::Replay(_)
        ));
        chosen.worker.output_digest = "9".repeat(64);
        assert_eq!(
            store
                .select_adaptive_winner(&chosen)
                .await
                .expect("changed seal"),
            AttemptMutation::Refused(AttemptRefusal::Conflict)
        );
        assert!(store
            .media_session_route_for_playback(
                &first.claim.binding.principal,
                &first.claim.binding.playback_id
            )
            .await
            .expect("no partial publication")
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn initial_claim_rechecks_predecessor_epoch_and_desired_before_acquisition() {
    for_each_backend(|store, _| async move {
        let mut fixture = AttemptFixture::new(store.as_ref()).await;
        let incumbent = store
            .activate_media_session(&ordinary(&fixture))
            .await
            .expect("ordinary activation")
            .expect("incumbent")
            .route;
        assert!(
            store.claim_adaptive_attempt(&fixture.claim).await.is_err(),
            "missing expected predecessor"
        );
        fixture.claim.binding.expected_predecessor_incarnation_id =
            Some(Uuid::parse_str(&incumbent.incarnation_id).expect("UUID"));
        fixture.claim.binding.expected_predecessor_owner_epoch = Some(incumbent.owner_epoch + 1);
        assert!(
            store.claim_adaptive_attempt(&fixture.claim).await.is_err(),
            "stale epoch"
        );
        fixture.claim.binding.expected_predecessor_owner_epoch = Some(incumbent.owner_epoch);
        let desired = store
            .record_desired_selection(
                &fixture.claim.binding.principal,
                &fixture.claim.binding.playback_id,
                &"1".repeat(64),
                "{}",
                10_001,
            )
            .await
            .expect("actual desired ask");
        fixture.claim.binding.expected_desired_revision = Some(desired.revision + 1);
        assert!(
            store.claim_adaptive_attempt(&fixture.claim).await.is_err(),
            "stale desired revision"
        );
        assert!(store
            .media_session_route_by_incarnation(&fixture.claim.key.route_incarnation_id.to_string())
            .await
            .expect("no provisional before admission")
            .is_none());
        fixture.claim.binding.expected_desired_revision = Some(desired.revision);
        let status = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("current acquisition context"),
        );
        assert_eq!(status.attempts.len(), 1);
        assert_eq!(
            store
                .media_session_route_for_playback(
                    &fixture.claim.binding.principal,
                    &fixture.claim.binding.playback_id
                )
                .await
                .expect("incumbent pointer")
                .expect("incumbent")
                .incarnation_id,
            incumbent.incarnation_id
        );
    })
    .await;
}

#[tokio::test]
async fn failed_desired_adoption_retains_provisional_lease_and_incumbent() {
    for_each_backend(|store, _| async move {
        let mut fixture = AttemptFixture::new(store.as_ref()).await;
        let incumbent = store
            .activate_media_session(&ordinary(&fixture))
            .await
            .expect("ordinary")
            .expect("incumbent")
            .route;
        fixture.claim.binding.expected_predecessor_incarnation_id =
            Some(Uuid::parse_str(&incumbent.incarnation_id).expect("UUID"));
        fixture.claim.binding.expected_predecessor_owner_epoch = Some(incumbent.owner_epoch);
        let desired = store
            .record_desired_selection(
                &fixture.claim.binding.principal,
                &fixture.claim.binding.playback_id,
                &"1".repeat(64),
                "{}",
                10_001,
            )
            .await
            .expect("ask");
        fixture.claim.binding.expected_desired_revision = Some(desired.revision);
        let adoption = ready_adoption(store.as_ref(), &fixture).await;
        let before = store
            .adaptive_request_status(&fixture.claim.binding.key, &fixture.claim.binding.principal)
            .await
            .expect("before");
        store
            .record_desired_selection(
                &fixture.claim.binding.principal,
                &fixture.claim.binding.playback_id,
                &"2".repeat(64),
                "{\"changed\":true}",
                10_004,
            )
            .await
            .expect("new desired");
        assert!(
            store.adopt_adaptive_winner(&adoption).await.is_err(),
            "transaction must refuse changed desired"
        );
        assert_eq!(
            store
                .adaptive_request_status(
                    &fixture.claim.binding.key,
                    &fixture.claim.binding.principal
                )
                .await
                .expect("rollback"),
            before
        );
        let retained = store
            .media_session_route_by_incarnation(&fixture.claim.key.route_incarnation_id.to_string())
            .await
            .expect("route")
            .expect("retained");
        assert_eq!(retained.state, "starting");
        assert_eq!(
            retained.lease_expires_at_ms,
            fixture.claim.binding.original_deadline_ms
        );
        let retained = store
            .media_session_route_by_incarnation(&incumbent.incarnation_id)
            .await
            .expect("incumbent")
            .expect("retained");
        assert_eq!(retained.state, "active");
        assert_eq!(retained.publication_ready_at_ms, 0);
    })
    .await;
}

#[tokio::test]
async fn replace_upsert_proposed_identity_and_requestless_activation_are_fenced() {
    for_each_strandable_backend(|store,_,raw|async move {
        let fixture=AttemptFixture::new(store.as_ref()).await;
        let before=applied(store.claim_adaptive_attempt(&fixture.claim).await.expect("claim"));
        let route=fixture.claim.key.route_incarnation_id;
        let request=&fixture.claim.binding.key.request_id;
        for sql in [
            format!("PRAGMA recursive_triggers=OFF; INSERT OR REPLACE INTO media_sessions SELECT * FROM media_sessions WHERE incarnation_id='{route}'"),
            format!("PRAGMA recursive_triggers=OFF; INSERT OR REPLACE INTO media_session_requests SELECT * FROM media_session_requests WHERE request_id='{request}'"),
            format!("PRAGMA recursive_triggers=OFF; INSERT OR REPLACE INTO job_leases SELECT * FROM job_leases WHERE resource='session:{route}'"),
            format!("INSERT INTO media_sessions SELECT * FROM media_sessions WHERE incarnation_id='{route}' ON CONFLICT(incarnation_id) DO UPDATE SET state='active'"),
            format!("DELETE FROM media_resource_attempts WHERE route_incarnation_id='{route}'"),
            format!("UPDATE media_resource_attempts SET phase='closed' WHERE route_incarnation_id='{route}'"),
        ] { assert!(raw_refused(raw.as_ref(),&sql).await,"legacy SQL: {sql}"); }
        for request_id in [None,Some("alternate-unrelated-request".to_owned())] {
            let mut bypass=ordinary(&fixture);
            bypass.incarnation_id=route.to_string();
            bypass.session_id=fixture.claim.key.session_id.to_string();
            bypass.owner_node_id=fixture.claim.key.worker_node_id.clone();
            bypass.request_id=request_id;
            assert!(!matches!(store.activate_media_session(&bypass).await,Ok(Some(_))),"cannot bypass using ordinary activation");
        }
        assert!(store.validation_write_legacy_playback_pointer(&fixture.claim.binding.principal,&fixture.claim.binding.playback_id,&route.to_string(),10_005).await.is_err());
        let other=ordinary(&AttemptFixture::new(store.as_ref()).await);
        let other_route=store.activate_media_session(&other).await.expect("unrelated ordinary").expect("active").route;
        assert!(raw_refused(raw.as_ref(),&format!("UPDATE media_sessions SET session_id='{}' WHERE incarnation_id='{}'",fixture.claim.key.session_id,other_route.incarnation_id)).await,"proposed adaptive session identity also protected");
        assert!(raw_refused(raw.as_ref(),&format!("UPDATE job_leases SET resource='session:{route}' WHERE resource='session:{}'",other_route.incarnation_id)).await);
        assert_eq!(store.adaptive_request_status(&fixture.claim.binding.key,&fixture.claim.binding.principal).await.expect("retained"),Some(before));
    }).await;
}

#[tokio::test]
async fn both_backend_maintenance_preserves_expired_unknown_debt_and_ordinary_work() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let before = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("claim"),
        );
        let other = AttemptFixture::new(store.as_ref()).await;
        store
            .maintain_media_sessions(100_000_000)
            .await
            .expect("mixed maintenance");
        assert_eq!(
            store
                .adaptive_request_status(
                    &fixture.claim.binding.key,
                    &fixture.claim.binding.principal
                )
                .await
                .expect("expired debt"),
            Some(before)
        );
        assert!(matches!(
            store
                .claim_media_session_request(
                    &other.claim.binding.principal,
                    &other.claim.binding.key.request_id,
                    &other.claim.binding.request_fingerprint,
                    &other.claim.binding.playback_id,
                    &Uuid::new_v4().to_string(),
                    100_000_001,
                    100_010_000
                )
                .await
                .expect("ordinary root was actually pruned"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test]
async fn replicated_lost_prepare_and_adopt_acknowledgements_reconcile_exact_operations() {
    use plurx_core::store::adaptive_resource_attempts::validation::{self, TransactionFault};
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("exclusive fixture reset");
    let fixture = AttemptFixture::new(&store).await;
    assert!(validation::claim_hiqlite(
        &store,
        &fixture.claim,
        TransactionFault::LostAcknowledgement
    )
    .await
    .is_err());
    let claimed = match store
        .claim_adaptive_attempt(&fixture.claim)
        .await
        .expect("exact claim replay")
    {
        AttemptMutation::Replay(s) => s,
        other => panic!("{other:?}"),
    };
    let mut prepared = fixture.prepared(&claimed);
    prepared.worker.selected_recipe_digest = selected_recipe_digest("{}").expect("recipe");
    assert!(
        validation::prepared_hiqlite(&store, &prepared, TransactionFault::LostAcknowledgement)
            .await
            .is_err()
    );
    let sealed = match store
        .record_adaptive_prepared(&prepared)
        .await
        .expect("exact prepared replay")
    {
        AttemptMutation::Replay(s) => s,
        other => panic!("{other:?}"),
    };
    let chosen = WinnerSelection {
        context: MutationContext {
            operation_id: Uuid::new_v4(),
            expected_request_sequence: sealed.sequence,
            expected_attempt_sequence: sealed.attempts[0].sequence,
            now_ms: 10_003,
        },
        binding: prepared.binding,
        key: prepared.key,
        worker: prepared.worker,
    };
    let won = applied(store.select_adaptive_winner(&chosen).await.expect("winner"));
    let mut activation = ordinary(&fixture);
    activation.incarnation_id = chosen.key.route_incarnation_id.to_string();
    activation.session_id = chosen.key.session_id.to_string();
    activation.request_id = Some(chosen.binding.key.request_id.clone());
    activation.owner_node_id = chosen.key.worker_node_id.clone();
    activation.publication_ready_at_ms = MEDIA_SESSION_PUBLICATION_BLOCKED;
    activation.now_ms = 10_004;
    let adoption = AdaptiveAdoption {
        context: MutationContext {
            operation_id: Uuid::new_v4(),
            expected_request_sequence: won.sequence,
            expected_attempt_sequence: won.attempts[0].sequence,
            now_ms: 10_004,
        },
        binding: chosen.binding,
        key: chosen.key,
        worker: chosen.worker,
        activation,
    };
    assert!(
        validation::adopt_hiqlite(&store, &adoption, TransactionFault::FinalAssertion)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .adaptive_request_status(&fixture.claim.binding.key, &fixture.claim.binding.principal)
            .await
            .expect("whole adoption rolled back"),
        Some(won)
    );
    assert!(
        validation::adopt_hiqlite(&store, &adoption, TransactionFault::LostAcknowledgement)
            .await
            .is_err()
    );
    assert!(matches!(
        store
            .adopt_adaptive_winner(&adoption)
            .await
            .expect("exact adoption replay"),
        AttemptAdoption::Replay(_)
    ));
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test]
async fn cancelled_waiter_and_late_commit_remain_blocked_until_exact_status() {
    use plurx_core::store::adaptive_resource_attempts::validation::{self, TransactionFault};
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = Arc::new(open_contract_hiqlite_store(&cluster).await);
    store
        .validation_reset_contract_state()
        .await
        .expect("exclusive fixture reset");
    let fixture = AttemptFixture::new(store.as_ref()).await;
    let submitted = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let operation = tokio::spawn({
        let store = store.clone();
        let claim = fixture.claim.clone();
        let submitted = submitted.clone();
        let release = release.clone();
        async move {
            let result = validation::claim_hiqlite(
                &store,
                &claim,
                TransactionFault::BeforeCommit { submitted, release },
            )
            .await;
            let _ = tx.send(result);
        }
    });
    submitted.notified().await;
    drop(rx); // cancel ONLY the waiter, not the owned in-flight mutation
    release.notify_one();
    operation
        .await
        .expect("owned mutation completed after waiter cancellation");
    let status = store
        .adaptive_request_status(&fixture.claim.binding.key, &fixture.claim.binding.principal)
        .await
        .expect("authoritative settlement")
        .expect("late committed attempt");
    assert_eq!(status.attempts.len(), 1);
    let route = store
        .media_session_route_by_incarnation(&fixture.claim.key.route_incarnation_id.to_string())
        .await
        .expect("route")
        .expect("retained");
    assert_eq!(
        route.publication_ready_at_ms,
        MEDIA_SESSION_PUBLICATION_BLOCKED
    );
    assert!(matches!(
        store
            .claim_adaptive_attempt(&fixture.claim)
            .await
            .expect("same-op reconciliation"),
        AttemptMutation::Replay(_)
    ));
    let mut late = fixture.claim.clone();
    late.context.operation_id = Uuid::new_v4();
    late.context.now_ms = 30_000;
    assert_eq!(
        store
            .claim_adaptive_attempt(&late)
            .await
            .expect("no reset budget"),
        AttemptMutation::Refused(AttemptRefusal::Expired)
    );
}
