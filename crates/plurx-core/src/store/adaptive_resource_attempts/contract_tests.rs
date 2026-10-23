//! Registered by the main-owned store_contract.rs seam. These are real Store
//! scenarios, not pure SQL-string assertions. Do not execute before final review.
use super::*;
use plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED;
use plurx_core::store::adaptive_resource_attempts::*;
use uuid::Uuid;

#[path = "contract_adversarial.rs"]
mod adversarial;

#[path = "restart_tests.rs"]
mod restart;

struct AttemptFixture {
    claim: AttemptClaim,
}

impl AttemptFixture {
    async fn new(store: &dyn Store) -> Self {
        assert_eq!(
            store.adaptive_attempt_schema().await.expect("authority"),
            AttemptSchemaReadiness::Ready
        );
        let id = Uuid::new_v4();
        let user = store
            .create_user(
                &format!("attempt-{id}"),
                "synthetic-unused-password-hash",
                false,
            )
            .await
            .expect("local user");
        let key = RequestKey {
            request_id: format!("adaptive-{id}"),
            request_incarnation_id: Uuid::new_v4(),
        };
        let binding = RequestBinding {
            version: 1,
            key: key.clone(),
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                user_id: user.id,
            },
            request_fingerprint: "a".repeat(64),
            playback_id: Uuid::new_v4().to_string(),
            recovery_epoch: Uuid::new_v4(),
            planning_generation: 7,
            planning_source_digest: "b".repeat(64),
            settings_digest: "c".repeat(64),
            intent_digest: "d".repeat(64),
            original_deadline_ms: 20_000,
            expected_predecessor_incarnation_id: None,
            expected_predecessor_owner_epoch: None,
            expected_desired_revision: None,
        };
        assert!(matches!(
            store
                .claim_media_session_request(
                    &binding.principal,
                    &key.request_id,
                    &binding.request_fingerprint,
                    &binding.playback_id,
                    &key.request_incarnation_id.to_string(),
                    10_000,
                    binding.original_deadline_ms
                )
                .await
                .expect("root claim"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        let key = AttemptKey {
            request: key,
            attempt_id: Uuid::new_v4(),
            route_incarnation_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            worker_node_id: format!("worker-{id}"),
            owner_epoch: 1,
            ordinal: 1,
        };
        Self {
            claim: AttemptClaim {
                context: MutationContext {
                    operation_id: Uuid::new_v4(),
                    expected_request_sequence: 0,
                    expected_attempt_sequence: 0,
                    now_ms: 10_001,
                },
                binding,
                key,
                worker_instance_id: Uuid::new_v4(),
                serving_generation: u64::MAX,
                domains: vec![format!("processing:{id}")],
            },
        }
    }

    fn prepared(&self, status: &RequestStatus) -> PreparedAttempt {
        let attempt = status
            .attempts
            .iter()
            .find(|a| a.key == self.claim.key)
            .expect("exact attempt");
        PreparedAttempt {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: status.sequence,
                expected_attempt_sequence: attempt.sequence,
                now_ms: 10_002,
            },
            binding: self.claim.binding.clone(),
            key: self.claim.key.clone(),
            worker: WorkerBinding {
                node_id: self.claim.key.worker_node_id.clone(),
                worker_instance_id: self.claim.worker_instance_id,
                serving_generation: self.claim.serving_generation,
                planning_generation: self.claim.binding.planning_generation,
                planning_source_digest: self.claim.binding.planning_source_digest.clone(),
                selected_recipe_digest: "e".repeat(64),
                prepared_handle_digest: "f".repeat(64),
                output_digest: "0".repeat(64),
                prepared_expires_at_ms: 19_999,
            },
        }
    }

    fn second(&self, status: &RequestStatus, overlap: bool) -> AttemptClaim {
        let mut claim = self.claim.clone();
        claim.context.operation_id = Uuid::new_v4();
        claim.context.expected_request_sequence = status.sequence;
        claim.key.attempt_id = Uuid::new_v4();
        claim.key.route_incarnation_id = Uuid::new_v4();
        claim.key.session_id = Uuid::new_v4();
        claim.key.worker_node_id = Uuid::new_v4().to_string();
        claim.key.ordinal = status.next_ordinal;
        claim.key.owner_epoch = i64::from(claim.key.ordinal);
        claim.worker_instance_id = Uuid::new_v4();
        if !overlap {
            claim.domains = vec![format!("processing:{}", claim.key.worker_node_id)];
        }
        claim
    }
}

fn applied(result: AttemptMutation) -> RequestStatus {
    match result {
        AttemptMutation::Applied(status) => status,
        other => panic!("expected actual durable transition, got {other:?}"),
    }
}

#[tokio::test]
async fn provisional_route_precedes_spawn_and_is_never_serveable() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let status = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("commit before spawn"),
        );
        assert_eq!(status.attempts[0].phase, AttemptPhase::Provisional);
        assert_eq!(status.attempts[0].serving_generation, u64::MAX);
        let route = store
            .media_session_route_by_incarnation(&fixture.claim.key.route_incarnation_id.to_string())
            .await
            .expect("route")
            .expect("provisional row");
        assert_eq!(route.state, "starting");
        assert_eq!(route.owner_epoch, 1);
        assert_eq!(
            route.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );
        assert!(store
            .media_session_route_for_playback(
                &fixture.claim.binding.principal,
                &fixture.claim.binding.playback_id
            )
            .await
            .expect("pointer")
            .is_none());
        assert!(matches!(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("exact retry"),
            AttemptMutation::Replay(_)
        ));
        let mut changed = fixture.claim.clone();
        changed.serving_generation = 1;
        assert_eq!(
            store
                .claim_adaptive_attempt(&changed)
                .await
                .expect("conflicting retry"),
            AttemptMutation::Refused(AttemptRefusal::Conflict)
        );
    })
    .await;
}

