use super::*;

fn identity(sequence: u64) -> QualityIntentIdentity {
    QualityIntentIdentity {
        generation: uuid::Uuid::new_v4().to_string(),
        control_epoch: 1,
        client_instance_id: uuid::Uuid::new_v4().to_string(),
        lifetime_id: "quality-test".to_owned(),
        recipe_revision: 1,
        accepted_sequence: sequence,
    }
}

fn cached_wait_request(generation: String) -> crate::playback_control::ControlRequestV1 {
    crate::playback_control::ControlRequestV1 {
        intent: None,
        protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
        generation,
        control_epoch: 1,
        client_instance_id: uuid::Uuid::new_v4().to_string(),
        sequence: 1,
        demand: crate::playback_control::PlaybackDemand::Active,
        position_ms: 1_000,
        buffered_from_ms: Some(0),
        buffered_through_ms: 10_000,
        playback_rate: 1.0,
        render_state: crate::playback_control::RenderState::Rendering,
        seek_target_ms: None,
        observed_download_bps: None,
        selection: crate::playback_control::ClientSelection {
            quality: crate::playback_control::QualitySelection::Auto {
                height: None,
                candidate_id: None,
            },
            audio_track: None,
            subtitle: crate::playback_control::SubtitleSelection {
                mode: crate::playback_control::SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: crate::playback_control::CodecPolicy::Auto,
            dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
        },
        capabilities: Some(crate::playback_control::DynamicCapabilities {
            presentation_target: None,
            decoder_caps: None,
            platform: crate::playback_control::ClientPlatform::Web,
            max_height: 1080,
            codecs: vec![crate::playback_control::CodecPolicy::H264],
            dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
            dual_player_preparation: false,
        }),
        observation: None,
        acknowledgement: None,
        supported_actions: None,
    }
}

#[tokio::test]
async fn cached_wait_preserves_registered_reader_and_its_pending_token_without_resurrection() {
    cached_wait_watcher_fixture(None).await;
}

#[tokio::test]
async fn cached_accepted_wait_survives_actual_foreground_watcher_capacity_pressure() {
    for revocation in [
        "deadline",
        "preparation_deadline",
        "unsealed",
        "unproved",
        "new_ask",
        "paused",
        "recipe_revision",
        "lifetime",
        "destination_revision",
        "transport_revision",
        "owner_epoch",
        "owner_incarnation",
        "reader_retired",
    ] {
        cached_wait_watcher_fixture(Some(revocation)).await;
    }
}

#[tokio::test]
async fn cached_accepted_wait_survives_contended_session_lock_during_foreground_pressure() {
    cached_wait_watcher_fixture(Some("contended_sessions")).await;
}

#[tokio::test]
async fn cached_accepted_wait_survives_contended_reader_lock_during_foreground_pressure() {
    cached_wait_watcher_fixture(Some("contended_readers")).await;
}

#[tokio::test]
async fn cached_accepted_wait_contention_keeps_control_witness_without_renewal() {
    cached_wait_watcher_fixture(Some("contended_control_witness")).await;
}

#[tokio::test]
async fn cached_accepted_wait_defers_playing_revocation_until_session_lock_releases() {
    cached_wait_watcher_fixture(Some("contended_sessions_playing")).await;
}

async fn cached_wait_watcher_fixture(watcher_revocation: Option<&str>) {
    let (_output_dir, serve, facts, output_request) =
        crate::vodserve::retained::test_post_attachment_output_facts().await;
    let work = crate::test_tempdir().expect("control fixture");
    let fixture =
        crate::transcode::HlsDeliveryFixture::publish(work.path(), "waiting-incumbent").await;
    let mut state = fixture.state.clone();
    state.transcode = Arc::new(
        crate::transcode::TranscodeManager::new(
            state.store.clone(),
            work.path().join("cached-recovery"),
            plurx_core::transcode::EncoderCaps::default(),
            plurx_core::transcode::Pipeline::Cpu,
        )
        .with_copy_test_vod(serve.clone()),
    );
    let playback = uuid::Uuid::new_v4().to_string();
    let mut route = crate::media_sessions::takeover_eligible_route(
        "incumbent",
        &uuid::Uuid::new_v4().to_string(),
    );
    route.playback_id = playback.clone();
    route.owner_node_id = state.node_id.clone();
    let mut request = cached_wait_request(route.incarnation_id.clone());
    let candidate = plurx_core::playback::candidate::CandidateId::for_recipe_digest([42; 32]);
    request.selection.quality = crate::playback_control::QualitySelection::Auto {
        height: Some(360),
        candidate_id: Some(candidate),
    };
    request.render_state = crate::playback_control::RenderState::Waiting;
    request.intent = Some(plurx_core::playback::MediaIntentEnvelope {
        lifetime_id: "cached-pressure-viewer".into(),
        recipe_revision: 3,
        destination_revision: 1,
        transport_revision: 1,
        selection: request.selection.desired(),
    });
    let pending = PendingCandidateGuard::begin_control(&state, &route, &request);
    pending
        .registered
        .store(true, std::sync::atomic::Ordering::Release);
    let id = "00000000-0000-4000-8000-00000000cace";
    let incarnation = uuid::Uuid::new_v4().to_string();
    serve.mark_prepared_incarnation(id, &incarnation).await;
    let output = facts
        .response_owner
        .retained_output_facts()
        .expect("real sealed complete artifact");
    let owner = serve
        .cached_prepared_reader_owner(id, &incarnation, &output)
        .await
        .expect("exact retained speculative reader");
    let mut recipe: RemoteStartRequest =
        serde_json::from_str(&route.recipe_json).expect("fixture recipe");
    recipe.request.file_id = output_request.file_id;
    recipe.request.kind = crate::transcode::SessionKind::Transcode { height: 360 };
    recipe.candidate_id = Some(candidate);
    let preparation = plurx_core::domain::MediaSessionPreparation {
        quality_cancellation_key: None,
        incarnation_id: incarnation.clone(),
        session_id: id.into(),
        principal: route.principal.clone(),
        playback_id: playback.clone(),
        expected_predecessor_incarnation_id: route.incarnation_id.clone(),
        expected_predecessor_owner_node_id: route.owner_node_id.clone(),
        expected_predecessor_owner_epoch: route.owner_epoch,
        request_fingerprint: "exact-cached-reader".into(),
        owner_node_id: state.node_id.clone(),
        recipe_json: serde_json::to_string(&recipe).expect("recipe"),
        response_json: "{}".into(),
        media_origin_ms: 0,
        now_ms: unix_ms(),
        expected_desired_revision: None,
        deadline_ms: unix_ms() + 30_000,
    };
    let mut active = ActivePreparedSuccessor {
        state: state.clone(),
        executor: crate::playback_control::PreparationExecutor::new(
            state.store.clone(),
            state
                .transcode
                .session_preparation_gate(id)
                .await
                .expect("exact cached gate"),
            route.principal.clone(),
            route.playback_id.clone(),
            route.owner_node_id.clone(),
            route.owner_epoch,
        ),
        preparation,
        purpose: PreparationPurpose::SelectionChange,
        cached_recovery_owner: None,
        cached_accepted_wait: None,
        planning_registration: Some(Arc::clone(&pending.registered)),
        quality_intent: QualityIntentIdentity::from_control(&request),
        cancelled: pending.cancellation_token(),
    };
    route.session_id = id.to_owned();
    serve
        .accept_cached_wait_for_test(&route, &request, &incarnation)
        .await;
    register_active_preparation(active.clone());
    record_cached_recovery_owner_if_registered(&active, (owner.clone(), false));
    request.sequence += 1;
    serve
        .accept_cached_wait_for_test(&route, &request, &incarnation)
        .await;
    cancel_preparations_for_incumbent_wait(&playback, Some((&route, &request, unix_ms() + 4_000)));
    assert!(
        !pending.cancelled(),
        "the actual Waiting path must preserve its keeper's still-live pending token"
    );
    assert!(!active.cancelled.is_cancelled());
    assert!(active_prepared_successors()
        .lock()
        .expect("registry")
        .contains_key(&incarnation));
    if let Some(revocation) = watcher_revocation {
        use crate::admission::Priority;
        let admissions = state.transcode.preparation_admissions_for_test();
        let held = admissions
            .try_admit_software(4, 4, Priority::Live)
            .expect("real occupied resource pool");
        let mut blocked = admissions.queue_for_slot();
        assert!(admissions
            .try_admit_software(4, 1, Priority::Live)
            .is_none());
        blocked.mark_capacity_blocked();
        assert!(state.transcode.foreground_media_waiting());
        // The captured clone has no cached proof/witness. Production must
        // consult the actual current entry after accepted wait publication.
        assert!(active.cached_recovery_owner.is_none());
        arm_preparation_foreground_watch(active.clone());
        tokio::time::sleep(Duration::from_millis(90)).await;
        assert!(
            active_preparation(&incarnation).is_some(),
            "real capacity pressure cannot abort the proved accepted keeper"
        );
        assert!(!pending.cancelled());
        if revocation == "contended_sessions_playing" {
            let original_deadline = active_preparation(&incarnation)
                .expect("cached keeper")
                .cached_accepted_wait
                .expect("existing witness")
                .deadline_unix_ms;
            request.sequence += 1;
            request.render_state = crate::playback_control::RenderState::Rendering;
            serve
                .accept_cached_wait_while_sessions_held_for_test(
                    &route,
                    &request,
                    &incarnation,
                    async {
                        // The actual accepted snapshot is now Playing, but both
                        // probes are unknown while this same sessions lock is held.
                        tokio::time::sleep(Duration::from_millis(90)).await;
                        let keeper =
                            active_preparation(&incarnation).expect("unknown retains keeper");
                        let witness = keeper.cached_accepted_wait.expect("existing witness only");
                        assert_eq!(witness.deadline_unix_ms, original_deadline);
                        assert!(witness.fence.still_live());
                        assert!(!pending.cancelled());
                    },
                )
                .await;
            tokio::time::timeout(Duration::from_millis(150), async {
                while active_preparation(&incarnation).is_some() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("Playing cancels on watcher polls after sessions lock release");
            assert!(pending.cancelled());
            drop(blocked);
            drop(held);
            assert!(!state.transcode.foreground_media_waiting());
            return;
        }
        if let Some(lock) = revocation.strip_prefix("contended_") {
            let original = active_preparation(&incarnation)
                .expect("cached keeper")
                .cached_accepted_wait
                .expect("existing accepted witness");
            let locks: &[&str] = if lock == "control_witness" {
                &["sessions", "readers"]
            } else {
                &[lock]
            };
            for lock in locks {
                serve
                    .while_cached_recovery_lock_held_for_test(id, lock, async {
                        // Both real session-table and real reader-table locks
                        // outlive at least two 25 ms watcher ticks under actual
                        // occupied live capacity, not a mocked liveness answer.
                        tokio::time::sleep(Duration::from_millis(90)).await;
                        assert!(active_preparation(&incarnation).is_some());
                        assert!(!pending.cancelled());
                        if revocation == "contended_control_witness" {
                            cancel_preparations_for_incumbent_wait(
                                &playback,
                                Some((&route, &request, unix_ms() + 20_000)),
                            );
                            let kept = active_preparation(&incarnation)
                                .expect("busy keeper")
                                .cached_accepted_wait
                                .expect("retained witness");
                            assert_eq!(
                                kept.deadline_unix_ms, original.deadline_unix_ms,
                                "unknown must not renew an existing witness"
                            );
                            assert!(kept.fence.still_live());
                            assert!(!pending.cancelled());
                        }
                    })
                    .await;
            }
            // A definite accepted Playing observation must still revoke
            // while the independent reader probe is unknown.
            request.sequence += 1;
            request.render_state = crate::playback_control::RenderState::Rendering;
            serve
                .accept_cached_wait_for_test(&route, &request, &incarnation)
                .await;
            serve
                .while_cached_recovery_lock_held_for_test(id, "readers", async {
                    tokio::time::timeout(Duration::from_millis(150), async {
                        while active_preparation(&incarnation).is_some() {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    })
                    .await
                    .expect("next watcher polls cancel definite Playing even with busy reader");
                    assert!(pending.cancelled());
                })
                .await;
            drop(blocked);
            drop(held);
            assert!(!state.transcode.foreground_media_waiting());
            return;
        }
        // Acceptance is visible before the HTTP path can refresh the
        // witness. An ordinary same-intent Waiting heartbeat must survive
        // this exact window even though its sequence and position advance.
        let delayed_request = request.clone();
        let original_witness_deadline = active_preparation(&incarnation)
            .expect("keeper")
            .cached_accepted_wait
            .expect("witness")
            .deadline_unix_ms;
        request.sequence += 1;
        request.position_ms += 50;
        serve
            .accept_cached_wait_for_test(&route, &request, &incarnation)
            .await;
        tokio::time::sleep(Duration::from_millis(90)).await;
        assert!(
            active_preparation(&incarnation).is_some(),
            "same-intent accepted Waiting must not need witness refresh"
        );
        assert!(!pending.cancelled());
        // N's delayed continuation cannot clear or renew the witness
        // after N+1 is accepted, either before or after N+1 refreshes it.
        cancel_preparations_for_incumbent_wait(
            &playback,
            Some((&route, &delayed_request, original_witness_deadline)),
        );
        assert_eq!(
            active_preparation(&incarnation)
                .expect("keeper")
                .cached_accepted_wait
                .expect("witness")
                .deadline_unix_ms,
            original_witness_deadline
        );
        let current_deadline = unix_ms() + 4_000;
        cancel_preparations_for_incumbent_wait(
            &playback,
            Some((&route, &request, current_deadline)),
        );
        let renewed_deadline = active_preparation(&incarnation)
            .expect("keeper")
            .cached_accepted_wait
            .expect("renewed witness")
            .deadline_unix_ms;
        assert_eq!(
            renewed_deadline,
            current_deadline.min(active.preparation.deadline_ms)
        );
        assert!(
            renewed_deadline > original_witness_deadline,
            "current accepted heartbeat may renew its own bounded witness"
        );
        cancel_preparations_for_incumbent_wait(
            &playback,
            Some((&route, &delayed_request, original_witness_deadline)),
        );
        assert_eq!(
            active_preparation(&incarnation)
                .expect("keeper")
                .cached_accepted_wait
                .expect("witness")
                .deadline_unix_ms,
            renewed_deadline
        );
        assert!(!pending.cancelled());
        // A stale different-intent/old-owner continuation cannot overrule
        // a fresh valid current keeper. It cannot renew that witness.
        let mut different_intent = delayed_request.clone();
        different_intent
            .intent
            .as_mut()
            .expect("old intent")
            .lifetime_id
            .push_str("-old");
        different_intent
            .intent
            .as_mut()
            .expect("old intent")
            .recipe_revision += 1;
        different_intent.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 720 };
        different_intent
            .intent
            .as_mut()
            .expect("old intent")
            .selection = different_intent.selection.desired();
        let mut old_owner = route.clone();
        old_owner.owner_node_id.push_str("-old");
        cancel_preparations_for_incumbent_wait(
            &playback,
            Some((&route, &different_intent, original_witness_deadline)),
        );
        cancel_preparations_for_incumbent_wait(
            &playback,
            Some((&old_owner, &different_intent, original_witness_deadline)),
        );
        assert_eq!(
            active_preparation(&incarnation)
                .expect("current keeper")
                .cached_accepted_wait
                .expect("current witness")
                .deadline_unix_ms,
            renewed_deadline
        );
        assert!(!pending.cancelled());
        assert!(serve
            .playlist(id)
            .await
            .expect("live cached publication")
            .result
            .is_ok());
        match revocation {
            "deadline" => {
                active_prepared_successors()
                    .lock()
                    .expect("registry")
                    .get_mut(&incarnation)
                    .expect("keeper")
                    .cached_accepted_wait
                    .as_mut()
                    .expect("accepted witness")
                    .deadline_unix_ms = unix_ms();
            }
            "preparation_deadline" => {
                active_prepared_successors()
                    .lock()
                    .expect("registry")
                    .get_mut(&incarnation)
                    .expect("keeper")
                    .preparation
                    .deadline_ms = unix_ms();
            }
            "unproved" => {
                active_prepared_successors()
                    .lock()
                    .expect("registry")
                    .get_mut(&incarnation)
                    .expect("keeper")
                    .cached_accepted_wait = None;
            }
            "reader_retired" => {
                serve.end(id, crate::vodserve::Terminal::Deleted).await;
            }
            "owner_incarnation" => {
                active_prepared_successors()
                    .lock()
                    .expect("registry")
                    .get_mut(&incarnation)
                    .expect("keeper")
                    .cached_accepted_wait
                    .as_mut()
                    .expect("accepted witness")
                    .route
                    .incarnation_id = uuid::Uuid::new_v4().to_string();
            }
            "unsealed" => {
                active_prepared_successors()
                    .lock()
                    .expect("registry")
                    .get_mut(&incarnation)
                    .expect("keeper")
                    .cached_recovery_owner = None;
            }
            "paused" => {
                request.sequence += 1;
                request.demand = crate::playback_control::PlaybackDemand::Hold;
                request.render_state = crate::playback_control::RenderState::Rendering;
                serve
                    .accept_cached_wait_for_test(&route, &request, &incarnation)
                    .await;
                cancel_preparations_for_incumbent_wait(
                    &playback,
                    Some((&route, &delayed_request, original_witness_deadline)),
                );
                assert!(
                    active_preparation(&incarnation).is_none(),
                    "old waiting continuation cannot preserve current non-wait demand"
                );
            }
            "new_ask" => {
                request.sequence += 1;
                request.intent.as_mut().expect("intent").recipe_revision += 1;
                request.selection.quality =
                    crate::playback_control::QualitySelection::Manual { height: 720 };
                request.intent.as_mut().expect("intent").selection = request.selection.desired();
                serve
                    .accept_cached_wait_for_test(&route, &request, &incarnation)
                    .await;
                cancel_preparations_for_incumbent_wait(
                    &playback,
                    Some((&route, &delayed_request, original_witness_deadline)),
                );
                assert!(
                    active_preparation(&incarnation).is_none(),
                    "old continuation cannot preserve a current new ask"
                );
            }
            "recipe_revision" | "lifetime" | "destination_revision" | "transport_revision" => {
                request.sequence += 1;
                let intent = request.intent.as_mut().expect("intent");
                match revocation {
                    "recipe_revision" => intent.recipe_revision += 1,
                    "lifetime" => intent.lifetime_id = uuid::Uuid::new_v4().to_string(),
                    "destination_revision" => intent.destination_revision += 1,
                    "transport_revision" => intent.transport_revision += 1,
                    _ => unreachable!("finite intent revisions"),
                }
                // The desired selection and existing observation fence
                // stay equal. Current accepted envelope must still revoke
                // the keeper before the HTTP cancellation helper runs.
                serve
                    .accept_cached_wait_for_test(&route, &request, &incarnation)
                    .await;
            }
            "owner_epoch" => {
                route.owner_epoch += 1;
                request.control_epoch += 1;
                request.sequence = 1;
                serve
                    .accept_cached_wait_for_test(&route, &request, &incarnation)
                    .await;
            }
            _ => unreachable!("finite watcher revocations"),
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while active_preparation(&incarnation).is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual watcher retires revoked keeper");
        drop(blocked);
        drop(held);
        assert!(!state.transcode.foreground_media_waiting());
        return;
    }
    // A newly accepted revision cannot use an older keeper witness.
    request.intent.as_mut().expect("intent").recipe_revision += 1;
    request.sequence += 1;
    serve
        .accept_cached_wait_for_test(&route, &request, &incarnation)
        .await;
    cancel_preparations_for_incumbent_wait(&playback, Some((&route, &request, unix_ms() + 4_000)));
    assert!(pending.cancelled());
    assert!(!active_prepared_successors()
        .lock()
        .expect("registry")
        .contains_key(&incarnation));
    record_cached_recovery_owner_if_registered(&active, (owner.clone(), false));
    assert!(
        !active_prepared_successors()
            .lock()
            .expect("registry")
            .contains_key(&incarnation),
        "a late sealed proof cannot resurrect a cancelled registration"
    );
    // Unprimed work has no proof even if it happens to name cached metadata.
    let cold_pending = PendingCandidateGuard::begin_control(&state, &route, &request);
    active.cancelled = cold_pending.cancellation_token();
    active.quality_intent = QualityIntentIdentity::from_control(&request);
    active.planning_registration = Some(Arc::clone(&cold_pending.registered));
    active.cached_recovery_owner = None;
    register_active_preparation(active.clone());
    cancel_preparations_for_incumbent_wait(&playback, Some((&route, &request, unix_ms() + 4_000)));
    assert!(cold_pending.cancelled());
    assert!(!active_prepared_successors()
        .lock()
        .expect("registry")
        .contains_key(&incarnation));
    for quality in [
        crate::playback_control::QualitySelection::Manual { height: 360 },
        crate::playback_control::QualitySelection::Auto {
            height: None,
            candidate_id: None,
        },
    ] {
        let mut unsupported = request.clone();
        unsupported.selection.quality = quality;
        unsupported.intent.as_mut().expect("intent").selection = unsupported.selection.desired();
        let other = PendingCandidateGuard::begin_control(&state, &route, &unsupported);
        active.cancelled = other.cancellation_token();
        active.planning_registration = Some(Arc::clone(&other.registered));
        active.quality_intent = QualityIntentIdentity::from_control(&unsupported);
        active.cached_recovery_owner = Some(CachedRecoveryOwner {
            owner: owner.clone(),
            bootstrap: false,
        });
        register_active_preparation(active.clone());
        cancel_preparations_for_incumbent_wait(
            &playback,
            Some((&route, &unsupported, unix_ms() + 4_000)),
        );
        assert!(
            other.cancelled(),
            "manual and unnamed non-bootstrap Auto do not inherit pressure immunity"
        );
        assert!(!active_prepared_successors()
            .lock()
            .expect("registry")
            .contains_key(&incarnation));
    }
    // User cancellation remains authoritative for a live cached keeper.
    active.quality_intent = QualityIntentIdentity::from_control(&request);
    let user_pending = PendingCandidateGuard::begin_control(&state, &route, &request);
    active.cancelled = user_pending.cancellation_token();
    active.planning_registration = Some(Arc::clone(&user_pending.registered));
    active.cached_recovery_owner = Some(CachedRecoveryOwner {
        owner,
        bootstrap: false,
    });
    register_active_preparation(active.clone());
    assert!(cancel_quality_preparation(
        &playback,
        active.quality_intent.as_ref().expect("identity")
    ));
    assert!(active.cancelled.is_cancelled());
    assert!(!active_prepared_successors()
        .lock()
        .expect("registry")
        .contains_key(&incarnation));
}

#[test]
fn cached_recovery_wait_keeps_only_the_current_accepted_intent() {
    let admitted = QualityIntentIdentity {
        generation: uuid::Uuid::new_v4().to_string(),
        control_epoch: 2,
        client_instance_id: uuid::Uuid::new_v4().to_string(),
        lifetime_id: "same-viewer".into(),
        recipe_revision: 3,
        accepted_sequence: 19,
    };
    let mut current = admitted.clone();
    current.accepted_sequence = 28;
    assert!(
        same_accepted_quality_intent(&admitted, &current),
        "a later ordinary Waiting snapshot must not cancel its own recovery"
    );
    current.recipe_revision += 1;
    assert!(!same_accepted_quality_intent(&admitted, &current));
    current = admitted.clone();
    current.accepted_sequence -= 1;
    assert!(!same_accepted_quality_intent(&admitted, &current));
    current = admitted.clone();
    current.generation = uuid::Uuid::new_v4().to_string();
    assert!(!same_accepted_quality_intent(&admitted, &current));
    current = admitted.clone();
    current.client_instance_id = uuid::Uuid::new_v4().to_string();
    assert!(!same_accepted_quality_intent(&admitted, &current));
    current = admitted.clone();
    current.lifetime_id = "another-viewer".into();
    assert!(!same_accepted_quality_intent(&admitted, &current));
}

#[test]
fn cancellation_before_offer_is_exact_and_idempotent() {
    let playback = uuid::Uuid::new_v4().to_string();
    let identity = identity(3);
    let pending = PendingCandidateGuard::begin_owned(&playback, "720p", Some(identity.clone()));
    let mut wrong = identity.clone();
    wrong.control_epoch += 1;
    assert!(!cancel_quality_preparation(&playback, &wrong));
    assert!(!pending.cancelled());
    assert!(cancel_quality_preparation(&playback, &identity));
    assert!(cancel_quality_preparation(&playback, &identity));
    assert!(pending.cancelled());
    assert!(pending_candidate_for_playback(&playback).is_none());
}

#[test]
fn late_cancel_does_not_cancel_a_same_selection_retry() {
    let playback = uuid::Uuid::new_v4().to_string();
    let earlier = identity(3);
    let mut later = earlier.clone();
    later.accepted_sequence = 5;
    let old = PendingCandidateGuard::begin_owned(&playback, "720p", Some(earlier.clone()));
    let current = PendingCandidateGuard::begin_owned(&playback, "720p", Some(later.clone()));
    assert!(old.cancelled());
    assert!(!cancel_quality_preparation(&playback, &earlier));
    drop(old);
    assert!(!current.cancelled());
    assert!(cancel_quality_preparation(&playback, &later));
    assert!(current.cancelled());
}
