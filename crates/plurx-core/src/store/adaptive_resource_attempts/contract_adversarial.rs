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
    for_each_adaptive_backend(|store, _| async move {
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
    for_each_adaptive_backend(|store, _| async move {
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
    for_each_adaptive_backend(|store, _| async move {
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
    for_each_adaptive_strandable_backend(|store,_,raw|async move {
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
    for_each_adaptive_backend(|store, _| async move {
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
    with_adaptive_cluster(|cluster| {
        Box::pin(async move {
            use plurx_core::store::adaptive_resource_attempts::validation::{
                self, TransactionFault,
            };
            let store = cluster.open_store().await;
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
            assert!(validation::prepared_hiqlite(
                &store,
                &prepared,
                TransactionFault::LostAcknowledgement
            )
            .await
            .is_err());
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
                    .adaptive_request_status(
                        &fixture.claim.binding.key,
                        &fixture.claim.binding.principal
                    )
                    .await
                    .expect("whole adoption rolled back"),
                Some(won)
            );
            assert!(validation::adopt_hiqlite(
                &store,
                &adoption,
                TransactionFault::LostAcknowledgement
            )
            .await
            .is_err());
            assert!(matches!(
                store
                    .adopt_adaptive_winner(&adoption)
                    .await
                    .expect("exact adoption replay"),
                AttemptAdoption::Replay(_)
            ));
        })
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test]
async fn cancelled_waiter_and_late_commit_remain_blocked_until_exact_status() {
    with_adaptive_cluster(|cluster| {
        Box::pin(async move {
            use plurx_core::store::adaptive_resource_attempts::validation::{
                self, TransactionFault,
            };
            let store = Arc::new(cluster.open_store().await);
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
                .adaptive_request_status(
                    &fixture.claim.binding.key,
                    &fixture.claim.binding.principal,
                )
                .await
                .expect("authoritative settlement")
                .expect("late committed attempt");
            assert_eq!(status.attempts.len(), 1);
            let route = store
                .media_session_route_by_incarnation(
                    &fixture.claim.key.route_incarnation_id.to_string(),
                )
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
        })
    })
    .await;
}

async fn adaptive_route(store: &dyn Store, adoption: &AdaptiveAdoption) -> MediaSessionRoute {
    store
        .media_session_route_by_incarnation(&adoption.activation.incarnation_id)
        .await
        .expect("route read")
        .expect("route retained")
}

async fn committed_adoption(store: &dyn Store) -> (AttemptFixture, AdaptiveAdoption) {
    let fixture = AttemptFixture::new(store).await;
    let adoption = ready_adoption(store, &fixture).await;
    assert!(matches!(
        store.adopt_adaptive_winner(&adoption).await.expect("adopt"),
        AttemptAdoption::Applied(_)
    ));
    (fixture, adoption)
}

/// The worker's publication clears only the route its exact Adopt still
/// holds. Every refusal leaves the row BLOCKED for its original owner.
#[tokio::test]
async fn adaptive_publication_refuses_without_the_exact_committed_adoption() {
    for_each_adaptive_strandable_backend(|store, backend, raw| async move {
        let store = store.as_ref();
        // Before Adopt the provisional route is not the worker's to publish.
        let fixture = AttemptFixture::new(store).await;
        let adoption = ready_adoption(store, &fixture).await;
        assert!(
            store
                .publish_adaptive_adoption(&adoption, 10_005)
                .await
                .is_err(),
            "{backend}: publication before Adopt"
        );
        let route = adaptive_route(store, &adoption).await;
        assert_eq!(route.state, "starting", "{backend}");
        assert_eq!(
            route.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );

        // After the activation lease (30 000) expired.
        assert!(matches!(
            store.adopt_adaptive_winner(&adoption).await.expect("adopt"),
            AttemptAdoption::Applied(_)
        ));
        assert!(
            store
                .publish_adaptive_adoption(&adoption, 30_001)
                .await
                .is_err(),
            "{backend}: publication after the lease"
        );
        let route = adaptive_route(store, &adoption).await;
        assert_eq!(route.state, "active", "{backend}");
        assert_eq!(
            route.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );

        // A different Adopt DTO is a definite refusal, not a second owner.
        let (_, adoption) = committed_adoption(store).await;
        let mut changed = adoption.clone();
        changed.activation.response_json = "{\"changed\":true}".into();
        assert!(
            !matches!(
                store.publish_adaptive_adoption(&changed, 10_005).await,
                Ok(Some(_))
            ),
            "{backend}: changed response"
        );
        let mut epoch = adoption.clone();
        epoch.key.owner_epoch = 2;
        assert!(
            !matches!(
                store.publish_adaptive_adoption(&epoch, 10_005).await,
                Ok(Some(_))
            ),
            "{backend}: wrong owner epoch"
        );
        assert_eq!(
            adaptive_route(store, &adoption)
                .await
                .publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );

        // The pointer moved off the route (an ordinary writer may point it
        // at a non-attempt incarnation; the route itself stays active).
        let (_, adoption) = committed_adoption(store).await;
        let moved = format!(
            "UPDATE media_playback_pointers SET current_incarnation_id='{}' WHERE playback_id='{}'",
            Uuid::new_v4(),
            adoption.binding.playback_id
        );
        assert!(
            !raw_refused(raw.as_ref(), &moved).await,
            "{backend}: ordinary pointer move"
        );
        assert!(
            store
                .publish_adaptive_adoption(&adoption, 10_006)
                .await
                .is_err(),
            "{backend}: publication after the pointer moved"
        );
        let route = adaptive_route(store, &adoption).await;
        assert_eq!(route.state, "active", "{backend}");
        assert_eq!(
            route.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );

        // The route ended.
        let (_, adoption) = committed_adoption(store).await;
        store
            .end_media_session(&adoption.activation.session_id, "admin_stop", 10_005)
            .await
            .expect("end")
            .expect("ended route");
        assert!(
            store
                .publish_adaptive_adoption(&adoption, 10_006)
                .await
                .is_err(),
            "{backend}: publication after end"
        );
        let route = adaptive_route(store, &adoption).await;
        assert_eq!(route.state, "ended", "{backend}");
        assert_eq!(
            route.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );
    })
    .await;
}

/// A live attached rendition may renew between Adopt and publication. The
/// original Adopt remains the publication capability after that renewal.
#[tokio::test]
async fn adaptive_publication_accepts_the_same_owners_renewed_live_lease() {
    for_each_adaptive_backend(|store, backend| async move {
        let store = store.as_ref();
        let (_, adoption) = committed_adoption(store).await;
        let renewed = store
            .renew_media_sessions(
                &adoption.key.worker_node_id,
                &[MediaSessionRenewal {
                    incarnation_id: adoption.activation.incarnation_id.clone(),
                    owner_epoch: adoption.key.owner_epoch,
                    produced_playable_through_ms: 0,
                    fetched_through_ms: 0,
                    media_sequence: 0,
                }],
                10_005,
                50_000,
            )
            .await
            .expect("same owner renews its attached route");
        assert_eq!(
            renewed,
            vec![adoption.activation.incarnation_id.clone()],
            "{backend}"
        );
        let route = adaptive_route(store, &adoption).await;
        assert_eq!(route.lease_expires_at_ms, 50_000, "{backend}");
        assert_eq!(
            route.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );
        // The original expiry was 30 000. Only the actual renewed lease,
        // with the same owner and epoch, makes publication at 30 001 live.
        let published = store
            .publish_adaptive_adoption(&adoption, 30_001)
            .await
            .expect("publication after legitimate renewal")
            .expect("same adopted route");
        assert_eq!(published.route.lease_expires_at_ms, 50_000, "{backend}");
        assert_eq!(published.route.publication_ready_at_ms, 0, "{backend}");
        let replay = store
            .publish_adaptive_adoption(&adoption, 30_002)
            .await
            .expect("publication replay")
            .expect("same published route");
        assert_eq!(replay.route, published.route, "{backend}");
        assert!(
            store
                .publish_adaptive_adoption(&adoption, 50_001)
                .await
                .is_err(),
            "{backend}: renewal never authorizes publication after its actual expiry"
        );
    })
    .await;
}

#[tokio::test]
async fn adaptive_publication_refuses_mismatched_live_route_and_job_leases() {
    for_each_adaptive_strandable_backend(|store, backend, raw| async move {
        let store = store.as_ref();
        let (_, adoption) = committed_adoption(store).await;
        let changed = format!(
            "UPDATE job_leases SET expires_at_ms=40000 WHERE resource='session:{}'",
            adoption.activation.incarnation_id
        );
        assert!(
            !raw_refused(raw.as_ref(), &changed).await,
            "{backend}: fixture lease mismatch"
        );
        assert!(
            store
                .publish_adaptive_adoption(&adoption, 10_005)
                .await
                .is_err(),
            "{backend}: two live but inconsistent lease copies cannot publish"
        );
        assert_eq!(
            adaptive_route(store, &adoption)
                .await
                .publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED,
            "{backend}"
        );
    })
    .await;
}

async fn ready_staged_adoption(
    store: &dyn Store,
    retry: bool,
) -> (AdaptiveAdoption, AdaptiveStagedAdoption, MediaSessionRoute) {
    let mut fixture = AttemptFixture::new(store).await;
    let incumbent = store
        .activate_media_session(&ordinary(&fixture))
        .await
        .expect("ordinary activation")
        .expect("incumbent")
        .route;
    fixture.claim.binding.expected_predecessor_incarnation_id =
        Some(Uuid::parse_str(&incumbent.incarnation_id).expect("incumbent identity"));
    fixture.claim.binding.expected_predecessor_owner_epoch = Some(incumbent.owner_epoch);
    let desired = store
        .record_desired_selection(
            &incumbent.principal,
            &incumbent.playback_id,
            &"1".repeat(64),
            "{}",
            10_001,
        )
        .await
        .expect("actual viewer ask");
    fixture.claim.binding.expected_desired_revision = Some(desired.revision);
    if retry {
        let first = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("first Claim"),
        );
        let fence = AttemptFence {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: first.sequence,
                expected_attempt_sequence: attempt_sequence(&first, &fixture.claim.key),
                now_ms: 10_001,
            },
            binding: fixture.claim.binding.clone(),
            key: fixture.claim.key.clone(),
            cancel_request: false,
        };
        let fenced = applied(
            store
                .fence_adaptive_attempt(&fence)
                .await
                .expect("first Fence"),
        );
        let close = AttemptClose {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: fenced.sequence,
                expected_attempt_sequence: attempt_sequence(&fenced, &fixture.claim.key),
                now_ms: 10_001,
            },
            binding: fixture.claim.binding.clone(),
            key: fixture.claim.key.clone(),
            worker_instance_id: fixture.claim.worker_instance_id,
            serving_generation: fixture.claim.serving_generation,
            cleanup_operation_id: fence.context.operation_id,
        };
        let closed = applied(
            store
                .close_adaptive_attempt(&close)
                .await
                .expect("first Close"),
        );
        fixture.claim = fixture.second(&closed, false);
    }
    let activation = ready_adoption(store, &fixture).await;
    let staged = AdaptiveStagedAdoption {
        context: activation.context.clone(),
        binding: activation.binding.clone(),
        key: activation.key.clone(),
        worker: activation.worker.clone(),
        preparation: plurx_core::domain::MediaSessionPreparation {
            quality_cancellation_key: Some("e".repeat(64)),
            incarnation_id: activation.activation.incarnation_id.clone(),
            session_id: activation.activation.session_id.clone(),
            principal: incumbent.principal.clone(),
            playback_id: incumbent.playback_id.clone(),
            expected_predecessor_incarnation_id: incumbent.incarnation_id.clone(),
            expected_predecessor_owner_node_id: incumbent.owner_node_id.clone(),
            expected_predecessor_owner_epoch: incumbent.owner_epoch,
            request_fingerprint: activation.binding.request_fingerprint.clone(),
            owner_node_id: activation.key.worker_node_id.clone(),
            recipe_json: activation.activation.recipe_json.clone(),
            response_json: activation.activation.response_json.clone(),
            media_origin_ms: 0,
            now_ms: activation.context.now_ms,
            expected_desired_revision: Some(desired.revision),
            deadline_ms: 25_000,
        },
    };
    (activation, staged, incumbent)
}

fn staged_viewer_commit(
    p: &plurx_core::domain::MediaSessionPreparation,
    incumbent: &MediaSessionRoute,
) -> plurx_core::domain::MediaSessionPreparationCommitRequest {
    let now_ms = p.now_ms + 2;
    plurx_core::domain::MediaSessionPreparationCommitRequest {
        staged_incarnation_id: p.incarnation_id.clone(),
        expected_predecessor_owner_node_id: incumbent.owner_node_id.clone(),
        expected_predecessor_owner_epoch: incumbent.owner_epoch,
        expected_desired_revision: p.expected_desired_revision,
        now_ms,
        lease_expires_at_ms: 35_000,
        control_receipt: Some(plurx_core::domain::MediaSessionTerminalAck {
            incarnation_id: incumbent.incarnation_id.clone(),
            session_id: incumbent.session_id.clone(),
            owner_node_id: incumbent.owner_node_id.clone(),
            owner_epoch: incumbent.owner_epoch,
            client_instance_id: Uuid::new_v4().to_string(),
            sequence: 2,
            request_fingerprint: "f".repeat(64),
            response_json: "{}".to_owned(),
            expires_at_ms: 40_000,
            updated_at_ms: now_ms,
        }),
    }
}

#[tokio::test]
async fn staged_adopt_keeps_incumbent_current_until_existing_viewer_commit() {
    for retry in [false, true] {
        for_each_adaptive_backend(move |store, _| async move {
            let (ordinary, staged, incumbent) = ready_staged_adoption(store.as_ref(), retry).await;
            let p = &staged.preparation;
            let applied = store
                .stage_adaptive_winner(&staged)
                .await
                .expect("atomic stage");
            let AttemptAdoption::Applied(applied) = applied else {
                panic!("expected original staged route");
            };
            assert_eq!(applied.predecessor.as_ref(), Some(&incumbent));
            assert_eq!(applied.route.owner_epoch, if retry { 2 } else { 1 });
            assert_eq!(applied.route.incarnation_id, p.incarnation_id);
            assert_eq!(
                applied.route.publication_ready_at_ms,
                MEDIA_SESSION_PUBLICATION_BLOCKED
            );
            assert_eq!(applied.route.lease_expires_at_ms, p.deadline_ms);
            assert_eq!(
                store
                    .media_session_route_for_playback(&p.principal, &p.playback_id)
                    .await
                    .expect("active pointer"),
                Some(incumbent.clone())
            );
            assert_eq!(
                store
                    .staged_media_session_for_playback(&p.principal, &p.playback_id)
                    .await
                    .expect("pending ledger")
                    .expect("original staging")
                    .staged_incarnation_id,
                p.incarnation_id
            );
            assert_eq!(
                store
                    .stage_adaptive_winner(&staged)
                    .await
                    .expect("exact replay"),
                AttemptAdoption::Replay(applied)
            );
            assert_eq!(
                store
                    .adopt_adaptive_winner(&ordinary)
                    .await
                    .expect("other mode's receipt"),
                AttemptAdoption::Refused(AttemptRefusal::Conflict)
            );
            let mut changed = staged.clone();
            changed.preparation.deadline_ms += 1;
            assert_eq!(
                store
                    .stage_adaptive_winner(&changed)
                    .await
                    .expect("different replay"),
                AttemptAdoption::Refused(AttemptRefusal::Conflict)
            );
            let current = store
                .adaptive_request_status(&staged.binding.key, &staged.binding.principal)
                .await
                .expect("attempt ledger")
                .expect("transferred original");
            assert_eq!(current.disposition, RequestDisposition::Adopted);
            let fence = AttemptFence {
                context: MutationContext {
                    operation_id: Uuid::new_v4(),
                    expected_request_sequence: current.sequence,
                    expected_attempt_sequence: attempt_sequence(&current, &staged.key),
                    now_ms: p.now_ms + 1,
                },
                binding: staged.binding.clone(),
                key: staged.key.clone(),
                cancel_request: true,
            };
            assert_eq!(
                store
                    .fence_adaptive_attempt(&fence)
                    .await
                    .expect("attempt Fence after transfer"),
                AttemptMutation::Refused(AttemptRefusal::WinnerLost)
            );
            let commit = staged_viewer_commit(p, &incumbent);
            let committed = store
                .commit_media_session_preparation(&p.principal, &p.playback_id, &commit)
                .await
                .expect("existing viewer transaction")
                .expect("same successor");
            assert_eq!(committed.control_receipt, commit.control_receipt);
            let replay = store
                .commit_media_session_preparation(&p.principal, &p.playback_id, &commit)
                .await
                .expect("durable viewer receipt replay")
                .expect("same committed viewer");
            assert_eq!(replay.route, committed.route);
            assert_eq!(replay.control_receipt, commit.control_receipt);
            assert!(replay.predecessor.is_none());
            assert_eq!(committed.route.incarnation_id, p.incarnation_id);
            assert_eq!(committed.route.owner_epoch, staged.key.owner_epoch);
            assert_eq!(
                store
                    .lease_expiry_hint(&format!("session:{}", p.incarnation_id))
                    .await
                    .expect("exact staged lease"),
                Some(commit.lease_expires_at_ms)
            );
            assert_eq!(
                committed.route.lease_expires_at_ms,
                commit.lease_expires_at_ms
            );
            assert_eq!(
                store
                    .media_session_route_for_playback(&p.principal, &p.playback_id)
                    .await
                    .expect("advanced pointer")
                    .expect("new route")
                    .incarnation_id,
                p.incarnation_id
            );
            assert!(store
                .staged_media_session_for_playback(&p.principal, &p.playback_id)
                .await
                .expect("pending slot consumed")
                .is_none());
            let draining = store
                .media_session_route_by_incarnation(&incumbent.incarnation_id)
                .await
                .expect("predecessor")
                .expect("draining predecessor");
            assert_eq!(draining.state, "active");
            assert_eq!(draining.lease_expires_at_ms, incumbent.lease_expires_at_ms);
            assert!(draining.drain_deadline_ms.is_some());
        })
        .await;
    }
}

#[tokio::test]
async fn staged_adopt_refuses_changed_ask_and_abort_keeps_native_debt() {
    for_each_adaptive_backend(|store, _| async move {
        let (_, staged, incumbent) = ready_staged_adoption(store.as_ref(), false).await;
        let p = &staged.preparation;
        let wrong_owner = {
            let mut wrong = staged.clone();
            wrong.preparation.expected_predecessor_owner_node_id = "another-owner".into();
            wrong
        };
        assert!(store.stage_adaptive_winner(&wrong_owner).await.is_err());
        let before = store
            .adaptive_request_status(&staged.binding.key, &staged.binding.principal)
            .await
            .expect("before failed stage")
            .expect("pending winner");
        store
            .record_desired_selection(
                &p.principal,
                &p.playback_id,
                &"2".repeat(64),
                "{}",
                p.now_ms + 1,
            )
            .await
            .expect("new viewer ask");
        assert!(store.stage_adaptive_winner(&staged).await.is_err());
        assert_eq!(
            store
                .adaptive_request_status(&staged.binding.key, &staged.binding.principal)
                .await
                .expect("CAS rollback"),
            Some(before)
        );
        assert_eq!(
            store
                .media_session_route_for_playback(&p.principal, &p.playback_id)
                .await
                .expect("original pointer"),
            Some(incumbent)
        );
        assert!(store
            .staged_media_session_for_playback(&p.principal, &p.playback_id)
            .await
            .expect("empty preparation slot")
            .is_none());

        let (_, staged, incumbent) = ready_staged_adoption(store.as_ref(), false).await;
        let p = &staged.preparation;
        assert!(matches!(
            store
                .stage_adaptive_winner(&staged)
                .await
                .expect("fresh stage"),
            AttemptAdoption::Applied(_)
        ));
        let abort = plurx_core::domain::MediaSessionPreparationAbortRequest {
            staged_incarnation_id: p.incarnation_id.clone(),
            expected_predecessor_owner_node_id: p.expected_predecessor_owner_node_id.clone(),
            expected_predecessor_owner_epoch: p.expected_predecessor_owner_epoch,
            now_ms: p.now_ms + 1,
        };
        let ended = store
            .abort_media_session_preparation(&p.principal, &p.playback_id, &abort)
            .await
            .expect("existing abort")
            .expect("exact staged route");
        assert_eq!(ended.state, "ended");
        assert_eq!(
            store
                .media_session_route_for_playback(&p.principal, &p.playback_id)
                .await
                .expect("aborted stage leaves active pointer"),
            Some(incumbent)
        );
        let status = store
            .adaptive_request_status(&staged.binding.key, &staged.binding.principal)
            .await
            .expect("retained status")
            .expect("original winner");
        assert_eq!(status.disposition, RequestDisposition::Adopted);
        assert_eq!(status.attempts[0].phase, AttemptPhase::Winner);
        assert!(matches!(
            store.stage_adaptive_winner(&staged).await.expect("ended exact replay"),
            AttemptAdoption::Replay(outcome) if outcome.route.state == "ended"
        ));
    })
    .await;
}

#[tokio::test]
async fn staged_adopt_refuses_unproven_or_changed_viewer_commit() {
    for_each_adaptive_backend(|store, _| async move {
        for remove_receipt in [true, false] {
            let (_, staged, incumbent) = ready_staged_adoption(store.as_ref(), false).await;
            let p = &staged.preparation;
            assert!(matches!(
                store.stage_adaptive_winner(&staged).await.expect("stage"),
                AttemptAdoption::Applied(_)
            ));
            let mut commit = staged_viewer_commit(p, &incumbent);
            if remove_receipt {
                commit.control_receipt = None;
            } else {
                let changed = store
                    .record_desired_selection(
                        &p.principal,
                        &p.playback_id,
                        &"2".repeat(64),
                        "{}",
                        p.now_ms + 1,
                    )
                    .await
                    .expect("new desired ask");
                // Even a fresh read at the viewer commit cannot authorize a
                // generation that was prepared for a different ask.
                commit.expected_desired_revision = Some(changed.revision);
            }
            assert!(store
                .commit_media_session_preparation(&p.principal, &p.playback_id, &commit)
                .await
                .expect("rejected viewer commit")
                .is_none());
            assert_eq!(
                store
                    .media_session_route_for_playback(&p.principal, &p.playback_id)
                    .await
                    .expect("predecessor remains current"),
                Some(incumbent)
            );
            let held = store
                .adaptive_request_status(&staged.binding.key, &staged.binding.principal)
                .await
                .expect("original adaptive request")
                .expect("retained original winner");
            assert_eq!(held.disposition, RequestDisposition::Adopted);
            assert_eq!(held.attempts[0].phase, AttemptPhase::Winner);
        }
    })
    .await;
}