#[tokio::test]
async fn duplicate_prepare_reuses_exact_worker_custody() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let status = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("claim"),
        );
        let prepared = fixture.prepared(&status);
        let sealed = applied(
            store
                .record_adaptive_prepared(&prepared)
                .await
                .expect("seal"),
        );
        assert_eq!(sealed.attempts.len(), 1);
        assert_eq!(sealed.attempts[0].worker.as_ref(), Some(&prepared.worker));
        assert!(matches!(
            store
                .record_adaptive_prepared(&prepared)
                .await
                .expect("replay"),
            AttemptMutation::Replay(_)
        ));
        let mut changed = prepared.clone();
        changed.worker.output_digest = "1".repeat(64);
        assert_eq!(
            store
                .record_adaptive_prepared(&changed)
                .await
                .expect("mismatch"),
            AttemptMutation::Refused(AttemptRefusal::Conflict)
        );
    })
    .await;
}

#[tokio::test]
async fn two_attempts_have_one_winner_and_no_partial_activation() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let first = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("first"),
        );
        assert_eq!(
            store
                .claim_adaptive_attempt(&fixture.second(&first, true))
                .await
                .expect("overlap"),
            AttemptMutation::Refused(AttemptRefusal::Bound)
        );
        let second = fixture.second(&first, false);
        let two = applied(
            store
                .claim_adaptive_attempt(&second)
                .await
                .expect("independent admitted domains"),
        );
        assert_eq!(
            store
                .claim_adaptive_attempt(&fixture.second(&two, false))
                .await
                .expect("third unresolved"),
            AttemptMutation::Refused(AttemptRefusal::Bound)
        );
        let prepared = fixture.prepared(&two);
        let sealed = applied(
            store
                .record_adaptive_prepared(&prepared)
                .await
                .expect("prepared"),
        );
        let selection = WinnerSelection {
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
        let won = applied(
            store
                .select_adaptive_winner(&selection)
                .await
                .expect("one winner"),
        );
        assert_eq!(won.winner_attempt_id, Some(fixture.claim.key.attempt_id));
        assert_eq!(won.attempts[1].phase, AttemptPhase::Losing);
        assert!(store
            .media_session_route_for_playback(
                &fixture.claim.binding.principal,
                &fixture.claim.binding.playback_id
            )
            .await
            .expect("still no pointer")
            .is_none());
        let inventory = store
            .unclosed_adaptive_attempts(&AttemptInventoryQuery {
                worker_node_id: second.key.worker_node_id,
                after_attempt_id: None,
                limit: 1,
            })
            .await
            .expect("losing cleanup debt");
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].attempt.phase, AttemptPhase::Losing);
    })
    .await;
}

#[tokio::test]
async fn original_deadline_and_three_worker_bound_survive_retries_and_restart() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let original = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("durable original"),
        );
        let mut late = fixture.claim.clone();
        late.context.operation_id = Uuid::new_v4();
        late.context.now_ms = 20_001;
        assert_eq!(
            store.claim_adaptive_attempt(&late).await.expect("late"),
            AttemptMutation::Refused(AttemptRefusal::Expired)
        );
        let mut extended = fixture.claim.clone();
        extended.context.operation_id = Uuid::new_v4();
        extended.binding.original_deadline_ms = 30_000;
        assert_eq!(
            store
                .claim_adaptive_attempt(&extended)
                .await
                .expect("immutable deadline"),
            AttemptMutation::Refused(AttemptRefusal::Conflict)
        );
        assert_eq!(
            store
                .adaptive_request_status(
                    &fixture.claim.binding.key,
                    &fixture.claim.binding.principal
                )
                .await
                .expect("fresh authoritative reader"),
            Some(original)
        );
        let mut fourth = fixture.claim.clone();
        fourth.key.ordinal = 4;
        fourth.key.owner_epoch = 4;
        assert!(store.claim_adaptive_attempt(&fourth).await.is_err());
    })
    .await;
}

#[tokio::test]
async fn drain_includes_every_unclosed_attempt() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let initial = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("claim"),
        );
        let fence = AttemptFence {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: initial.sequence,
                expected_attempt_sequence: initial.attempts[0].sequence,
                now_ms: 30_000,
            },
            binding: fixture.claim.binding.clone(),
            key: fixture.claim.key.clone(),
            cancel_request: true,
        };
        let cleanup = applied(
            store
                .fence_adaptive_attempt(&fence)
                .await
                .expect("expiry retains debt"),
        );
        assert_eq!(cleanup.attempts[0].phase, AttemptPhase::Cleanup);
        assert_eq!(cleanup.disposition, RequestDisposition::Cancelled);
        let page = store
            .unclosed_adaptive_attempts(&AttemptInventoryQuery {
                worker_node_id: fixture.claim.key.worker_node_id.clone(),
                after_attempt_id: None,
                limit: 128,
            })
            .await
            .expect("all age-independent obligations");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].attempt.phase, AttemptPhase::Cleanup);
        assert!(matches!(
            store
                .fence_adaptive_attempt(&fence)
                .await
                .expect("exact cleanup retry"),
            AttemptMutation::Replay(_)
        ));
    })
    .await;
}

#[tokio::test]
async fn closed_cleanup_requires_exact_worker_and_prior_fence_then_replays() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let claimed = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("claim"),
        );
        let prepared = fixture.prepared(&claimed);
        let sealed = applied(
            store
                .record_adaptive_prepared(&prepared)
                .await
                .expect("prepared"),
        );
        let selected = applied(
            store
                .select_adaptive_winner(&WinnerSelection {
                    context: MutationContext {
                        operation_id: Uuid::new_v4(),
                        expected_request_sequence: sealed.sequence,
                        expected_attempt_sequence: sealed.attempts[0].sequence,
                        now_ms: 10_003,
                    },
                    binding: prepared.binding.clone(),
                    key: prepared.key.clone(),
                    worker: prepared.worker.clone(),
                })
                .await
                .expect("selected winner"),
        );
        let fence = AttemptFence {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: selected.sequence,
                expected_attempt_sequence: selected.attempts[0].sequence,
                now_ms: 20_001,
            },
            binding: fixture.claim.binding.clone(),
            key: fixture.claim.key.clone(),
            cancel_request: true,
        };
        let held = applied(store.fence_adaptive_attempt(&fence).await.expect("fence"));
        let mut close = AttemptClose {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: held.sequence,
                expected_attempt_sequence: held.attempts[0].sequence,
                now_ms: 20_002,
            },
            binding: fixture.claim.binding.clone(),
            key: fixture.claim.key.clone(),
            worker_instance_id: fixture.claim.worker_instance_id,
            serving_generation: fixture.claim.serving_generation,
            cleanup_operation_id: fence.context.operation_id,
        };
        let original = close.clone();
        close.worker_instance_id = Uuid::new_v4();
        assert_eq!(
            store
                .close_adaptive_attempt(&close)
                .await
                .expect("wrong worker"),
            AttemptMutation::Refused(AttemptRefusal::StaleBinding)
        );
        close = original.clone();
        close.cleanup_operation_id = Uuid::new_v4();
        assert_eq!(
            store
                .close_adaptive_attempt(&close)
                .await
                .expect("wrong fence"),
            AttemptMutation::Refused(AttemptRefusal::StaleBinding)
        );
        close = original.clone();
        close.context.operation_id = fence.context.operation_id;
        assert_eq!(
            store
                .close_adaptive_attempt(&close)
                .await
                .expect("old action UUID collision"),
            AttemptMutation::Refused(AttemptRefusal::Conflict)
        );
        let closed = applied(
            store
                .close_adaptive_attempt(&original)
                .await
                .expect("logical close"),
        );
        assert_eq!(closed.attempts[0].phase, AttemptPhase::Closed);
        assert_eq!(closed.disposition, RequestDisposition::Cancelled);
        assert!(matches!(
            store
                .close_adaptive_attempt(&original)
                .await
                .expect("same operation"),
            AttemptMutation::Replay(_)
        ));
        let mut future_claim = fixture.second(&closed, false);
        future_claim.context.operation_id = original.context.operation_id;
        assert_eq!(
            store
                .claim_adaptive_attempt(&future_claim)
                .await
                .expect("new action UUID collision"),
            AttemptMutation::Refused(AttemptRefusal::Conflict)
        );
        let inventory = store
            .unclosed_adaptive_attempts(&AttemptInventoryQuery {
                worker_node_id: fixture.claim.key.worker_node_id.clone(),
                after_attempt_id: None,
                limit: 128,
            })
            .await
            .expect("closed excluded");
        assert!(inventory.entries.is_empty());
    })
    .await;
}

#[tokio::test]
async fn adopted_winner_does_not_block_exact_losing_worker_close() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let first = applied(
            store
                .claim_adaptive_attempt(&fixture.claim)
                .await
                .expect("first"),
        );
        let second = fixture.second(&first, false);
        let two = applied(store.claim_adaptive_attempt(&second).await.expect("second"));
        let mut prepared = fixture.prepared(&two);
        prepared.worker.selected_recipe_digest = selected_recipe_digest("{}").expect("recipe");
        let sealed = applied(
            store
                .record_adaptive_prepared(&prepared)
                .await
                .expect("prepared"),
        );
        let selection = WinnerSelection {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: sealed.sequence,
                expected_attempt_sequence: sealed.attempts[0].sequence,
                now_ms: 10_003,
            },
            binding: prepared.binding.clone(),
            key: prepared.key.clone(),
            worker: prepared.worker.clone(),
        };
        let won = applied(
            store
                .select_adaptive_winner(&selection)
                .await
                .expect("winner"),
        );
        let adoption = adoption_from_won(prepared, won, "{}");
        assert!(matches!(
            store.adopt_adaptive_winner(&adoption).await.expect("adopt"),
            AttemptAdoption::Applied(_)
        ));
        let adopted = store
            .adaptive_request_status(&fixture.claim.binding.key, &fixture.claim.binding.principal)
            .await
            .expect("status")
            .expect("request");
        assert_eq!(adopted.disposition, RequestDisposition::Adopted);
        let losing = adopted
            .attempts
            .iter()
            .find(|attempt| attempt.key == second.key)
            .expect("loser");
        assert_eq!(losing.phase, AttemptPhase::Losing);
        let close = AttemptClose {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: adopted.sequence,
                expected_attempt_sequence: losing.sequence,
                now_ms: 10_005,
            },
            binding: second.binding.clone(),
            key: second.key.clone(),
            worker_instance_id: second.worker_instance_id,
            serving_generation: second.serving_generation,
            cleanup_operation_id: selection.context.operation_id,
        };
        let closed = applied(
            store
                .close_adaptive_attempt(&close)
                .await
                .expect("loser close"),
        );
        assert_eq!(closed.disposition, RequestDisposition::Adopted);
        assert_eq!(closed.winner_attempt_id, Some(fixture.claim.key.attempt_id));
        assert_eq!(closed.attempts[1].phase, AttemptPhase::Closed);
        let mut forbidden = close;
        forbidden.context.operation_id = Uuid::new_v4();
        forbidden.context.expected_request_sequence = closed.sequence;
        forbidden.context.expected_attempt_sequence = closed.attempts[0].sequence;
        forbidden.key = fixture.claim.key.clone();
        forbidden.worker_instance_id = fixture.claim.worker_instance_id;
        forbidden.serving_generation = fixture.claim.serving_generation;
        forbidden.cleanup_operation_id = closed.attempts[0].last_operation_id;
        assert_eq!(
            store
                .close_adaptive_attempt(&forbidden)
                .await
                .expect("winner stays active"),
            AttemptMutation::Refused(AttemptRefusal::WinnerLost)
        );
    })
    .await;
}

async fn ready_adoption(store: &dyn Store, fixture: &AttemptFixture) -> AdaptiveAdoption {
    ready_adoption_recipe(store, fixture, "{}").await
}

async fn ready_adoption_recipe(
    store: &dyn Store,
    fixture: &AttemptFixture,
    recipe: &str,
) -> AdaptiveAdoption {
    let claimed = applied(
        store
            .claim_adaptive_attempt(&fixture.claim)
            .await
            .expect("provisional"),
    );
    let mut prepared = fixture.prepared(&claimed);
    prepared.worker.selected_recipe_digest =
        selected_recipe_digest(recipe).expect("bounded canonical recipe");
    let sealed = applied(
        store
            .record_adaptive_prepared(&prepared)
            .await
            .expect("prepare"),
    );
    let selection = WinnerSelection {
        context: MutationContext {
            operation_id: Uuid::new_v4(),
            expected_request_sequence: sealed.sequence,
            expected_attempt_sequence: sealed.attempts[0].sequence,
            now_ms: 10_003,
        },
        binding: prepared.binding.clone(),
        key: prepared.key.clone(),
        worker: prepared.worker.clone(),
    };
    let won = applied(
        store
            .select_adaptive_winner(&selection)
            .await
            .expect("winner"),
    );
    adoption_from_won(prepared, won, recipe)
}

fn adoption_from_won(
    prepared: PreparedAttempt,
    won: RequestStatus,
    recipe: &str,
) -> AdaptiveAdoption {
    AdaptiveAdoption {
        context: MutationContext {
            operation_id: Uuid::new_v4(),
            expected_request_sequence: won.sequence,
            expected_attempt_sequence: won.attempts[0].sequence,
            now_ms: 10_004,
        },
        binding: prepared.binding.clone(),
        key: prepared.key.clone(),
        worker: prepared.worker,
        activation: MediaSessionActivation {
            incarnation_id: prepared.key.route_incarnation_id.to_string(),
            session_id: prepared.key.session_id.to_string(),
            principal: prepared.binding.principal,
            playback_id: prepared.binding.playback_id,
            recovery_epoch: prepared.binding.recovery_epoch.to_string(),
            expected_predecessor_incarnation_id: prepared
                .binding
                .expected_predecessor_incarnation_id
                .map(|id| id.to_string()),
            fence_predecessor: true,
            request_id: Some(prepared.binding.key.request_id),
            request_fingerprint: prepared.binding.request_fingerprint,
            owner_node_id: prepared.key.worker_node_id,
            recipe_json: recipe.into(),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: 10_004,
            lease_expires_at_ms: 30_000,
            expected_desired_revision: prepared.binding.expected_desired_revision,
        },
    }
}

#[tokio::test]
async fn adopt_rechecks_winner_epoch_predecessor_and_maintenance() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let adoption = ready_adoption(store.as_ref(), &fixture).await;
        let mut wrong = adoption.clone();
        wrong.activation.expected_predecessor_incarnation_id = Some(Uuid::new_v4().to_string());
        assert_eq!(
            store
                .adopt_adaptive_winner(&wrong)
                .await
                .expect("overlapping field mismatch"),
            AttemptAdoption::Refused(AttemptRefusal::Conflict)
        );
        let mut wrong_epoch = adoption.clone();
        wrong_epoch.key.owner_epoch = 2;
        assert!(store.adopt_adaptive_winner(&wrong_epoch).await.is_err());
        let committed = match store
            .adopt_adaptive_winner(&adoption)
            .await
            .expect("atomic adoption")
        {
            AttemptAdoption::Applied(outcome) => outcome,
            other => panic!("{other:?}"),
        };
        assert_eq!(committed.route.state, "active");
        assert_eq!(
            committed.route.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );
        assert_eq!(committed.route.owner_epoch, adoption.key.owner_epoch);
        assert!(matches!(
            store
                .adopt_adaptive_winner(&adoption)
                .await
                .expect("complete exact replay"),
            AttemptAdoption::Replay(_)
        ));
        let mut changed = adoption.clone();
        changed.activation.response_json = "{\"changed\":true}".into();
        assert_eq!(
            store
                .adopt_adaptive_winner(&changed)
                .await
                .expect("replay cannot use pointer to bypass payload"),
            AttemptAdoption::Refused(AttemptRefusal::Conflict)
        );
        let status = store
            .adaptive_request_status(&fixture.claim.binding.key, &fixture.claim.binding.principal)
            .await
            .expect("original identity survives projection")
            .expect("request");
        assert_eq!(status.disposition, RequestDisposition::Adopted);
        let page = store
            .unclosed_adaptive_attempts(&AttemptInventoryQuery {
                worker_node_id: adoption.key.worker_node_id,
                after_attempt_id: None,
                limit: 128,
            })
            .await
            .expect("adopted is still physical debt");
        assert_eq!(page.entries.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn expired_preparation_stays_held_until_confirmed_reap() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let mut adoption = ready_adoption(store.as_ref(), &fixture).await;
        adoption.context.now_ms = 20_001;
        adoption.activation.now_ms = 20_001;
        assert_eq!(
            store
                .adopt_adaptive_winner(&adoption)
                .await
                .expect("deadline refusal"),
            AttemptAdoption::Refused(AttemptRefusal::Expired)
        );
        let page = store
            .unclosed_adaptive_attempts(&AttemptInventoryQuery {
                worker_node_id: adoption.key.worker_node_id,
                after_attempt_id: None,
                limit: 128,
            })
            .await
            .expect("no age filter");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].attempt.phase, AttemptPhase::Winner);
        assert!(store
            .media_session_route_for_playback(
                &fixture.claim.binding.principal,
                &fixture.claim.binding.playback_id
            )
            .await
            .expect("no late pointer")
            .is_none());
    })
    .await;
}

async fn raw_refused(raw: &StrandedFieldOrder, sql: &str) -> bool {
    match raw {
        StrandedFieldOrder::Sqlite(path) => rusqlite::Connection::open(path)
            .expect("independent legacy writer")
            .execute_batch(sql)
            .is_err(),
        #[cfg(feature = "hiqlite-contract-tests")]
        StrandedFieldOrder::Hiqlite(client) => match client.batch(sql.to_owned()).await {
            Err(_) => true,
            Ok(results) => results.iter().any(Result::is_err),
        },
    }
}

#[tokio::test]
async fn legacy_writer_cannot_reacquire_or_activate_a_live_attempt() {
    for_each_strandable_backend(|store,_,raw|async move {
        let fixture=AttemptFixture::new(store.as_ref()).await;
        let before=applied(store.claim_adaptive_attempt(&fixture.claim).await.expect("claim"));
        for sql in [
            format!("UPDATE media_session_requests SET claim_expires_at_ms=999999 WHERE request_id='{}'",fixture.claim.binding.key.request_id),
            format!("UPDATE media_sessions SET state='active',publication_ready_at_ms=0 WHERE incarnation_id='{}'",fixture.claim.key.route_incarnation_id),
            format!("DELETE FROM media_sessions WHERE incarnation_id='{}'",fixture.claim.key.route_incarnation_id),
            format!("DELETE FROM job_leases WHERE resource='session:{}'",fixture.claim.key.route_incarnation_id),
            format!("UPDATE job_leases SET fence=2 WHERE resource='session:{}'",fixture.claim.key.route_incarnation_id),
        ] {assert!(raw_refused(raw.as_ref(),&sql).await,"old SQL must refuse: {sql}");}
        assert_eq!(store.adaptive_request_status(&fixture.claim.binding.key,&fixture.claim.binding.principal).await.expect("debt remains"),Some(before));
    }).await;
}
