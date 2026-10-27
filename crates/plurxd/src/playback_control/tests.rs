use super::*;

#[test]
fn a05_prepared_observation_survives_same_intent_poll_but_not_away_back_or_epoch_reset() {
    let mut state = ControlState::default();
    let client = uuid::Uuid::new_v4().to_string();
    let selected = selection_at(QualitySelection::Auto {
        height: Some(720),
        candidate_id: None,
    });
    let away = selection_at(QualitySelection::Auto {
        height: Some(1080),
        candidate_id: None,
    });
    let started = Instant::now();
    let accept = |state: &mut ControlState,
                  sequence,
                  epoch,
                  selection: &ClientSelection,
                  fingerprint: &str| {
        state
            .accept_at(
                started + Duration::from_secs(sequence),
                "incarnation",
                epoch,
                &client,
                sequence,
                ControlAcceptance::new(Some(ClientPlatform::Web), None)
                    .asking(selection)
                    .fingerprinted(fingerprint),
            )
            .expect("accepted")
    };
    assert_eq!(
        accept(&mut state, 1, 1, &selected, "original").0,
        ControlDisposition::Accepted
    );
    let identity = AcceptedControlIdentity {
        generation: "incarnation".into(),
        owner_epoch: 1,
        client_instance_id: client.clone(),
        sequence: 1,
        fingerprint: "original".into(),
        desired_digest: selected.desired().digest(),
    };
    let proof = state
        .accepted_observation(identity.clone())
        .expect("exact origin");
    accept(&mut state, 2, 1, &selected, "poll-position");
    assert!(
        state.observation_is_current(&proof),
        "new Poll fingerprint is not a new attachment"
    );
    assert!(
        state.accepted_observation(identity.clone()).is_none(),
        "old origin cannot be newly minted"
    );
    accept(&mut state, 3, 1, &away, "away");
    accept(&mut state, 4, 1, &selected, "back");
    assert!(
        !state.observation_is_current(&proof),
        "same digest cannot resurrect an old lifetime"
    );
    let latest = state
        .accepted_observation(AcceptedControlIdentity {
            sequence: 4,
            fingerprint: "back".into(),
            ..identity
        })
        .expect("new lifetime");
    accept(&mut state, 1, 2, &selected, "new-owner");
    assert!(!state.observation_is_current(&latest));
    assert!(!ControlState::default().observation_is_current(&latest));
    state.observational_desired_lifetime = Some(u64::MAX);
    state.advance_observational_lifetime();
    state.advance_observational_lifetime();
    assert_eq!(
        state.observational_desired_lifetime, None,
        "overflow is permanently Unknown"
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_commit_retry_starts_before_but_not_at_or_after_exact_expiry() {
    fn receipt(starts: Arc<AtomicUsize>) -> TerminalCommitReceipt {
        TerminalCommitReceipt::deferred_retryable_until(
            crate::media_sessions::unix_ms().saturating_add(60_000),
            move |attempt| {
                starts.fetch_add(1, Ordering::AcqRel);
                attempt.complete(Err(()));
            },
        )
    }

    fn deadline(receipt: &TerminalCommitReceipt) -> tokio::time::Instant {
        receipt
            .expiry
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .expect("exact terminal deadline")
    }

    let before_starts = Arc::new(AtomicUsize::new(0));
    let before = receipt(Arc::clone(&before_starts));
    tokio::time::advance(
        deadline(&before)
            .duration_since(tokio::time::Instant::now())
            .saturating_sub(Duration::from_millis(1)),
    )
    .await;
    before.retry();
    assert_eq!(before_starts.load(Ordering::Acquire), 1);
    assert!(before.wait().await.is_err());

    let at_starts = Arc::new(AtomicUsize::new(0));
    let at = receipt(Arc::clone(&at_starts));
    tokio::time::advance(deadline(&at).duration_since(tokio::time::Instant::now())).await;
    at.retry();
    assert_eq!(at_starts.load(Ordering::Acquire), 0);
    assert!(at.wait().await.is_err());

    let after_starts = Arc::new(AtomicUsize::new(0));
    let after = receipt(Arc::clone(&after_starts));
    tokio::time::advance(
        deadline(&after).duration_since(tokio::time::Instant::now()) + Duration::from_millis(1),
    )
    .await;
    after.retry();
    assert_eq!(after_starts.load(Ordering::Acquire), 0);
    assert!(after.wait().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn terminal_commit_success_completion_and_waiter_cannot_cross_exact_expiry() {
    fn response() -> ControlResponseV1 {
        ControlResponseV1 {
            protocol: PROTOCOL_V1.to_owned(),
            generation: "test-terminal-generation".to_owned(),
            control_epoch: 1,
            accepted_sequence: 1,
            server_time_unix_ms: 1,
            lease: PlaybackLeaseView {
                state: "ended".to_owned(),
                renew_after_ms: NEXT_EXCHANGE_MS,
                expires_at_unix_ms: 1,
            },
            delivery: DeliveryView {
                presentation: "test".to_owned(),
                producer_state: "complete".to_owned(),
                produced_through_ms: None,
                fetched_through_ms: 0,
                delivered_bps: None,
                delivered_idle_ms: None,
                recent_producer_speed: None,
                client_runway_ms: 0,
                admitted: None,
                producer_decision: None,
                hold_reason: None,
                subtitle_readiness: None,
                subtitle_revision: None,
                preparation: None,
                owner_node_hash: "n-test".to_owned(),
                owner_epoch: 1,
            },
            effective_selection: EffectiveSelection {
                candidate_id: None,
                quality_auto: true,
                height: 720,
                audio_track: None,
                subtitle_burn: None,
                audio_offset_ms: 0,
                codec: "test".to_owned(),
                dynamic_range: Some("sdr".to_owned()),
            },
            action: ControlAction::None,
        }
    }

    let pending_attempt = Arc::new(std::sync::Mutex::new(None::<TerminalCommitAttempt>));
    let completing = TerminalCommitReceipt::deferred_retryable_until(
        crate::media_sessions::unix_ms().saturating_add(60_000),
        {
            let pending_attempt = Arc::clone(&pending_attempt);
            move |attempt| {
                *pending_attempt
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(attempt);
            }
        },
    );
    let completion_deadline = completing.deadline_for_test();
    tokio::time::advance(
        completion_deadline
            .duration_since(tokio::time::Instant::now())
            .saturating_sub(Duration::from_millis(1)),
    )
    .await;
    completing.retry();
    assert!(pending_attempt
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some());
    tokio::time::advance(Duration::from_millis(1)).await;
    pending_attempt
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .expect("attempt started before expiry")
        .complete(Ok(response()));
    assert!(
        completing.wait().await.is_err(),
        "an attempt completing at expiry cannot publish success"
    );

    let stored = TerminalCommitReceipt::retryable_until(
        crate::media_sessions::unix_ms().saturating_add(60_000),
        |attempt| attempt.complete(Ok(response())),
    );
    assert!(
        stored.wait().await.is_ok(),
        "success is visible before expiry"
    );
    tokio::time::advance(
        stored
            .deadline_for_test()
            .duration_since(tokio::time::Instant::now()),
    )
    .await;
    assert!(
        stored.wait().await.is_err(),
        "a delayed waiter cannot consume stored success at expiry"
    );
}

struct DropReplyOnTerminalAdmission {
    receiver: std::sync::Mutex<
        Option<tokio::sync::oneshot::Receiver<Result<RollingControlOutcome, ControlStateError>>>,
    >,
    accepted: AtomicUsize,
}

impl RollingTerminalAdmission for DropReplyOnTerminalAdmission {
    fn accepted(&self, _outcome: RollingControlOutcome) {
        self.accepted.fetch_add(1, Ordering::AcqRel);
        drop(
            self.receiver
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
    }
}

struct DropReplyOnPreparationAdmission {
    receiver: std::sync::Mutex<
        Option<tokio::sync::oneshot::Receiver<Result<RollingControlOutcome, ControlStateError>>>,
    >,
    accepted: AtomicUsize,
}

impl PreparationSettlementAdmission for DropReplyOnPreparationAdmission {
    fn accepted(&self, _outcome: PreparationControlOutcome) {
        self.accepted.fetch_add(1, Ordering::AcqRel);
        drop(
            self.receiver
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
    }
}

/// A `switched` acknowledgement is only worth anything with the frame
/// timestamp on it, and it may not ride an `end`.
///
/// The two halves matter for different reasons. `committed` is the
/// server's transaction and carries `committed_media_origin_ms` as its
/// evidence, with the frame time alongside as a measurement; `switched` is
/// the client's display and the frame time is the whole of it, so one that
/// arrives without it reports nothing at all. And a packet that both ends
/// the stream and reports a successor reaching the screen is asking for
/// one teardown down two paths.
#[test]
fn a_switched_acknowledgement_needs_the_frame_it_claims() {
    let ack = |state: AcknowledgementState, first_frame: Option<i64>| ActionAcknowledgement {
        action_id: uuid::Uuid::new_v4().to_string(),
        state,
        buffered_through_ms: Some(25_000),
        committed_media_origin_ms: Some(0),
        first_frame_unix_ms: first_frame,
    };
    let with = |acknowledgement, demand| {
        let mut request = request();
        request.acknowledgement = Some(acknowledgement);
        request.demand = demand;
        if demand != PlaybackDemand::Active {
            request.playback_rate = 0.0;
        }
        request.validate(None, 2_000)
    };

    assert_eq!(
        with(
            ack(AcknowledgementState::Switched, None),
            PlaybackDemand::Active
        ),
        Err("acknowledgement.first_frame_unix_ms"),
        "switched without a frame timestamp says only that a different word was sent"
    );
    assert!(with(
        ack(AcknowledgementState::Switched, Some(1_700_000_000_000)),
        PlaybackDemand::Active
    )
    .is_ok());
    assert_eq!(
        with(
            ack(AcknowledgementState::Switched, Some(1_700_000_000_000)),
            PlaybackDemand::End
        ),
        Err("acknowledgement.state"),
        "a request cannot both end the stream and report a successor on screen"
    );
}

/// A `switched` naming a successor that was never committed tears the
/// preparation down; one that follows a real commit leaves it alone.
///
/// The second half is the ordinary case and the reason `switched` returns
/// no directive at all: by the time the client's frame is on screen the
/// slot has already been committed and cleared, and issuing an abort there
/// would tear down the stream the viewer is now watching.
///
/// The first half is what makes the first half safe. A slot still staged
/// is a successor this owner never published, so the claim is false — but
/// the expensive part is not the lie, it is that nothing else will ever
/// direct the slot. `record_preparation_progress` has recorded a terminal
/// state, so a later `committed` is out of order, and the reservation
/// would sit on one of two encoders until its own deadline lapsed.
#[test]
fn a_switch_onto_media_that_never_published_aborts_the_preparation() {
    let action_id = uuid::Uuid::new_v4().to_string();
    let staged = uuid::Uuid::new_v4().to_string();
    let selection = EffectiveSelection {
        candidate_id: None,
        quality_auto: true,
        height: 1080,
        audio_track: None,
        subtitle_burn: None,
        audio_offset_ms: 0,
        codec: "h264".to_owned(),
        dynamic_range: None,
    };
    // Announced after staging, as acceptance does it: `stage_preparation`
    // clears any prior binding, so a binding written first would not
    // survive to be acknowledged.
    let binding = || PreparedActionBinding {
        successor: PreparedSuccessorAction {
            enhancement: None,
            control: None,
            staged_incarnation_id: staged.clone(),
            deadline_ms: i64::MAX,
            session_id: "session".to_owned(),
            playlist_url: "/hls/session/index.m3u8".to_owned(),
            media_origin_ms: 0,
            effective_selection: selection.clone(),
        },
        action: ControlAction::Prepare {
            enhancement: None,
            control: None,
            action_id: action_id.clone(),
            session_id: "session".to_owned(),
            playlist_url: "/hls/session/index.m3u8".to_owned(),
            media_origin_ms: 0,
            effective_selection: selection.clone(),
        },
        acknowledgement: None,
    };
    let switched = ActionAcknowledgement {
        action_id: action_id.clone(),
        state: AcknowledgementState::Switched,
        buffered_through_ms: None,
        committed_media_origin_ms: None,
        first_frame_unix_ms: Some(1_700_000_000_000),
    };

    let mut still_staged = ControlState::default();
    assert!(still_staged.stage_preparation(
        staged.clone(),
        uuid::Uuid::new_v4().to_string(),
        i64::MAX,
        None,
    ));
    still_staged.prepared_action = Some(binding());
    assert_eq!(
        still_staged
            .record_terminal_preparation_acknowledgement(Some(&switched), 1_700_000_000_000),
        Some(PreparationDirective::Abort {
            staged_incarnation_id: staged.clone(),
            acknowledgement_rejected: true,
        }),
        "a successor on screen that this owner never published is refused, \
             and the slot it would otherwise hold is released",
    );
    assert!(
        matches!(
            &still_staged.preparation,
            PreparationSlot::Aborting { staged_incarnation_id } if *staged_incarnation_id == staged
        ),
        "the refusal has to move the slot, not merely report a directive",
    );

    // The ordinary case: the commit already cleared the slot.
    let mut after_commit = ControlState {
        prepared_action: Some(binding()),
        ..ControlState::default()
    };
    assert_eq!(
        after_commit
            .record_terminal_preparation_acknowledgement(Some(&switched), 1_700_000_000_000),
        None,
        "a switch that follows a real commit has no preparation left to \
             direct, and must not tear down what the viewer is watching",
    );
    assert!(matches!(after_commit.preparation, PreparationSlot::Empty));
}

#[test]
fn quality_snapshots_refuse_regression_and_equal_revision_conflicts_before_acceptance() {
    let request = request();
    let mut caps = request.capabilities.clone().expect("fixture capabilities");
    caps.decoder_caps = Some(DecoderCapsSnapshot {
        revision: 5,
        video: Vec::new(),
    });
    caps.presentation_target = Some(plurx_core::playback::candidate::PresentationTarget {
        width_px: 2400,
        height_px: 1600,
        revision: 5,
    });
    let now = Instant::now();
    let mut state = ControlState::default();
    let acceptance = |caps: &DynamicCapabilities| {
        ControlAcceptance::observed(
            Some(caps.platform),
            &PreparedSuccessorObservation::NotRequested,
            None,
            None,
            &request.selection,
            Some(caps),
        )
    };
    state
        .accept_at(
            now,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            acceptance(&caps),
        )
        .expect("first revision accepted");
    let mut stale = caps.clone();
    stale.decoder_caps.as_mut().expect("snapshot").revision = 4;
    assert_eq!(
        state.accept_at(
            now + Duration::from_secs(2),
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            acceptance(&stale)
        ),
        Err(ControlStateError::StaleSequence)
    );
    assert_eq!(state.last_sequence, 1);
    assert_eq!(state.last_capabilities.as_ref(), Some(&caps));
    let mut conflict = caps.clone();
    conflict
        .presentation_target
        .as_mut()
        .expect("target")
        .width_px = 1920;
    assert_eq!(
        state.accept_at(
            now + Duration::from_secs(2),
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            acceptance(&conflict)
        ),
        Err(ControlStateError::StaleSequence)
    );
    let mut omitted = caps.clone();
    omitted.decoder_caps = None;
    omitted.presentation_target = None;
    state
        .accept_at(
            now + Duration::from_secs(2),
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            acceptance(&omitted),
        )
        .expect("omission retains proof");
    assert_eq!(
        state
            .last_capabilities
            .as_ref()
            .and_then(|row| row.decoder_caps.as_ref()),
        caps.decoder_caps.as_ref()
    );
}

#[test]
fn newer_quality_snapshot_revokes_a_previously_reserved_preparation_commit() {
    let request = request();
    let mut caps = request.capabilities.clone().expect("fixture capabilities");
    caps.decoder_caps = Some(DecoderCapsSnapshot {
        revision: 1,
        video: Vec::new(),
    });
    let now = Instant::now();
    let mut state = ControlState::default();
    let acceptance = |caps: &DynamicCapabilities| {
        ControlAcceptance::observed(
            Some(caps.platform),
            &PreparedSuccessorObservation::NotRequested,
            None,
            None,
            &request.selection,
            Some(caps),
        )
    };
    state
        .accept_at(
            now,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            acceptance(&caps),
        )
        .expect("initial snapshot");
    assert!(state.stage_preparation("quality-successor".into(), "incumbent".into(), 10_000, None));
    assert!(state.reserve_preparation_commit("quality-successor", 1_000));
    assert!(state.may_commit_preparation_for_owner("quality-successor", 1));
    caps.decoder_caps.as_mut().expect("snapshot").revision = 2;
    state
        .accept_at(
            now + Duration::from_secs(2),
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            acceptance(&caps),
        )
        .expect("new revision accepted");
    assert!(!state.may_commit_preparation_for_owner("quality-successor", 1));
}

#[test]
fn decoder_snapshot_parser_preserves_loss_and_refuses_unbounded_claims() {
    let empty = serde_json::json!({"revision": 1, "video": []});
    let parsed: DecoderCapsSnapshot =
        serde_json::from_value(empty.clone()).expect("valid decoder snapshot fixture");
    assert_eq!(
        serde_json::to_value(parsed).expect("valid decoder snapshot fixture"),
        empty
    );
    for revision in [0, 9_007_199_254_740_992u64] {
        assert!(serde_json::from_value::<DecoderCapsSnapshot>(
            serde_json::json!({"revision": revision, "video": []})
        )
        .is_err());
    }
    let valid = serde_json::json!({"revision": 2, "video": [{
        "codec": "vp9", "profiles": ["main10"], "available": false,
        "dynamic_ranges": [], "dv_profiles": []
    }]});
    let parsed: DecoderCapsSnapshot =
        serde_json::from_value(valid.clone()).expect("valid decoder snapshot fixture");
    assert_eq!(
        serde_json::to_value(parsed).expect("valid decoder snapshot fixture"),
        valid
    );
    for (field, value) in [
        ("codec", serde_json::json!("auto")),
        ("profiles", serde_json::json!(["Main10"])),
        ("max_width", serde_json::json!(0)),
        (
            "max_frame_rate",
            serde_json::json!({"numerator": 30, "denominator": 0}),
        ),
        ("unrecognized", serde_json::json!(true)),
    ] {
        let mut bad = valid.clone();
        bad["video"][0][field] = value;
        assert!(
            serde_json::from_value::<DecoderCapsSnapshot>(bad).is_err(),
            "{field}"
        );
    }
    let mut bad = valid.clone();
    bad["video"] = serde_json::json!(vec![valid["video"][0].clone(); 65]);
    assert!(serde_json::from_value::<DecoderCapsSnapshot>(bad).is_err());
}

#[test]
fn auto_candidate_parser_floor_retains_nested_route_for_owner_dispatch() {
    let mut wire = serde_json::to_value(relay_request()).expect("valid parser-floor test fixture");
    let id = "0123456789abcdef0123456789abcdef";
    wire["control"]["selection"]["quality"]["candidate_id"] = id.into();
    wire["control"]["capabilities"]["presentation_target"] = serde_json::json!({
        "width_px": 2400, "height_px": 1600, "revision": 3
    });
    wire["control"]["capabilities"]["decoder_caps"] = serde_json::json!({
        "revision": 4,
        "video": [{"codec": "h264", "profiles": ["high"], "available": false,
            "dynamic_ranges": ["sdr"], "dv_profiles": [], "max_width": 1920,
            "max_height": 1080, "max_frame_rate": {"numerator": 30000, "denominator": 1001}}]
    });
    let parsed: ControlRelayRequest =
        serde_json::from_value(wire.clone()).expect("valid parser-floor test fixture");
    wire["control"]["intent"] = serde_json::to_value(plurx_core::playback::MediaIntentEnvelope {
        lifetime_id: "candidate-parser-floor".to_owned(),
        recipe_revision: 1,
        destination_revision: 1,
        transport_revision: 1,
        selection: parsed.control.selection.desired(),
    })
    .expect("valid parser-floor test fixture");
    let parsed: ControlRelayRequest =
        serde_json::from_value(wire.clone()).expect("valid parser-floor test fixture");
    assert_eq!(
        serde_json::to_value(&parsed).expect("valid parser-floor test fixture"),
        wire
    );
    assert!(parsed
        .control
        .intent
        .as_ref()
        .expect("valid parser-floor test fixture")
        .validate()
        .is_ok());
    assert!(parsed.control.selection.validate().is_ok());
    assert!(
        parsed.is_valid(),
        "a tolerant ingress must preserve and forward the bounded ask"
    );
    assert_eq!(
        parsed.control.selection.desired().quality,
        plurx_core::playback::DesiredQuality::Auto {
            height: None,
            candidate_id: Some(
                plurx_core::playback::candidate::CandidateId::from_hex(id)
                    .expect("valid parser-floor test fixture")
            ),
        }
    );
    for path in ["control", "selection", "quality"] {
        let mut bad = wire.clone();
        match path {
            "control" => bad["control"]["unexpected"] = true.into(),
            "selection" => bad["control"]["selection"]["unexpected"] = true.into(),
            _ => bad["control"]["selection"]["quality"]["unexpected"] = true.into(),
        }
        assert!(serde_json::from_value::<ControlRelayRequest>(bad).is_err());
    }
    let legacy = serde_json::to_value(relay_request()).expect("valid parser-floor test fixture");
    assert!(legacy["control"]["selection"]["quality"]
        .get("candidate_id")
        .is_none());
    assert!(legacy["control"]["capabilities"]
        .get("presentation_target")
        .is_none());
    assert!(legacy["control"]["capabilities"]
        .get("decoder_caps")
        .is_none());
}

#[test]
fn auto_candidate_parser_floor_refuses_bad_ids_and_non_auto_extension() {
    for quality in [
        serde_json::json!({"mode":"auto", "candidate_id":"NOT-A-CANDIDATE"}),
        serde_json::json!({"mode":"manual", "height":1080,
            "candidate_id":"0123456789abcdef0123456789abcdef"}),
        serde_json::json!({"mode":"original",
            "candidate_id":"0123456789abcdef0123456789abcdef"}),
        serde_json::json!({"mode":"future"}),
    ] {
        let mut wire = serde_json::to_value(request()).expect("valid parser-floor test fixture");
        wire["selection"]["quality"] = quality;
        assert!(serde_json::from_value::<ControlRequestV1>(wire).is_err());
    }
}

fn request() -> ControlRequestV1 {
    ControlRequestV1 {
        intent: None,
        protocol: PROTOCOL_V1.to_owned(),
        generation: uuid::Uuid::new_v4().to_string(),
        control_epoch: 1,
        client_instance_id: uuid::Uuid::new_v4().to_string(),
        sequence: 1,
        demand: PlaybackDemand::Active,
        position_ms: 10_000,
        buffered_from_ms: Some(9_000),
        buffered_through_ms: 25_000,
        playback_rate: 1.0,
        render_state: RenderState::Rendering,
        seek_target_ms: None,
        observed_download_bps: Some(8_000_000),
        selection: ClientSelection {
            quality: QualitySelection::Auto {
                height: None,
                candidate_id: None,
            },
            audio_track: Some(0),
            subtitle: SubtitleSelection {
                mode: SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: CodecPolicy::Auto,
            dynamic_range: DynamicRangePolicy::Auto,
        },
        capabilities: Some(DynamicCapabilities {
            presentation_target: None,
            decoder_caps: None,
            platform: ClientPlatform::Web,
            max_height: 2160,
            codecs: vec![CodecPolicy::H264, CodecPolicy::Hevc],
            dynamic_ranges: vec![DynamicRangePolicy::Sdr, DynamicRangePolicy::Hdr10],
            dual_player_preparation: false,
        }),
        observation: None,
        acknowledgement: None,
        supported_actions: None,
    }
}

fn delivery_with_hold(reason: Option<&str>) -> DeliveryView {
    DeliveryView {
        presentation: "live-recovery".to_owned(),
        producer_state: "held".to_owned(),
        produced_through_ms: Some(30_000),
        fetched_through_ms: 25_000,
        delivered_bps: Some(4_000_000),
        delivered_idle_ms: Some(0),
        recent_producer_speed: Some(1.4),
        client_runway_ms: 15_000,
        admitted: None,
        producer_decision: None,
        hold_reason: reason.map(str::to_owned),
        subtitle_readiness: None,
        subtitle_revision: None,
        preparation: None,
        owner_node_hash: "n-0123456789abcdef".to_owned(),
        owner_epoch: 1,
    }
}

#[test]
fn a_suppressed_hold_is_counted_so_the_rollout_can_be_watched() {
    // The number that says how much a client upgrade would buy. Production
    // is held, the server knows why, and the client cannot be told because
    // it never declared the action.
    let passive = request();
    let held = delivery_with_hold(Some("demand"));
    let action = resolve_action(&ControlAction::None, &held, &passive);
    assert_eq!(action, ControlAction::None);
    assert_eq!(
        action_metrics(&action, &held, &passive, false),
        ActionMetrics {
            action: ActionKind::None,
            hold_reason: None,
            suppressed: true,
            recovery_withheld: false,
        },
    );

    // Nothing was held, so nothing was withheld.
    let flowing = delivery_with_hold(None);
    assert!(
        !action_metrics(
            &resolve_action(&ControlAction::None, &flowing, &passive),
            &flowing,
            &passive,
            false,
        )
        .suppressed,
    );

    // A reason this server cannot name was never a candidate instruction.
    // Counting it would inflate the gap with responses that no client
    // rollout would change.
    let unknown = delivery_with_hold(Some("a_reason_from_next_year"));
    assert!(
        !action_metrics(
            &resolve_action(&ControlAction::None, &unknown, &passive),
            &unknown,
            &passive,
            false,
        )
        .suppressed,
    );
}

#[test]
fn a_sent_hold_is_counted_by_reason_and_never_as_suppressed() {
    let mut accepting = request();
    accepting.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
    let held = delivery_with_hold(Some("working_set"));
    let action = resolve_action(&ControlAction::None, &held, &accepting);
    assert_eq!(
        action_metrics(&action, &held, &accepting, false),
        ActionMetrics {
            action: ActionKind::Hold,
            hold_reason: Some(HoldReason::WorkingSet),
            suppressed: false,
            recovery_withheld: false,
        },
    );
}

/// A client the serving predicate is about to speak for: stalled, its
/// decoder starved, no runway left, and 35 s of published media it has not
/// fetched. `delivery_with_hold` is the healthy counterpart — 15 s of
/// runway and a 5 s gap — and every test below plays the two against each
/// other.
fn stalled_starved() -> (DeliveryView, ControlRequestV1) {
    let mut delivery = delivery_with_hold(Some("time"));
    delivery.client_runway_ms = 0;
    delivery.produced_through_ms = Some(60_000);
    delivery.fetched_through_ms = 25_000;
    let mut request = accepts_everything();
    request.render_state = RenderState::Stalled;
    request.observation = Some(ClientObservation {
        dropped_frames: None,
        decoder_state: Some(DecoderState::Starved),
        error_code: None,
        error_detail: None,
    });
    (delivery, request)
}

/// A VOD status carrying the two frontiers a far seek pulls apart: the
/// title's own published run, and the run measured from the segment this
/// client was last served.
fn vod_status(
    published_end_ms: Option<i64>,
    ready_ahead_end_ms: Option<i64>,
    fetched_end_ms: i64,
) -> HlsSessionInfo {
    vod_status_delivering(published_end_ms, ready_ahead_end_ms, fetched_end_ms, None)
}

/// The same VOD status, carrying a recorded producer failure.
fn vod_status_failed(decision: ProducerDecisionReason) -> HlsSessionInfo {
    let mut status = vod_status(Some(120_000), Some(120_000), 60_000);
    let HlsSessionInfo::Vod(info) = &mut status else {
        panic!("fixture was not VOD");
    };
    info.producer_state = "failed";
    info.producer_hold = None;
    info.producer_failed = Some("the producer stopped".to_owned());
    info.producer_decision = Some(decision.status());
    status
}

/// A reason this node has never heard of relays instead of collapsing the
/// exchange.
///
/// The hazard is a rolling deploy: the owner runs the new build and the
/// ingress relaying for it runs the old one, so the ingress validates
/// against its own compiled vocabulary. Rejecting the response turns a
/// perfectly good exchange into a 503 `control_unavailable` carrying a
/// 500 ms retry, and that session then polls ten times faster than its own
/// cadence for the whole deploy window while learning nothing. Widening
/// the vocabulary is only safe because of this.
#[test]
fn a_relay_passes_on_a_decision_it_does_not_know() {
    let (mut response, request) = relay_pair();
    response.delivery.producer_decision = Some("a_reason_from_a_newer_build".to_owned());
    assert!(
        response.is_valid_for(&request),
        "an unknown but well-formed reason is the client's to ignore, not \
             the relay's to fail the exchange over"
    );

    for malformed in ["", "Has Caps", "has spaces", &"x".repeat(49)] {
        response.delivery.producer_decision = Some(malformed.to_owned());
        assert!(
            !response.is_valid_for(&request),
            "shape is still bounded: {malformed:?} is not a wire name"
        );
    }
}

/// P1-2. A VOD failure reaches the client as an action, not as silence.
///
/// `vodserve` has always had a failure; what it did not have was a class,
/// so `DeliveryView::from_status` set `producer_decision: None` and
/// `resolve_action` had nothing to rank. Segment GETs returned a typed 5xx
/// while the control plane went on answering `action: none`, and every
/// client fell back on its own generic watchdog and reopen budget.
///
/// This drives the whole path the review asks for — recorded class, into
/// the delivery view, through action selection, out as serialized JSON —
/// because the value of the class is entirely in the action it produces.
#[test]
fn a_classified_vod_failure_becomes_a_serialized_client_action() {
    let (_, mut request) = stalled_starved();
    request.buffered_through_ms = request.position_ms;
    request.supported_actions = Some(vec![
        HOLD_ACTION.to_owned(),
        RETRY_RESOURCE_ACTION.to_owned(),
        TERMINAL_ACTION.to_owned(),
    ]);

    for decision in ProducerDecisionReason::ALL {
        let delivery =
            DeliveryView::from_status(&vod_status_failed(decision), &request, "node", 1, 0, None);
        assert_eq!(
            delivery.producer_decision.as_deref(),
            Some(decision.status()),
            "the class the rendition recorded is the class the view publishes"
        );

        let action = resolve_action(&ControlAction::None, &delivery, &request);
        let wire = serde_json::to_value(&action).expect("the action serializes");
        if decision.is_permanent() {
            assert_eq!(wire["type"], "terminal");
            assert_eq!(wire["code"], decision.status());
        } else {
            assert_eq!(wire["type"], "retry_resource");
            assert_eq!(wire["reason"], decision.status());
            assert!(
                wire["after_ms"].as_u64().is_some_and(|after| after > 0),
                "a retryable verdict must say when"
            );
        }
    }
}

/// The silence this replaces: an unclassified VOD failure still produces
/// no action at all, so the test above cannot be passing on some other
/// signal in the fixture.
#[test]
fn an_unclassified_vod_failure_still_says_nothing() {
    let (_, mut request) = stalled_starved();
    request.buffered_through_ms = request.position_ms;
    request.supported_actions = Some(vec![
        RETRY_RESOURCE_ACTION.to_owned(),
        TERMINAL_ACTION.to_owned(),
    ]);

    let mut status = vod_status_failed(ProducerDecisionReason::ReaderFailed);
    let HlsSessionInfo::Vod(info) = &mut status else {
        panic!("fixture was not VOD");
    };
    info.producer_decision = None;

    let delivery = DeliveryView::from_status(&status, &request, "node", 1, 0, None);
    assert_eq!(delivery.producer_decision, None);
    assert_eq!(
        resolve_action(&ControlAction::None, &delivery, &request),
        ControlAction::None,
        "a failure nobody classified is exactly what P1-2 was"
    );
}

/// The same VOD status, with a measured delivery rate.
///
/// `None` is the honest default and the state every VOD session was stuck
/// in before P1-3: no window has closed, so there is no rate.
fn vod_status_delivering(
    published_end_ms: Option<i64>,
    ready_ahead_end_ms: Option<i64>,
    fetched_end_ms: i64,
    delivered_bps: Option<i64>,
) -> HlsSessionInfo {
    HlsSessionInfo::Vod(Box::new(crate::vodserve::VodSessionInfo {
        stream_format: None,
        active_encode_milli_realtime: None,
        active_encode_age_ms: None,
        active_encode_active_ms: None,
        active_encode_segments: None,
        active_encode_candidate_id: None,
        // A plausible total, not a rate divided by eight: a fixture that
        // encodes the wrong unit is how the wrong unit gets copied.
        delivered_bytes: delivered_bps.map_or(0, |_| 4_194_304),
        delivered_bps,
        delivered_idle_ms: 0,
        http_wait_count: 0,
        http_wait_oldest_ms: None,
        http_wait_segment: None,
        status_generated_unix_ms: crate::media_sessions::unix_ms(),
        id: "vod-session".to_owned(),
        file_id: 7,
        target_height: 1080,
        encoder: "vod",
        tone_map_peak_nits: None,
        tone_map_peak_source: None,
        playlist_shape: "vod",
        producer_state: "held",
        producer_hold: Some("working_set"),
        producer_failed: None,
        producer_decision: None,
        control_demand: Some("active"),
        reported_position_ms: Some(fetched_end_ms),
        client_runway_ms: Some(0),
        render_state: Some("waiting"),
        server_ready_state: "ready",
        server_ready_anchor_ms: Some(fetched_end_ms),
        server_ready_end_ms: ready_ahead_end_ms,
        server_ready_seconds: ready_ahead_end_ms
            .map(|end| end.saturating_sub(fetched_end_ms) as f64 / 1_000.0),
        server_next_ready_start_ms: Some(fetched_end_ms),
        server_next_ready_end_ms: ready_ahead_end_ms,
        published_end_ms,
        ready_ahead_end_ms,
        fetched_end_ms,
        fetched_segment: Some(20),
        ahead_seconds: ready_ahead_end_ms.map(|end| (end - fetched_end_ms).max(0) / 1_000),
        materialized_segments: 8,
        planned_segments: 60,
        materialized_bytes: 8_192,
        planned_bytes: 61_440,
        working_set_bytes: 8_192,
        working_set_budget_bytes: 1 << 30,
        completed_cache_bytes: 0,
        admitted: true,
        suspended: true,
        final_: false,
    }))
}

/// P1-3. A measured VOD rate reaches the control view, and an unmeasured
/// one stays unknown.
///
/// Both halves matter. The first is what makes preparation reachable at
/// all on the primary presentation; the second is the constraint the
/// review states in the same breath — unknown must stay unknown, never
/// filled in from the encoder's configured target. On VOD the presentation
/// is a stream copy, so the encoder target describes nothing that was ever
/// delivered, and a substituted number would let the headroom decision
/// pass a client whose link nobody measured.
#[test]
fn a_measured_vod_delivery_reaches_the_control_view_and_an_unmeasured_one_stays_unknown() {
    let (_, mut request) = stalled_starved();
    request.buffered_through_ms = request.position_ms;

    let measured = DeliveryView::from_status(
        &vod_status_delivering(Some(120_000), Some(120_000), 60_000, Some(12_000_000)),
        &request,
        "node",
        1,
        0,
        None,
    );
    assert_eq!(measured.delivered_bps, Some(12_000_000));
    assert_eq!(
        measured.delivered_idle_ms,
        Some(0),
        "the idle age is what tells a stale rate from a dead link, so it \
             travels with the rate rather than being dropped"
    );

    let unmeasured = DeliveryView::from_status(
        &vod_status(Some(120_000), Some(120_000), 60_000),
        &request,
        "node",
        1,
        0,
        None,
    );
    assert_eq!(
        unmeasured.delivered_bps, None,
        "a session that has closed no window reports no rate at all"
    );
}

#[test]
fn a_vod_seek_past_a_hole_is_judged_by_the_media_ahead_of_the_client() {
    // The trap this closes: after a far seek the title's published run
    // ends behind the playhead, so `published − fetched` is negative and
    // the predicate fails safe — leaving a client wedged in front of
    // segments that are materialized and servable right now.
    let (_, mut request) = stalled_starved();
    // Unlike the resolver fixtures, this one goes through `from_status`,
    // which computes the runway itself: a stalled player's buffer ends at
    // its own position.
    request.buffered_through_ms = request.position_ms;
    let seeked = DeliveryView::from_status(
        &vod_status(Some(20_000), Some(240_000), 200_000),
        &request,
        "node-a",
        1,
        0,
        None,
    );
    assert_eq!(
        seeked.produced_through_ms,
        Some(240_000),
        "the frontier the control plane reads is the one this client can fetch from",
    );
    assert_eq!(
        resolve_action(&ControlAction::None, &seeked, &request),
        ControlAction::None,
        "so a post-seek wedge recovers rather than deadlocking",
    );

    // No hole, no difference: this is a correction to one reading, not a
    // second policy.
    let contiguous = DeliveryView::from_status(
        &vod_status(Some(240_000), Some(240_000), 200_000),
        &request,
        "node-a",
        1,
        0,
        None,
    );
    assert_eq!(contiguous.produced_through_ms, seeked.produced_through_ms);

    // A rendition that has published nothing ahead of this client proves
    // nothing, and an unknown frontier must never authorise a reopen.
    let unknown = DeliveryView::from_status(
        &vod_status(Some(20_000), None, 200_000),
        &request,
        "node-a",
        1,
        0,
        None,
    );
    assert_eq!(unknown.produced_through_ms, None);
    assert_eq!(
        resolve_action(&ControlAction::None, &unknown, &request),
        ControlAction::Hold {
            reason: HoldReason::WorkingSet,
            revisit_after_ms: HoldReason::WorkingSet.revisit_after_ms(),
        },
    );
}

#[test]
fn a_stalled_starved_client_with_fetchable_media_is_not_told_to_hold() {
    // Whatever the producer paused for, it did not pause the bytes this
    // client has already been served and has not fetched. Production state
    // is not authority over serving.
    let healthy = accepts_everything();
    for reason in [
        "demand",
        "time",
        "bytes",
        "global",
        "ahead",
        "working_set",
        "no_room",
    ] {
        let (mut wedged, request) = stalled_starved();
        wedged.hold_reason = Some(reason.to_owned());
        assert_eq!(
            resolve_action(&ControlAction::None, &wedged, &request),
            ControlAction::None,
            "{reason} must not veto a wedged client's recovery",
        );

        // The same hold, to a client that is playing, is still an
        // instruction. Withholding it there would cost a reopen the server
        // asked for.
        let held = delivery_with_hold(Some(reason));
        assert_eq!(
            resolve_action(&ControlAction::None, &held, &healthy),
            {
                let reason = HoldReason::from_delivery(reason).expect("named reason");
                ControlAction::Hold {
                    reason,
                    revisit_after_ms: reason.revisit_after_ms(),
                }
            },
            "{reason} must still be sent to a healthy client",
        );
    }
}

#[test]
fn the_serving_predicate_requires_an_active_stall_and_a_recoverable_boundary() {
    // Active stalled presentation evidence is shared by both recovery arms.
    // The final fact may be either published-but-unfetched supply or a
    // substantial contiguous loaded runway; neither depends on a producer
    // hold to make progress.
    type BreakOne = fn(&mut DeliveryView, &mut ControlRequestV1);
    let cases: [(&str, BreakOne); 3] = [
        ("not active", |_, request| {
            request.demand = PlaybackDemand::Hold;
        }),
        ("not stalled", |_, request| {
            request.render_state = RenderState::Rendering;
        }),
        ("no recoverable boundary", |delivery, _| {
            delivery.client_runway_ms = STARVED_RUNWAY_MS;
            delivery.fetched_through_ms =
                delivery.produced_through_ms.expect("produced") - (FETCHABLE_GAP_MS - 1);
        }),
    ];
    for (name, break_one) in cases {
        let (mut delivery, mut request) = stalled_starved();
        break_one(&mut delivery, &mut request);
        assert_eq!(
            resolve_action(&ControlAction::None, &delivery, &request),
            ControlAction::Hold {
                reason: HoldReason::Time,
                revisit_after_ms: HoldReason::Time.revisit_after_ms(),
            },
            "{name} alone must leave the hold in force",
        );
        assert!(!recovery_outranks_hold(&delivery, &request), "{name}");
    }
}

#[test]
fn loaded_stall_recovery_does_not_require_decoder_failure() {
    for decoder_state in [
        None,
        Some(DecoderState::Unknown),
        Some(DecoderState::Ready),
        Some(DecoderState::Starved),
        Some(DecoderState::Failed),
    ] {
        let (mut delivery, mut request) = stalled_starved();
        // The incident's loaded runway, with no fetch gap: presentation
        // recovery must not depend on a second, invented supply problem.
        delivery.client_runway_ms = 18_310;
        delivery.produced_through_ms = Some(214_673);
        delivery.fetched_through_ms = 214_673;
        request.observation = decoder_state.map(|state| ClientObservation {
            dropped_frames: Some(1),
            decoder_state: Some(state),
            error_code: None,
            error_detail: None,
        });
        assert!(
            recovery_outranks_hold(&delivery, &request),
            "{decoder_state:?}"
        );
        assert_eq!(
            resolve_action(&ControlAction::None, &delivery, &request),
            ControlAction::None,
            "{decoder_state:?}"
        );
        assert!(action_metrics(&ControlAction::None, &delivery, &request, false).recovery_withheld);
        // A fetch wedge is equally independent of decoder attribution.
        delivery.client_runway_ms = 0;
        delivery.fetched_through_ms = 180_000;
        assert!(
            recovery_outranks_hold(&delivery, &request),
            "{decoder_state:?}"
        );
        assert_eq!(
            resolve_action(&ControlAction::None, &delivery, &request),
            ControlAction::None,
            "{decoder_state:?}"
        );
        // The same observations do not override intentional pause.
        request.demand = PlaybackDemand::Hold;
        assert!(!recovery_outranks_hold(&delivery, &request));
    }
}

#[test]
fn lifecycle_refill_loaded_native_wait_outranks_a_producer_hold() {
    let (mut delivery, request) = stalled_starved();
    delivery.client_runway_ms = 22_000;
    delivery.fetched_through_ms = delivery.produced_through_ms.expect("produced");
    assert!(
        recovery_outranks_hold(&delivery, &request),
        "loaded native media is a presentation boundary even with no fetch gap"
    );
    assert_eq!(
        resolve_action(&ControlAction::None, &delivery, &request),
        ControlAction::None,
        "a production hold cannot defer the client's bounded native recovery"
    );

    delivery.produced_through_ms = None;
    assert!(
        recovery_outranks_hold(&delivery, &request),
        "known contiguous client media does not become unknown with publication"
    );
}

#[test]
fn a_producer_decision_still_outranks_the_predicate() {
    // The ranking is unchanged: a producer that has stopped is not
    // holding, and a wedged client must still be told when trying again
    // cannot help.
    let (mut delivery, request) = stalled_starved();
    delivery.producer_decision = Some("unsupported".to_owned());
    let ControlAction::Terminal { code, .. } =
        resolve_action(&ControlAction::None, &delivery, &request)
    else {
        panic!("a permanent decision must still end the session");
    };
    assert_eq!(code, ProducerDecisionReason::Unsupported);

    delivery.producer_decision = Some("reader_failed".to_owned());
    assert_eq!(
        resolve_action(&ControlAction::None, &delivery, &request),
        ControlAction::RetryResource {
            after_ms: NEXT_EXCHANGE_MS,
            reason: ProducerDecisionReason::ReaderFailed,
        },
    );
}

#[test]
fn a_passive_client_is_still_told_nothing_and_counted_suppressed() {
    // The vocabulary fence comes first. A client that never declared
    // `hold` was not withheld from — it was never a candidate.
    let (delivery, mut request) = stalled_starved();
    request.supported_actions = None;
    let action = resolve_action(&ControlAction::None, &delivery, &request);
    assert_eq!(action, ControlAction::None);
    assert_eq!(
        action_metrics(&action, &delivery, &request, false),
        ActionMetrics {
            action: ActionKind::None,
            hold_reason: None,
            suppressed: true,
            recovery_withheld: false,
        },
    );
}

#[test]
fn a_withheld_hold_is_counted_by_reason_and_never_as_suppressed() {
    // Two different facts about one exchange. `suppressed` is a client too
    // old to be told; this is a client deliberately not told. An operator
    // watching the first fall to zero must not see the second in it.
    let (delivery, request) = stalled_starved();
    let action = resolve_action(&ControlAction::None, &delivery, &request);
    assert_eq!(action, ControlAction::None);
    let metrics = action_metrics(&action, &delivery, &request, false);
    assert_eq!(
        metrics,
        ActionMetrics {
            action: ActionKind::None,
            hold_reason: Some(HoldReason::Time),
            suppressed: false,
            recovery_withheld: true,
        },
    );

    let before_withheld =
        CONTROL_RECOVERY_WITHHELD[HoldReason::Time as usize][1].load(Ordering::Relaxed);
    let before_sent = CONTROL_HOLD_REASONS[HoldReason::Time as usize].load(Ordering::Relaxed);
    let before_suppressed = CONTROL_ACTIONS_SUPPRESSED[1].load(Ordering::Relaxed);
    record_action(&action, &delivery, &request, ClientPlatform::Apple, false);
    assert_eq!(
        CONTROL_RECOVERY_WITHHELD[HoldReason::Time as usize][1].load(Ordering::Relaxed),
        before_withheld + 1,
    );
    // A withheld hold was not sent, so the sent counter must not move.
    assert_eq!(
        CONTROL_HOLD_REASONS[HoldReason::Time as usize].load(Ordering::Relaxed),
        before_sent,
    );
    assert_eq!(
        CONTROL_ACTIONS_SUPPRESSED[1].load(Ordering::Relaxed),
        before_suppressed,
    );
}

#[test]
fn every_hold_reason_has_its_own_counter_slot() {
    // The exposition indexes `CONTROL_HOLD_REASONS` by `reason as usize`,
    // so a variant added in the middle would silently report under its
    // neighbour's name. This pins the discriminants to the exposed order.
    for (expected, reason) in [
        HoldReason::Demand,
        HoldReason::Time,
        HoldReason::Bytes,
        HoldReason::Global,
        HoldReason::Ahead,
        HoldReason::WorkingSet,
        HoldReason::NoRoom,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(reason as usize, expected, "{reason:?} moved slot");
    }
    assert_eq!(CONTROL_HOLD_REASONS.len(), 7);
    // The withheld exposition indexes the same discriminant, so it moves
    // with the same variants and is pinned by the same assertion.
    assert_eq!(CONTROL_RECOVERY_WITHHELD.len(), 7);
    assert_eq!(CONTROL_RECOVERY_WITHHELD[0].len(), CLIENT_PLATFORMS);
}

#[test]
fn only_a_verdict_no_retry_can_change_is_permanent() {
    // The split that decides whether a client should ever be told to give
    // up. Seventeen of the twenty-two are timing, process, executor or
    // plan facts: the same file on the same pipeline may work on the next
    // attempt. Five cannot be retried unchanged into working — two
    // verdicts about the source, one about a published rendition, one
    // about this daemon process, and one about the decode.
    //
    // `EngineChanged` is the process one, because the fragment-index
    // engine baseline is a per-process `OnceCell`: once it has moved,
    // every planned rendition fails identically until the node restarts,
    // and a reopen re-plans straight back into it. Telling that client to
    // retry is telling it to poll until an operator intervenes.
    //
    // `SourceDecodeRetry` sits on the impermanent side on purpose, and it
    // is the pair that is easiest to get backwards: it and
    // `SourceDecodeFailed` describe the same finding, and what separates
    // them is whether there is an alternate to install. One is a retry the
    // client should wait through; the other is the answer.
    //
    // `SourceDecodeFailed` is the one that mattered most to get on this
    // side of the line: it is produced when the decoder itself said the
    // source did not decode, and the process very often exits zero.
    // Classified as timing, a client reopens and reproduces it — which is
    // the loop this whole effort exists to end.
    for reason in ProducerDecisionReason::ALL {
        let expected = matches!(
            reason,
            ProducerDecisionReason::Unsupported
                | ProducerDecisionReason::InvalidConfiguration
                | ProducerDecisionReason::SourceDecodeFailed
                | ProducerDecisionReason::RenditionInitChanged
                | ProducerDecisionReason::EngineChanged
        );
        assert_eq!(
            reason.is_permanent(),
            expected,
            "{} classified wrongly",
            reason.status(),
        );
    }
    assert_eq!(
        ProducerDecisionReason::ALL
            .into_iter()
            .filter(|reason| reason.is_permanent())
            .count(),
        5,
    );
    // A permanent reason must carry a sentence a viewer could be shown,
    // not the wire name falling through `terminal_message`'s default.
    for reason in ProducerDecisionReason::ALL
        .into_iter()
        .filter(|r| r.is_permanent())
    {
        assert_ne!(
            terminal_message(reason),
            reason.status(),
            "{} is permanent and needs its own message",
            reason.status(),
        );
    }
}

#[test]
fn local_init_drift_and_process_engine_change_have_distinct_verdicts() {
    let local = ProducerDecisionReason::RenditionInitChanged;
    let process = ProducerDecisionReason::EngineChanged;

    assert!(local.is_permanent(), "the published rendition must stop");
    assert!(process.is_permanent(), "the changed process must stop");
    assert_eq!(
        terminal_message(local),
        "this rendition changed unexpectedly and must be reopened"
    );
    assert!(!terminal_message(local).contains("restart"));
    assert!(terminal_message(process).contains("restart"));
}

#[test]
fn every_decision_reason_round_trips_its_wire_name() {
    // `ALL` is what the wire is bounded against and what metrics will be
    // labelled by. A variant added to the enum and forgotten here would
    // be accepted by the relay check under no name at all.
    for reason in ProducerDecisionReason::ALL {
        assert_eq!(
            ProducerDecisionReason::from_status(reason.status()),
            Some(reason),
        );
    }
    assert_eq!(ProducerDecisionReason::ALL.len(), 22);
    // A variant present in the enum and absent from `ALL` is caught by
    // nothing else. `status` is exhaustive, so the compiler forces a
    // spelling to be written; `ALL` is a hand-maintained array it cannot
    // check, and both this test and the permanence test iterate `ALL` — so
    // a forgotten variant is invisible to both while being unnameable by
    // `from_status`, which is the hazard the paragraph above describes.
    //
    // The match is exhaustive over the variants, so adding one fails to
    // compile here, and the arm it forces you to write is the reminder to
    // extend `ALL` with it.
    for reason in ProducerDecisionReason::ALL {
        match reason {
            ProducerDecisionReason::StartupDeadline
            | ProducerDecisionReason::ProgressDeadline
            | ProducerDecisionReason::ExitClassificationDeadline
            | ProducerDecisionReason::ProcessExit
            | ProducerDecisionReason::PartialSuccessExit
            | ProducerDecisionReason::Unsupported
            | ProducerDecisionReason::InvalidConfiguration
            | ProducerDecisionReason::ReaderFailed
            | ProducerDecisionReason::FlowStopFailed
            | ProducerDecisionReason::FlowResumeFailed
            | ProducerDecisionReason::FlowStopDeadline
            | ProducerDecisionReason::FlowResumeDeadline
            | ProducerDecisionReason::InstallDeadline
            | ProducerDecisionReason::ExecutorLost
            | ProducerDecisionReason::SourceDecodeRetry
            | ProducerDecisionReason::SourceDecodeFailed
            | ProducerDecisionReason::SourceChanged
            | ProducerDecisionReason::RenditionInitChanged
            | ProducerDecisionReason::EngineChanged
            | ProducerDecisionReason::ProducerLaunchFailed
            | ProducerDecisionReason::MediaLandingFailed
            | ProducerDecisionReason::ProducerWriteFailed => {}
        }
    }
    assert_eq!(ProducerDecisionReason::from_status("invented"), None);
    // The names are wire-safe: lowercase, underscored, no spaces.
    for reason in ProducerDecisionReason::ALL {
        let name = reason.status();
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
            "{name} is not a wire-safe name",
        );
    }
}

/// The readiness value is a fact about the *selected* track, and silence
/// is a real answer rather than a missing one.
#[test]
fn subtitle_readiness_speaks_only_for_a_selected_native_track() {
    let native = |track| SubtitleSelection {
        mode: SubtitleMode::Native,
        track: Some(track),
    };

    assert_eq!(
        subtitle_readiness_value(&native(0), Some(SubtitleTrackCache::Ready)).as_deref(),
        Some("ready")
    );
    assert_eq!(
        subtitle_readiness_value(&native(0), Some(SubtitleTrackCache::Warming)).as_deref(),
        Some("warming")
    );
    assert_eq!(
        subtitle_readiness_value(&native(0), Some(SubtitleTrackCache::Unavailable)).as_deref(),
        Some("unavailable")
    );
    // A track the caller could not resolve at all: the client asked for a
    // specific one and deserves to be told to stop waiting, not silence.
    assert_eq!(
        subtitle_readiness_value(&native(0), None).as_deref(),
        Some("unavailable")
    );

    // Every other mode names no sidecar. Burn-in is video and its
    // readiness is the producer's, already reported; overlay is the bitmap
    // path's own lifecycle; off names no track.
    for mode in [SubtitleMode::Off, SubtitleMode::Overlay, SubtitleMode::Burn] {
        let selection = SubtitleSelection {
            mode,
            // `Off` carries no track and the others must; validation
            // enforces that upstream, so match it rather than fight it.
            track: (!matches!(mode, SubtitleMode::Off)).then_some(0),
        };
        assert_eq!(
            subtitle_readiness_value(&selection, Some(SubtitleTrackCache::Ready)),
            None,
            "{mode:?} must not report subtitle readiness"
        );
    }
}

/// An older peer relays a response it built before this field existed.
///
/// The compatibility that matters here is absence: it must deserialize,
/// and must not be read as `ready`. This mirrors the guarantee
/// `producer_decision` already carries — absence means "not classified
/// here", never "healthy". The other direction, a reader meeting a field it
/// does not know, is `a_delivery_view_tolerates_a_field_from_a_newer_node`.
#[test]
fn a_delivery_without_subtitle_readiness_round_trips_as_absent() {
    let json = serde_json::to_string(&delivery_with_hold(None)).expect("serialize");
    assert!(
        !json.contains("subtitle_readiness"),
        "an absent readiness must not be written to the wire at all: {json}"
    );

    let relayed: DeliveryView = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(relayed.subtitle_readiness, None);
    assert_eq!(relayed.subtitle_revision, None);
    assert!(!json.contains("subtitle_revision"));

    let mut ready = delivery_with_hold(None);
    ready.subtitle_readiness = Some("ready".to_owned());
    ready.subtitle_revision = Some("opaque-window-revision".to_owned());
    let round_tripped: DeliveryView =
        serde_json::from_str(&serde_json::to_string(&ready).expect("serialize"))
            .expect("deserialize");
    assert_eq!(round_tripped.subtitle_readiness.as_deref(), Some("ready"));
    assert_eq!(round_tripped.subtitle_revision, ready.subtitle_revision);
}

/// A response written by a node one release ahead must still parse here.
///
/// This is the direction that was fatal, and it is not the one
/// `preparation`'s own rustdoc was written for. A relaying ingress
/// deserializes the owner's answer before returning it
/// (`validated_control_relay_response`), so during a rolling deploy the
/// *older* binary is the one doing the strict parse. With
/// `deny_unknown_fields` on `DeliveryView` that parse failed outright:
/// `PeerTransportError::InvalidResponse`, then 503 `control_unavailable`
/// with a `retry_after_ms` the client honours — every affected viewer
/// re-asking twice a second, renewing no lease and receiving no action,
/// until the deploy finished. `preparation` was that field.
///
/// Reinstating `deny_unknown_fields` on `DeliveryView` fails this test.
#[test]
fn a_delivery_view_tolerates_a_field_from_a_newer_node() {
    let current = ControlResponseV1 {
        protocol: PROTOCOL_V1.to_owned(),
        generation: "relay-forward-compat".to_owned(),
        control_epoch: 1,
        accepted_sequence: 1,
        server_time_unix_ms: 1,
        lease: PlaybackLeaseView {
            state: "active".to_owned(),
            renew_after_ms: NEXT_EXCHANGE_MS,
            expires_at_unix_ms: 1,
        },
        delivery: delivery_with_hold(Some("demand")),
        effective_selection: EffectiveSelection {
            candidate_id: None,
            quality_auto: true,
            height: 720,
            audio_track: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            codec: "h264".to_owned(),
            dynamic_range: Some("sdr".to_owned()),
        },
        action: ControlAction::None,
    };
    let mut body = serde_json::to_value(&current).expect("serialize a current-shape response");
    body["delivery"]
        .as_object_mut()
        .expect("a delivery serializes as an object")
        .insert(
            "a_field_from_next_year".to_owned(),
            serde_json::Value::String("whatever it comes to mean".to_owned()),
        );
    let bytes = serde_json::to_vec(&body).expect("serialize the newer node's body");

    // Exactly what the relay does with the owner's answer.
    let relayed = serde_json::from_slice::<ControlResponseV1>(&bytes)
        .expect("a newer node's response must still parse on an older reader");
    assert_eq!(
        relayed.delivery.hold_reason.as_deref(),
        Some("demand"),
        "and every field this build does know must survive the one it does not",
    );
    assert_eq!(relayed.lease.state, "active");

    // And what the relay then hands the client: it re-serializes what it
    // parsed, so a field it did not understand is dropped rather than
    // forwarded. That is what makes `preparation`'s "absent means not
    // evaluated here, never a decline" a promise a client can rely on.
    let forwarded = serde_json::to_value(&relayed).expect("re-serialize for the client");
    assert!(
        forwarded["delivery"]
            .get("a_field_from_next_year")
            .is_none(),
        "a relay normalizes away what it cannot name: {forwarded}",
    );
    assert!(
        forwarded["delivery"].get("hold_reason").is_some(),
        "without dropping what it can",
    );
}

/// Relays preserve extension values they do not understand. The consumer,
/// not an intermediate server, decides whether a value means `ready`; if a
/// relay normalized this to absence, a value introduced by a future peer
/// could never cross an older node.
#[test]
fn subtitle_readiness_relay_preserves_an_unknown_value() {
    let mut delivery = delivery_with_hold(None);
    delivery.subtitle_readiness = Some("a_value_from_next_year".to_owned());

    let relayed: DeliveryView =
        serde_json::from_str(&serde_json::to_string(&delivery).expect("serialize"))
            .expect("deserialize");

    assert_eq!(
        relayed.subtitle_readiness.as_deref(),
        Some("a_value_from_next_year")
    );
}

fn delivery_with_decision(decision: &str) -> DeliveryView {
    let mut delivery = delivery_with_hold(None);
    delivery.producer_state = "failed".to_owned();
    delivery.producer_decision = Some(decision.to_owned());
    delivery
}

fn accepts_everything() -> ControlRequestV1 {
    let mut request = request();
    request.supported_actions = Some(vec![
        HOLD_ACTION.to_owned(),
        TERMINAL_ACTION.to_owned(),
        RETRY_RESOURCE_ACTION.to_owned(),
        PREPARE_REPLACEMENT_ACTION.to_owned(),
    ]);
    request
}

#[test]
fn a_permanent_decision_ends_the_session_and_a_transient_one_does_not() {
    let client = accepts_everything();
    for reason in ProducerDecisionReason::ALL {
        let delivery = delivery_with_decision(reason.status());
        let action = resolve_action(&ControlAction::None, &delivery, &client);
        if reason.is_permanent() {
            let ControlAction::Terminal { code, message } = action else {
                panic!("{} must end the session", reason.status());
            };
            assert_eq!(code, reason);
            assert!(!message.is_empty());
            assert!(message.len() <= MAX_TERMINAL_MESSAGE_BYTES);
        } else {
            assert_eq!(
                action,
                ControlAction::RetryResource {
                    after_ms: NEXT_EXCHANGE_MS,
                    reason,
                },
                "{} must be retryable",
                reason.status(),
            );
        }
    }
}

#[test]
fn a_stopped_producer_outranks_a_hold() {
    // A client told to hold while the producer is finished would sit
    // through a film that was never going to play. The ranking is what
    // stops that, so it is asserted rather than assumed.
    let client = accepts_everything();
    let mut delivery = delivery_with_decision("unsupported");
    delivery.hold_reason = Some("demand".to_owned());
    assert!(matches!(
        resolve_action(&ControlAction::None, &delivery, &client),
        ControlAction::Terminal { .. },
    ));

    let mut retryable = delivery_with_decision("reader_failed");
    retryable.hold_reason = Some("demand".to_owned());
    assert!(matches!(
        resolve_action(&ControlAction::None, &retryable, &client),
        ControlAction::RetryResource { .. },
    ));
}

#[test]
fn an_undeclared_verdict_is_withheld_rather_than_softened_into_a_hold() {
    // The client accepts `hold` and nothing else. It must not be told to
    // hold when the truth is that the producer stopped: a producer that
    // has stopped is not holding, and saying so would be a comfortable
    // lie that leaves the client waiting on nothing.
    let mut hold_only = request();
    hold_only.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
    let mut delivery = delivery_with_decision("unsupported");
    delivery.hold_reason = Some("demand".to_owned());
    assert_eq!(
        resolve_action(&ControlAction::None, &delivery, &hold_only),
        ControlAction::None,
    );
    assert!(
        action_metrics(&ControlAction::None, &delivery, &hold_only, false).suppressed,
        "the withheld verdict is what the rollout metric counts",
    );
}

#[test]
fn a_relayed_verdict_must_match_the_decision_and_its_permanence() {
    // A peer calling a retryable decision terminal would end a playable
    // session; the reverse would loop a client forever. Neither is
    // forwarded.
    let permanent = ProducerDecisionReason::Unsupported;
    let transient = ProducerDecisionReason::ReaderFailed;
    assert!(permanent.is_permanent() && !transient.is_permanent());
    assert_eq!(
        ControlAction::Terminal {
            code: permanent,
            message: terminal_message(permanent),
        }
        .vocabulary_name(),
        Some(TERMINAL_ACTION),
    );
    assert_eq!(
        ControlAction::RetryResource {
            after_ms: NEXT_EXCHANGE_MS,
            reason: transient,
        }
        .vocabulary_name(),
        Some(RETRY_RESOURCE_ACTION),
    );
    assert_eq!(ControlAction::None.vocabulary_name(), None);
}

#[test]
fn the_new_actions_are_tagged_on_the_wire() {
    assert_eq!(
        serde_json::to_value(ControlAction::Terminal {
            code: ProducerDecisionReason::Unsupported,
            message: "nope".to_owned(),
        })
        .expect("action json"),
        serde_json::json!({
            "type": "terminal",
            "code": "unsupported",
            "message": "nope",
        }),
    );
    assert_eq!(
        serde_json::to_value(ControlAction::RetryResource {
            after_ms: 5_000,
            reason: ProducerDecisionReason::ReaderFailed,
        })
        .expect("action json"),
        serde_json::json!({
            "type": "retry_resource",
            "after_ms": 5_000,
            "reason": "reader_failed",
        }),
    );
}

#[test]
fn a_passive_client_is_never_sent_a_hold_it_cannot_apply() {
    // The whole reason the vocabulary exists. Every M2 reporter stops on
    // an action it does not recognise, so a server that ignored this would
    // silence the fleet the moment production held for any reason at all.
    let passive = request();
    assert!(passive.supported_actions.is_none());
    assert_eq!(
        resolve_action(
            &ControlAction::None,
            &delivery_with_hold(Some("demand")),
            &passive,
        ),
        ControlAction::None,
    );

    let mut names_other_actions = request();
    names_other_actions.supported_actions =
        Some(vec!["terminal".to_owned(), "retry_resource".to_owned()]);
    assert_eq!(
        resolve_action(
            &ControlAction::None,
            &delivery_with_hold(Some("demand")),
            &names_other_actions,
        ),
        ControlAction::None,
    );
}

#[test]
fn a_client_that_accepts_hold_is_told_why_production_is_held() {
    let mut accepting = request();
    accepting.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
    for (delivery_reason, expected) in [
        ("demand", HoldReason::Demand),
        ("time", HoldReason::Time),
        ("bytes", HoldReason::Bytes),
        ("global", HoldReason::Global),
        ("ahead", HoldReason::Ahead),
        ("working_set", HoldReason::WorkingSet),
        ("no_room", HoldReason::NoRoom),
    ] {
        assert_eq!(
            resolve_action(
                &ControlAction::None,
                &delivery_with_hold(Some(delivery_reason)),
                &accepting,
            ),
            ControlAction::Hold {
                reason: expected,
                revisit_after_ms: expected.revisit_after_ms(),
            },
            "delivery reported {delivery_reason}",
        );
    }
    // No hold reported is not a hold. A client that accepts the action
    // must still receive `none` while production is advancing.
    assert_eq!(
        resolve_action(&ControlAction::None, &delivery_with_hold(None), &accepting),
        ControlAction::None,
    );
    // A reason this server does not know is not promoted to an
    // instruction. Reporting it in the delivery view is harmless; telling
    // a client to hold on a word we cannot explain is not.
    assert_eq!(
        resolve_action(
            &ControlAction::None,
            &delivery_with_hold(Some("something_new")),
            &accepting,
        ),
        ControlAction::None,
    );
}

#[test]
fn a_decided_action_outranks_the_advisory_hold() {
    // An action the actor decided is a transaction fenced by
    // `prior_action`; the advisory hold is derived per response. When both
    // exist the transaction is the one with identity, so it wins and its
    // reason is not quietly rewritten by current delivery.
    let mut accepting = request();
    accepting.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
    assert_eq!(
        resolve_action(
            &ControlAction::Hold {
                reason: HoldReason::Demand,
                revisit_after_ms: HoldReason::Demand.revisit_after_ms(),
            },
            &delivery_with_hold(Some("bytes")),
            &accepting,
        ),
        ControlAction::Hold {
            reason: HoldReason::Demand,
            revisit_after_ms: HoldReason::Demand.revisit_after_ms(),
        },
    );
}

#[test]
fn every_hold_reason_round_trips_its_delivery_spelling() {
    // The action and `DeliveryView::hold_reason` are one vocabulary read
    // twice. If either side gains a value without the other, an operator
    // reading the two fields sees different words for one fact — and the
    // relay check below starts refusing valid responses.
    for reason in [
        HoldReason::Demand,
        HoldReason::Time,
        HoldReason::Bytes,
        HoldReason::Global,
        HoldReason::Ahead,
        HoldReason::WorkingSet,
        HoldReason::NoRoom,
    ] {
        assert_eq!(
            HoldReason::from_delivery(reason.as_delivery_str()),
            Some(reason),
        );
    }
    assert_eq!(HoldReason::from_delivery("unheard_of"), None);
}

/// A hold says when it is worth asking again, and `no_room` says a longer
/// number than the rest.
///
/// Without an interval, `hold` is an instruction to wait with no end.
/// That is survivable for the reasons a client clears itself — it is
/// consuming buffer, so the answer changes because it changed it — and it
/// is not survivable for `no_room`, which only something outside this
/// rendition can clear. A client told to hold for that reason could sit
/// indefinitely with nothing to act on.
///
/// The longer interval is the point rather than a detail. Asking again at
/// the ordinary cadence spends exchanges on a node that has already said
/// it has nothing to give, and the ask cannot change the answer.
///
/// It is a revisit contract, not an expiry: nothing fails when the
/// interval passes. That distinction is why this is not a terminal, which
/// would tell a client to tear down a player over a server working
/// exactly as designed.
/// A buffer that does not reach the playhead is not runway.
///
/// `buffered_through_ms` alone cannot tell a comfortable client from one
/// about to starve: it is the end of *a* buffered region, and whether that
/// region contains the playhead is a different fact. The case that matters
/// is an ordinary seek — the anchor moves to the target while the region
/// the client is describing is the one around where it was, so the
/// difference between them is the gap being seeked across, not buffer.
/// Counting it tells the server the viewer is fine at precisely the moment
/// they have nothing, and the server holds production on that.
///
/// The two shapes below report an identical `buffered_through_ms` and must
/// not report identical runway. That is the whole property; everything
/// else here is making sure the honest cases are left alone.
#[test]
fn runway_counts_only_a_buffer_that_reaches_the_playhead() {
    let contiguous = ControlRequestV1 {
        position_ms: 10_000,
        buffered_from_ms: Some(9_000),
        buffered_through_ms: 40_000,
        ..request()
    };
    assert_eq!(
        contiguous_runway_ms(&contiguous, 10_000),
        30_000,
        "a region containing the playhead is runway to its end",
    );

    let scattered = ControlRequestV1 {
        buffered_from_ms: Some(25_000),
        ..contiguous.clone()
    };
    assert_eq!(
        contiguous_runway_ms(&scattered, 10_000),
        0,
        "the same end, with a hole at the playhead, is not 30 seconds of \
             comfort — it is nothing to play",
    );

    let forward = ControlRequestV1 {
        render_state: RenderState::Seeking,
        seek_target_ms: Some(600_000),
        ..contiguous.clone()
    };
    assert_eq!(forward.validate(Some(900_000), 2_000), Ok(()));
    assert_eq!(contiguous_runway_ms(&forward, 600_000), 0);

    // No contiguity evidence is not a hole. Every client that predates the
    // field sends this, and so does one that could not identify a region
    // containing its playhead; telling either that it has nothing would
    // stall production for clients that are fine.
    let silent = ControlRequestV1 {
        buffered_from_ms: None,
        ..contiguous.clone()
    };
    assert_eq!(
        contiguous_runway_ms(&silent, 10_000),
        30_000,
        "a client that says nothing about contiguity keeps the old reading",
    );

    // And a region starting exactly at the playhead is contiguous.
    let exact = ControlRequestV1 {
        buffered_from_ms: Some(10_000),
        ..contiguous
    };
    assert_eq!(contiguous_runway_ms(&exact, 10_000), 30_000);
}

#[test]
fn seek_buffers_are_valid_and_wire_and_producer_runway_agree() {
    // Includes all deployed platforms, omitted-range legacy snapshots,
    // no buffer after a late landing, and an actually buffered seek.
    for platform in [
        ClientPlatform::Web,
        ClientPlatform::Apple,
        ClientPlatform::Android,
    ] {
        for (position, from, through, target, runway) in [
            (10_000, Some(8_000), 24_000, 600_000, 0),
            (600_000, Some(598_000), 620_000, 10_000, 0),
            (600_000, Some(10_000), 30_000, 10_000, 20_000),
            (600_000, None, 620_000, 10_000, 0),
            (797_711, None, 797_711, 729_643, 0),
            (797_711, Some(797_711), 809_211, 729_643, 0),
            (20_000, Some(8_000), 30_000, 10_000, 20_000),
            (10_000, Some(8_000), 30_000, 20_000, 10_000),
            (10_000, None, 30_000, 20_000, 10_000),
            (10_000, None, 30_000, 10_000, 20_000),
        ] {
            let mut request = request();
            request
                .capabilities
                .as_mut()
                .expect("fixture capabilities")
                .platform = platform;
            request.position_ms = position;
            request.buffered_from_ms = from;
            request.buffered_through_ms = through;
            request.render_state = RenderState::Seeking;
            request.seek_target_ms = Some(target);
            assert_eq!(request.validate(Some(900_000), 6_000), Ok(()));
            assert_eq!(contiguous_runway_ms(&request, target), runway);
            assert_eq!(PlaybackDemandSnapshot::from(&request).runway_ms(), runway);
        }
    }
}

#[test]
fn seeking_still_rejects_malformed_buffers_and_targets() {
    let seek = ControlRequestV1 {
        render_state: RenderState::Seeking,
        seek_target_ms: Some(600_000),
        ..request()
    };
    for (invalid, field) in [
        (
            ControlRequestV1 {
                position_ms: -1,
                ..seek.clone()
            },
            "position_ms",
        ),
        (
            ControlRequestV1 {
                buffered_through_ms: -1,
                ..seek.clone()
            },
            "buffered_through_ms",
        ),
        (
            ControlRequestV1 {
                buffered_through_ms: 906_001,
                ..seek.clone()
            },
            "buffered_through_ms",
        ),
        (
            ControlRequestV1 {
                buffered_from_ms: Some(26_000),
                ..seek.clone()
            },
            "buffered_from_ms",
        ),
        (
            ControlRequestV1 {
                seek_target_ms: None,
                ..seek.clone()
            },
            "seek_target_ms",
        ),
        (
            ControlRequestV1 {
                seek_target_ms: Some(906_001),
                ..seek.clone()
            },
            "seek_target_ms",
        ),
    ] {
        assert_eq!(invalid.validate(Some(900_000), 6_000), Err(field));
    }
}

/// What survives a restart is the failures, and only the failures.
///
/// `record_action` counts into process atomics, so a viewer terminated at
/// 21:04 leaves nothing behind once that process exits — which tends to be
/// the same restart that prompted the question. The durable half exists
/// for the person asking afterwards.
///
/// Both halves of the rule are asserted, and the second is the one that
/// keeps the ledger readable: an exchange happens every few seconds per
/// viewer, so keeping the healthy path would bury the four rows that
/// matter under millions that do not, and turn ordinary playback into a
/// write-heavy workload.
#[test]
fn only_the_outcomes_worth_explaining_later_are_kept() {
    let request = request();
    let mut delivery = delivery_with_hold(None);
    delivery.client_runway_ms = 2_500;
    delivery.delivered_bps = Some(1_200_000);

    let kept = |action: &ControlAction| {
        let metrics = action_metrics(action, &delivery, &request, false);
        durable_outcome(action, &delivery, &request, &metrics, 1_700_000_000_000)
    };

    let terminal = ControlAction::Terminal {
        code: ProducerDecisionReason::Unsupported,
        message: "the file is gone".to_owned(),
    };
    let event = kept(&terminal).expect("a terminal must outlive the process that sent it");
    assert_eq!(event.event, "control_terminal");
    assert_eq!(event.level.as_deref(), Some("error"));
    assert!(
        event
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("the file is gone")),
        "the message the viewer's client was given is the thing being explained",
    );
    assert_eq!(
        event.runway_ds,
        Some(25),
        "what the client had left is what makes a terminal explicable rather \
             than merely recorded",
    );
    assert_eq!(event.delivered_bps, Some(1_200_000));

    let retry = ControlAction::RetryResource {
        after_ms: 4_000,
        reason: ProducerDecisionReason::ProgressDeadline,
    };
    assert_eq!(
        kept(&retry)
            .expect("a backoff leaves nothing behind by design")
            .event,
        "control_retry_resource",
    );

    // The healthy path, which must not be written.
    for quiet in [
        ControlAction::None,
        ControlAction::Hold {
            reason: HoldReason::WorkingSet,
            revisit_after_ms: NEXT_EXCHANGE_MS,
        },
    ] {
        assert!(
            kept(&quiet).is_none(),
            "{quiet:?} is routine; a ledger that records the healthy path is \
                 one nobody can read",
        );
    }
}

/// Silence is an outcome too, and it is the one nothing else records.
///
/// Two different silences, and they are separate rows because they have
/// separate causes. A *withheld* recovery is one the ranking decided to
/// send and the client could not receive; a *suppressed* action is the
/// broader case of any decision this server could name going unsent. Both
/// leave a response that looks entirely ordinary on the wire, which is why
/// neither is visible without a row.
///
/// Asserted by exact event name. An earlier version of this test accepted
/// either name, and disabling the withheld branch outright still passed it
/// — the case fell through to `suppressed` and nothing noticed. A test
/// that accepts two answers proves neither.
#[test]
fn a_withheld_recovery_and_a_suppressed_action_are_separate_rows() {
    // The recovery ranking's own preconditions, so this is genuinely the
    // withheld path rather than the general one wearing its name.
    let mut stalled = request();
    // Accepts hold — that is the point. A withheld recovery is a hold the
    // client could have received and the ranking chose not to send,
    // because production state must not veto fetching bytes already
    // published. A client that cannot receive a hold is the *other* case.
    stalled.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
    stalled.render_state = RenderState::Stalled;
    stalled.observation = Some(ClientObservation {
        dropped_frames: None,
        decoder_state: Some(DecoderState::Starved),
        error_code: None,
        error_detail: None,
    });
    let mut delivery = delivery_with_hold(Some("no_room"));
    delivery.client_runway_ms = 0;
    delivery.produced_through_ms = Some(120_000);
    delivery.fetched_through_ms = 60_000;
    assert!(
        recovery_outranks_hold(&delivery, &stalled),
        "the fixture must actually reach the withheld path",
    );

    let action = resolve_action(&ControlAction::None, &delivery, &stalled);
    let metrics = action_metrics(&action, &delivery, &stalled, false);
    assert!(metrics.recovery_withheld, "this is the withheld case");
    let event = durable_outcome(&action, &delivery, &stalled, &metrics, 1_700_000_000_000)
        .expect("the withholding is the fact worth keeping");
    assert_eq!(event.event, "control_hold_withheld");
    assert_eq!(event.level.as_deref(), Some("warn"));
    assert_eq!(
        event.hold_reason.as_deref(),
        Some("no_room"),
        "the reason the server could not pass on is the point of the row",
    );

    // And the broader case, which must not be filed under the narrow name.
    let mut passive = request();
    passive.supported_actions = Some(vec![]);
    let ordinary = delivery_with_hold(Some("working_set"));
    let action = resolve_action(&ControlAction::None, &ordinary, &passive);
    let metrics = action_metrics(&action, &ordinary, &passive, false);
    assert!(!metrics.recovery_withheld);
    assert!(metrics.suppressed);
    let event = durable_outcome(&action, &ordinary, &passive, &metrics, 1_700_000_000_000)
        .expect("a decision that went unsent is still an outcome");
    assert_eq!(event.event, "control_action_suppressed");
}

/// A commit proves what the client built, not what time it was.
///
/// `Committed` used to require `first_frame_unix_ms`, which made a
/// wall-clock reading from an unsynchronised device the gate on a durable
/// server commit. It proves neither ownership, nor freshness, nor that
/// what was displayed is what was offered — a client whose clock is wrong
/// is not a client whose switch failed, and a client that lies about the
/// time still gets its commit.
///
/// What the server can actually contradict is the origin. Between
/// `Prepare` and `Committed` its own intent can move — a seek, a quality
/// change, a new recipe — and an acknowledgement carrying only an
/// `action_id` cannot distinguish a client that committed to the current
/// offer from one that committed to a stale one and is about to present
/// media nobody asked for.
///
/// First-frame time stays accepted, because it is genuinely useful for
/// diagnosis. It is simply not the proof.
/// A commit for a different origin is not this offer's commit.
///
/// This is the assertion that makes the field worth carrying. Validation
/// only proves a client *said* something; refusing the mismatch is what
/// lets the server contradict a stale commit, and without this test the
/// field would be a required value nothing reads.
///
/// The gap is narrower than it looks, and worth stating precisely: a
/// changed **ask** — quality, codec, dynamic range, subtitles — is already
/// refused at commit by `desired_digest`. What the digest does not cover
/// is **position**. So the case this closes is a successor prepared for
/// one point in the film and acknowledged after the viewer seeked
/// somewhere else: previously indistinguishable from a correct commit, and
/// published using the client's own acknowledgement as the authority.
#[test]
fn a_commit_naming_an_origin_the_offer_did_not_is_refused() {
    let offered_origin_ms = 600_000;
    let action_id = uuid::Uuid::new_v4().to_string();
    let staged = uuid::Uuid::new_v4().to_string();

    let bound = |origin: Option<i64>, state| {
        let control = ControlState {
            prepared_action: Some(PreparedActionBinding {
                successor: PreparedSuccessorAction {
                    enhancement: None,
                    control: None,
                    staged_incarnation_id: staged.clone(),
                    deadline_ms: i64::MAX,
                    session_id: "session".to_owned(),
                    playlist_url: "/hls/session/index.m3u8".to_owned(),
                    media_origin_ms: offered_origin_ms,
                    effective_selection: EffectiveSelection {
                        candidate_id: None,
                        quality_auto: true,
                        height: 1080,
                        audio_track: None,
                        subtitle_burn: None,
                        audio_offset_ms: 0,
                        codec: "h264".to_owned(),
                        dynamic_range: None,
                    },
                },
                action: ControlAction::Prepare {
                    enhancement: None,
                    control: None,
                    action_id: action_id.clone(),
                    session_id: "session".to_owned(),
                    playlist_url: "/hls/session/index.m3u8".to_owned(),
                    media_origin_ms: offered_origin_ms,
                    effective_selection: EffectiveSelection {
                        candidate_id: None,
                        quality_auto: true,
                        height: 1080,
                        audio_track: None,
                        subtitle_burn: None,
                        audio_offset_ms: 0,
                        codec: "h264".to_owned(),
                        dynamic_range: None,
                    },
                },
                acknowledgement: None,
            }),
            ..ControlState::default()
        };
        control.bound_preparation_acknowledgement(Some(&ActionAcknowledgement {
            action_id: action_id.clone(),
            state,
            buffered_through_ms: None,
            committed_media_origin_ms: origin,
            first_frame_unix_ms: Some(1_700_000_000_000),
        }))
    };

    assert_eq!(
        bound(Some(offered_origin_ms), AcknowledgementState::Committed),
        Some(staged.clone()),
        "the commit that echoes the offer binds to the staged successor",
    );
    assert_eq!(
        bound(
            Some(offered_origin_ms + 30_000),
            AcknowledgementState::Committed
        ),
        None,
        "a successor built for a different point in the film is not this \
             offer's commit, and must not publish on the strength of it",
    );
    assert_eq!(
        bound(Some(0), AcknowledgementState::Committed),
        None,
        "zero is a real origin, not a wildcard",
    );

    // Progress toward a successor that is not being published yet reports
    // no origin, and demanding one would make ordinary progress
    // unreportable.
    assert_eq!(
        bound(None, AcknowledgementState::BufferReady),
        Some(staged),
        "only a commit is held to the offered origin",
    );
}

#[test]
fn a_commit_carries_the_origin_it_adopted_and_not_a_wall_clock() {
    let ack = |state, origin, first_frame| ActionAcknowledgement {
        action_id: "8a1f2c3d-4b5e-6f70-8192-a3b4c5d6e7f8".to_owned(),
        state,
        buffered_through_ms: Some(30_000),
        committed_media_origin_ms: origin,
        first_frame_unix_ms: first_frame,
    };

    assert_eq!(
        ack(
            AcknowledgementState::Committed,
            None,
            Some(1_700_000_000_000)
        )
        .validate(),
        Err("acknowledgement.committed_media_origin_ms"),
        "a first-frame time is not a substitute for saying what was built",
    );
    assert_eq!(
        ack(AcknowledgementState::Committed, Some(0), None).validate(),
        Err("acknowledgement.first_frame_unix_ms"),
        "and the origin does not replace the measurement either — this is \
             additive, so a commit carries both",
    );
    assert!(
        ack(
            AcknowledgementState::Committed,
            Some(600_000),
            Some(1_700_000_000_000),
        )
        .validate()
        .is_ok(),
        "origin zero is an ordinary film start rather than a missing \
             value, and a complete commit is accepted",
    );
    assert!(ack(AcknowledgementState::Committed, Some(0), Some(1))
        .validate()
        .is_ok(),);

    // The other states are unchanged: only a commit claims to have built
    // something, so only a commit has to say what.
    assert!(ack(AcknowledgementState::MetadataReady, None, None)
        .validate()
        .is_ok(),);
    assert!(
        ack(AcknowledgementState::Failed, None, None)
            .validate()
            .is_ok(),
        "a failure has no origin to report, and demanding one would make \
             the client's own error unreportable",
    );

    // Range, so a garbage origin is refused at the door rather than
    // compared against an offer it could never match.
    assert_eq!(
        ack(AcknowledgementState::Committed, Some(-1), None).validate(),
        Err("acknowledgement.committed_media_origin_ms"),
    );
    assert_eq!(
        ack(
            AcknowledgementState::Committed,
            Some(MAX_MEDIA_MILLIS + 1),
            None,
        )
        .validate(),
        Err("acknowledgement.committed_media_origin_ms"),
    );
}

#[test]
fn a_hold_says_when_to_ask_again_and_no_room_says_later() {
    for reason in [
        HoldReason::Demand,
        HoldReason::Time,
        HoldReason::Bytes,
        HoldReason::Global,
        HoldReason::Ahead,
        HoldReason::WorkingSet,
    ] {
        assert_eq!(
            reason.revisit_after_ms(),
            NEXT_EXCHANGE_MS,
            "{reason:?} lifts as the client consumes, so the ordinary \
                 cadence is the honest interval",
        );
    }
    assert!(
        HoldReason::NoRoom.revisit_after_ms() > NEXT_EXCHANGE_MS,
        "nothing the client does clears no_room, so asking at the ordinary \
             cadence is work spent on a node already out of room",
    );

    // And the resolved action carries it, so a client reads the interval
    // from the action it was given rather than inferring one.
    let delivery = delivery_with_hold(Some("no_room"));
    let mut client = request();
    client.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
    let resolved = resolve_action(&ControlAction::None, &delivery, &client);
    assert_eq!(
        resolved,
        ControlAction::Hold {
            reason: HoldReason::NoRoom,
            revisit_after_ms: NO_ROOM_REVISIT_MS,
        },
        "the hold a client receives says when to come back",
    );
}

#[test]
fn the_action_is_tagged_on_the_wire() {
    assert_eq!(
        serde_json::to_value(ControlAction::Hold {
            reason: HoldReason::WorkingSet,
            revisit_after_ms: HoldReason::WorkingSet.revisit_after_ms(),
        })
        .expect("action json"),
        // Additive: a client that predates the field ignores it and holds
        // exactly as it did, which is why this is a bound on the existing
        // action rather than a new one every client has to learn.
        serde_json::json!({
            "type": "hold",
            "reason": "working_set",
            "revisit_after_ms": NEXT_EXCHANGE_MS,
        }),
    );
    assert_eq!(
        serde_json::to_value(ControlAction::None).expect("action json"),
        serde_json::json!({"type": "none"}),
    );
}

/// The preparation action names itself, and is not passive.
///
/// It is the first action that is a *transaction* — a durable row exists
/// and the actor's slot is taken — so `resolve_action` must let it through
/// unchanged rather than recomputing it from delivery the way it does the
/// advisory hold. `is_passive` is what decides that, and a `Prepare` that
/// answered `true` would be silently replaced by whatever production
/// happened to be doing.
#[test]
fn the_preparation_action_is_a_transaction_not_a_report() {
    let prepare = prepare_action();
    assert_eq!(
        prepare.vocabulary_name(),
        Some("prepare_replacement"),
        "roadmap §3.1 names the three fenced actions; this is the first",
    );
    assert!(!prepare.is_passive());

    let delivery = delivery_view();
    let mut asking = request();
    asking.supported_actions = Some(vec![
        HOLD_ACTION.to_owned(),
        PREPARE_REPLACEMENT_ACTION.to_owned(),
    ]);
    assert_eq!(
        resolve_action(&prepare, &delivery, &asking),
        prepare,
        "a decided preparation is returned exactly, not recomputed",
    );
    assert_eq!(
        action_metrics(&prepare, &delivery, &asking, false).action,
        ActionKind::Prepare,
        "and it occupies its own metric slot rather than another's",
    );
    assert_eq!(
        resolve_action(&prepare, &delivery, &request()),
        prepare,
        "the actor already froze the vocabulary-resolved transaction, so response assembly must not re-gate an exact replay",
    );
    assert!(
        action_metrics(&ControlAction::None, &delivery, &request(), true,).suppressed,
        "an actor-suppressed preparation remains visible to rollout metrics",
    );
}

/// A relayed preparation is the one action that hands a client a URL, and
/// a peer must not be able to point it anywhere.
///
/// The other believability checks ask whether the peer is telling the
/// truth about production. This one asks something sharper: an arbitrary
/// `playlist_url` is an open redirect into a viewer's second player, so
/// the URL must address the session the same action names. An unstaged
/// successor costs a failed prime; an arbitrary URL costs the playback.
#[test]
fn a_relayed_preparation_cannot_point_a_client_anywhere() {
    let session = uuid::Uuid::new_v4().to_string();
    let honest = ControlAction::Prepare {
        control: None,
        enhancement: None,
        action_id: uuid::Uuid::new_v4().to_string(),
        session_id: session.clone(),
        playlist_url: format!("/api/v1/hls/{session}/master.m3u8"),
        media_origin_ms: 0,
        effective_selection: prepared_selection(),
    };
    assert!(believable(&honest), "a successor addressed by its own id");

    // Each rejection is spelled out in full rather than by struct update,
    // so the field under test is visible at the assertion.
    let ControlAction::Prepare {
        action_id,
        session_id,
        media_origin_ms,
        effective_selection,
        ..
    } = honest.clone()
    else {
        unreachable!("constructed as Prepare")
    };
    for (case, action) in [
        (
            "a protocol-relative URL onto another host",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("//attacker.invalid/{session_id}/master.m3u8"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "an absolute URL onto another host",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("https://example.invalid/{session_id}/master.m3u8"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "no session at all",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: String::new(),
                playlist_url: "/api/v1/hls//master.m3u8".to_owned(),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "no action id to fence the acknowledgement with",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: String::new(),
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}/master.m3u8"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "an action id that acknowledgements cannot parse",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: "not-a-uuid".to_owned(),
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}/master.m3u8"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "a backslash-relative URL, which browsers treat as //",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/\\attacker.invalid/{session_id}/master.m3u8"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "the session named only in a query parameter",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/anything/at/all.m3u8?session={session_id}"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "the session named only in a fragment",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/anything/at/all.m3u8#{session_id}"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "a path that climbs out of the session's own",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}/../../../etc/master.m3u8"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "encoded dot segments normalized after validation",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}/%2e%2e/%2e%2e/settings"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "an internal backslash normalized as a path separator",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}\\..\\settings"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "an arbitrary same-origin route containing the session id",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/settings/{session_id}/master.m3u8"),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "a session id that is not one",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: "a".to_owned(),
                playlist_url: "//attacker.invalid/a/master.m3u8".to_owned(),
                media_origin_ms,
                effective_selection: effective_selection.clone(),
            },
        ),
        (
            "an origin before the start of the film",
            ControlAction::Prepare {
                enhancement: None,
                control: None,
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}/master.m3u8"),
                media_origin_ms: -1,
                effective_selection: effective_selection.clone(),
            },
        ),
    ] {
        assert!(!believable(&action), "{case}");
    }

    let mut invalid_selection = effective_selection;
    invalid_selection.height = crate::transcode::MAX_HEIGHT + 1;
    let invalid_nested = ControlAction::Prepare {
        control: None,
        enhancement: None,
        action_id,
        session_id: session_id.clone(),
        playlist_url: format!("/api/v1/hls/{session_id}/master.m3u8"),
        media_origin_ms,
        effective_selection: invalid_selection,
    };
    assert!(
        !believable(&invalid_nested),
        "the nested prepared selection crosses the same trust boundary",
    );

    let local_selection = prepared_selection();
    assert!(prepared_payload_is_valid(
        None,
        &session_id,
        &format!("/api/v1/hls/{session_id}/index.m3u8"),
        media_origin_ms,
        &local_selection,
    ));
}

/// A client that never declared the vocabulary is never handed one.
///
/// This is what makes shipping the server half ahead of the client half
/// safe: an older client sees the action it has always seen.
#[test]
fn a_relayed_preparation_needs_the_declared_vocabulary() {
    let session = uuid::Uuid::new_v4().to_string();
    let action = ControlAction::Prepare {
        control: None,
        enhancement: None,
        action_id: uuid::Uuid::new_v4().to_string(),
        session_id: session.clone(),
        playlist_url: format!("/api/v1/hls/{session}/master.m3u8"),
        media_origin_ms: 0,
        effective_selection: prepared_selection(),
    };
    assert!(believable(&action));
    assert!(
        !believable_to(&action, &[HOLD_ACTION]),
        "a client that declared only hold",
    );
    assert!(
        !believable_to(&action, &[]),
        "and one that declared nothing"
    );
}

fn prepare_action() -> ControlAction {
    let session = uuid::Uuid::new_v4().to_string();
    ControlAction::Prepare {
        enhancement: None,
        control: None,
        action_id: uuid::Uuid::new_v4().to_string(),
        session_id: session.clone(),
        playlist_url: format!("/api/v1/hls/{session}/master.m3u8"),
        media_origin_ms: 6_000,
        effective_selection: prepared_selection(),
    }
}

fn believable(action: &ControlAction) -> bool {
    believable_to(action, &[HOLD_ACTION, PREPARE_REPLACEMENT_ACTION])
}

/// Whether a peer's response carrying `action` would be forwarded to a
/// client that declared `declared`.
fn believable_to(action: &ControlAction, declared: &[&str]) -> bool {
    let mut control = request();
    control.supported_actions = Some(declared.iter().map(|name| (*name).to_owned()).collect());
    let response = ControlResponseV1 {
        protocol: PROTOCOL_V1.to_owned(),
        generation: control.generation.clone(),
        control_epoch: 1,
        accepted_sequence: 1,
        server_time_unix_ms: 1,
        lease: PlaybackLeaseView {
            state: "active".to_owned(),
            renew_after_ms: NEXT_EXCHANGE_MS,
            expires_at_unix_ms: 1,
        },
        delivery: delivery_view(),
        effective_selection: prepared_selection(),
        action: action.clone(),
    };
    let relay = ControlRelayRequest {
        session_id: uuid::Uuid::new_v4().to_string(),
        generation: control.generation.clone(),
        expected_owner_node_id: "n-test".to_owned(),
        expected_owner_epoch: 1,
        deadline_unix_ms: 1,
        control,
    };
    response.action_is_believable(&relay)
}

fn delivery_view() -> DeliveryView {
    DeliveryView {
        presentation: "vod".to_owned(),
        producer_state: "vod".to_owned(),
        produced_through_ms: None,
        fetched_through_ms: 0,
        delivered_bps: None,
        delivered_idle_ms: None,
        recent_producer_speed: None,
        client_runway_ms: 0,
        admitted: None,
        producer_decision: None,
        hold_reason: None,
        subtitle_readiness: None,
        subtitle_revision: None,
        preparation: None,
        owner_node_hash: "n-test".to_owned(),
        owner_epoch: 1,
    }
}

/// One request carrying a new ask and the old ask's commit does not commit
/// the old one.
///
/// This is the ordering §1 names, and the reason it cannot be fixed by
/// extending `observe`. Both engines call `observe` *after* `accept`, so
/// the desired state `observe` records is one exchange behind every
/// decision `accept` takes — harmless everywhere except here. A client that
/// changes quality and, in the same packet, acknowledges the successor
/// staged for the previous quality, used to have that acknowledgement
/// accepted as authority to commit: the viewer's own message publishing
/// media the viewer had, in that same message, moved off. By the time
/// `observe` saw the new ask the commit directive already existed.
///
/// So acceptance learns the ask first, and the slot records what it was
/// staged for. The acknowledgement is then judged against the ask that is
/// current at that instant, and a successor nobody is waiting for is torn
/// down rather than published — with `acknowledgement_rejected`, so the
/// response tells the client its commit did not happen instead of
/// reporting a success it did not get.
#[test]
fn a_new_ask_in_the_same_packet_refuses_the_old_asks_commit() {
    let request = request();
    let asked_for = selection_at(QualitySelection::Manual { height: 720 });
    let moved_to = selection_at(QualitySelection::Manual { height: 1080 });
    assert_ne!(
        asked_for.desired().digest(),
        moved_to.desired().digest(),
        "the fixture must actually change the ask"
    );

    let commit_for = |staged_for: &ClientSelection, asking: &ClientSelection| {
        let mut state = ControlState::default();
        let started = Instant::now();
        let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
        assert!(state.stage_preparation(
            staged_incarnation_id.clone(),
            request.generation.clone(),
            i64::MAX,
            Some(staged_for.desired().digest()),
        ));
        let prepared_session_id = uuid::Uuid::new_v4().to_string();
        let successor = PreparedSuccessorAction {
            enhancement: None,
            control: None,
            staged_incarnation_id: staged_incarnation_id.clone(),
            deadline_ms: i64::MAX,
            session_id: prepared_session_id.clone(),
            playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
            media_origin_ms: 42_000,
            effective_selection: prepared_selection(),
        };

        // The exchange that announces the successor, under the ask it was
        // staged for.
        let announced = state
            .accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor))
                    .asking(staged_for),
            )
            .expect("the successor is announced");
        let ControlAction::Prepare {
            action_id,
            media_origin_ms: offered_origin_ms,
            ..
        } = &announced.2
        else {
            panic!("an occupied preparation slot announces its successor");
        };

        // And the next exchange: the client acknowledges that Prepare as
        // committed, while asking for `asking`.
        state
            .accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &request.client_instance_id,
                2,
                ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor))
                    .asking(asking)
                    .acknowledging(ActionAcknowledgement {
                        action_id: action_id.clone(),
                        state: AcknowledgementState::Committed,
                        buffered_through_ms: None,
                        // Echo the offer this test announced; a commit naming a
                        // different origin is refused.
                        committed_media_origin_ms: Some(*offered_origin_ms),
                        first_frame_unix_ms: None,
                    }),
            )
            .expect("the acknowledgement is accepted as an exchange");
        (state.preparation_directive(), staged_incarnation_id)
    };

    // Unchanged ask: this is the ordinary handover and it must still work.
    let (directive, staged) = commit_for(&asked_for, &asked_for);
    assert_eq!(
        directive,
        Some(PreparationDirective::Commit {
            staged_incarnation_id: staged
        }),
        "a viewer who has not changed their mind still gets the successor they waited for"
    );

    // Changed ask in the same packet: the commit is refused and the
    // successor torn down, and the client is told its acknowledgement was
    // rejected rather than being reported a success.
    let (directive, staged) = commit_for(&asked_for, &moved_to);
    assert_eq!(
        directive,
        Some(PreparationDirective::Abort {
            staged_incarnation_id: staged,
            acknowledgement_rejected: true,
        }),
        "a successor built for an ask the viewer has left is not published by their own \
             acknowledgement"
    );
}

/// The ask is offered for persistence until it has been, and then not
/// again.
///
/// Two failures this guards against, pulling in opposite directions. A
/// state that offered the ask on every exchange would put a durable write
/// on a path that runs about once a second per client, for a row that is
/// already correct — and a control exchange that can fail on a store
/// hiccup is a control exchange that fails on heartbeats. A state that
/// stopped offering it after the first *attempt* would leave the store one
/// ask behind whenever a write failed, with nothing left to notice: the
/// client has been refused, retries, and the retry says nothing needs
/// writing.
///
/// So the offer is retired by [`ControlState::record_desired_persisted`]
/// and by nothing else, which is what makes a failed write cost exactly
/// one refused exchange rather than a silently stale row.
#[test]
fn an_ask_is_offered_for_persistence_until_the_write_has_landed() {
    let mut state = ControlState::default();
    let first = selection_at(QualitySelection::Auto {
        height: None,
        candidate_id: None,
    });
    let second = selection_at(QualitySelection::Manual { height: 720 });

    // A session's opening ask has to be written too: without it the store
    // holds nothing for a viewer who never changes their mind, and an
    // admission point comparing against "no row" cannot tell that from a
    // viewer who has asked for something new.
    let offer = state
        .observe(&first, None)
        .persist_desired
        .expect("the opening ask is offered");
    assert_eq!(offer.digest, first.desired().digest());
    assert_eq!(offer.canonical_form, first.desired().canonical_form());

    // Still offered while the write has not landed — a refused exchange
    // must leave the next one asking for the same thing.
    assert_eq!(
        state.observe(&first, None).persist_desired,
        Some(offer.clone()),
        "an unacknowledged write is still outstanding"
    );

    state.record_desired_persisted(&offer.digest);
    assert_eq!(
        state.observe(&first, None).persist_desired,
        None,
        "and once it has landed, a heartbeat repeating the same ask writes nothing"
    );

    // A new ask is offered again.
    let next = state
        .observe(&second, None)
        .persist_desired
        .expect("a changed ask is offered");
    assert_ne!(next.digest, offer.digest);

    // A stale acknowledgement — the viewer moved on while the write was in
    // flight — does not retire the current offer. Accepting it would mark
    // the row as naming an ask it does not name.
    state.record_desired_persisted(&offer.digest);
    assert_eq!(
        state.observe(&second, None).persist_desired,
        Some(next.clone()),
        "a write that landed for an ask the viewer has already left retires nothing"
    );
    state.record_desired_persisted(&next.digest);
    assert_eq!(state.observe(&second, None).persist_desired, None);
}

/// An ask that arrives while the slot is busy is not lost.
///
/// The old gate asked "did this packet differ from the last one", which is
/// true for exactly one exchange. A viewer who changed quality while a
/// successor was already in flight therefore had their candidate built,
/// refused by the occupied slot, and never rebuilt — they simply never got
/// the thing they asked for, and nothing said so.
///
/// The gate now asks "has the current ask been dispatched", which stays
/// true across exchanges until it has been. Coalescing falls out for free:
/// only the latest ask is ever held, so three changes while the slot is
/// busy leave one candidate to build rather than three.
#[test]
fn an_ask_arriving_while_the_slot_is_busy_is_dispatched_once_it_frees() {
    let mut state = ControlState::default();
    let first = selection_at(QualitySelection::Auto {
        height: None,
        candidate_id: None,
    });
    let second = selection_at(QualitySelection::Manual { height: 720 });
    let third = selection_at(QualitySelection::Manual { height: 1080 });

    // The session's own ask dispatches nothing: there is no change to
    // prepare for, and this is the first-exchange behaviour the old gate
    // had too.
    assert!(!state.observe(&first, None).dispatch_preparation);
    assert!(
        !state.observe(&first, None).dispatch_preparation,
        "an unchanged steady state stays silent"
    );

    // A real change, with the slot free.
    assert!(state.observe(&second, None).dispatch_preparation);
    assert!(
        !state.observe(&second, None).dispatch_preparation,
        "and is dispatched once, not on every exchange after it"
    );

    // That candidate takes the slot, and the viewer changes their mind
    // again — three times, to prove the coalescing.
    assert!(state.stage_preparation(
        uuid::Uuid::new_v4().to_string(),
        "predecessor".to_owned(),
        i64::MAX,
        Some(second.desired().digest()),
    ));
    for _ in 0..3 {
        assert!(
            !state.observe(&third, None).dispatch_preparation,
            "an occupied slot cannot take a successor, so nothing is dispatched into it"
        );
    }

    // The slot frees. The next exchange picks up the ask that has been
    // waiting — without a wake-up path, and without having queued three
    // copies of it.
    let staged = state
        .preparation
        .staged_incarnation_id()
        .expect("the fixture staged one")
        .to_owned();
    assert!(state.abort_preparation(&staged));
    state.preparation = PreparationSlot::Empty;
    assert!(
        state.observe(&third, None).dispatch_preparation,
        "the ask the viewer has been waiting on is built once the slot can take it"
    );
    assert!(
        !state.observe(&third, None).dispatch_preparation,
        "and only once"
    );
}

/// A packet the server refused cannot move what the viewer is understood
/// to want.
///
/// The ask is advanced inside acceptance, downstream of every fence. A
/// replay is the same exchange arriving twice and advanced the ask the
/// first time or not at all; a rate-limited packet was never accepted.
///
/// This reads the field rather than a consequence, and the first version of
/// it did the opposite and proved nothing. Today the only consumer of the
/// ask — `reserve_preparation_commit` — runs inside the same call that
/// writes it, so a rejected packet advancing it is immediately overwritten
/// by the next accepted one and no behaviour differs. Moving the write
/// above the rate-limit fence passed that test. It fails this one.
///
/// The property is worth pinning now anyway, because it stops being
/// unobservable the moment §1's durable desired-ownership row exists: that
/// row is read outside acceptance, and a rejected packet that had advanced
/// this would be a rejected packet that had changed what the system
/// believes the viewer wants. Establishing the placement before something
/// depends on it is cheaper than discovering it afterwards.
#[test]
fn a_refused_packet_cannot_advance_the_ask_and_strand_a_successor() {
    let request = request();
    let asked_for = selection_at(QualitySelection::Manual { height: 720 });
    let never_asked = selection_at(QualitySelection::Manual { height: 1080 });

    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        i64::MAX,
        Some(asked_for.desired().digest()),
    ));
    let prepared_session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id: staged_incarnation_id.clone(),
        deadline_ms: i64::MAX,
        session_id: prepared_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };

    let announced = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor)).asking(&asked_for),
        )
        .expect("announced");
    let ControlAction::Prepare {
        action_id,
        media_origin_ms: offered_origin_ms,
        ..
    } = &announced.2
    else {
        panic!("an occupied preparation slot announces its successor");
    };
    let action_id = action_id.clone();

    // A replay of that same sequence, claiming a different ask.
    let replay = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor))
                .asking(&never_asked),
        )
        .expect("a replay is answered, not errored");
    assert_eq!(replay.0, ControlDisposition::Replay);

    // And a rate-limited packet, also claiming it.
    assert!(matches!(
        state.accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor))
                .asking(&never_asked),
        ),
        Err(ControlStateError::RateLimited(_))
    ));

    // Neither refused packet moved the ask.
    assert_eq!(
        state.desired_digest_for_test(),
        Some(asked_for.desired().digest().as_str()),
        "a replayed or rate-limited packet is not the viewer changing their mind"
    );

    // And the successor they are waiting for still commits.
    state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor))
                .asking(&asked_for)
                .acknowledging(ActionAcknowledgement {
                    action_id,
                    state: AcknowledgementState::Committed,
                    buffered_through_ms: None,
                    // Echo the offer this test announced; a commit naming a
                    // different origin is refused.
                    committed_media_origin_ms: Some(*offered_origin_ms),
                    first_frame_unix_ms: None,
                }),
        )
        .expect("accepted");
    assert_eq!(
        state.preparation_directive(),
        Some(PreparationDirective::Commit {
            staged_incarnation_id
        }),
        "a duplicated or throttled packet must not be able to strand a successor"
    );
}

/// A slot staged before any ask was recorded is not second-guessed.
///
/// Several paths stage without an observed selection — the plain
/// `stage_preparation`, and any staging that precedes the first control
/// exchange. An absent record is not evidence that the ask changed, and
/// refusing on it would strand successors with nothing wrong with them, so
/// the comparison is skipped rather than failed.
#[test]
fn a_successor_staged_without_a_recorded_ask_still_commits() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        i64::MAX,
        None,
    ));
    let prepared_session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id: staged_incarnation_id.clone(),
        deadline_ms: i64::MAX,
        session_id: prepared_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };
    let announced = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor)).asking(
                &selection_at(QualitySelection::Auto {
                    height: None,
                    candidate_id: None,
                }),
            ),
        )
        .expect("announced");
    let ControlAction::Prepare {
        action_id,
        media_origin_ms: offered_origin_ms,
        ..
    } = &announced.2
    else {
        panic!("an occupied preparation slot announces its successor");
    };
    state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor))
                // A different ask, and still no record of what the slot was
                // staged for.
                .asking(&selection_at(QualitySelection::Manual { height: 480 }))
                .acknowledging(ActionAcknowledgement {
                    action_id: action_id.clone(),
                    state: AcknowledgementState::Committed,
                    buffered_through_ms: None,
                    // Echo the offer this test announced; a commit naming a
                    // different origin is refused.
                    committed_media_origin_ms: Some(*offered_origin_ms),
                    first_frame_unix_ms: None,
                }),
        )
        .expect("accepted");
    assert_eq!(
        state.preparation_directive(),
        Some(PreparationDirective::Commit {
            staged_incarnation_id
        }),
        "an absent record is no evidence, and must not strand a successor"
    );
}

fn prepared_selection() -> EffectiveSelection {
    EffectiveSelection {
        candidate_id: None,
        quality_auto: false,
        height: 1080,
        audio_track: None,
        subtitle_burn: None,
        audio_offset_ms: 0,
        codec: "server_selected".to_owned(),
        dynamic_range: Some("sdr".to_owned()),
    }
}

#[test]
fn the_preparation_metric_publishes_its_own_action_slot() {
    assert!(prometheus()
        .contains("plurx_playback_control_actions_total{action=\"prepare\",platform=\"apple\"}"));
}

/// The ask travels on this request twice, and the two copies must agree.
///
/// This is the one property the optional envelope needs in order to be
/// worth having. `selection` and `intent.selection` are the same statement
/// in two shapes, so a request carrying both is either redundant or wrong,
/// and there is no third possibility to fall back on. Picking `selection`
/// would make the envelope decorative; picking the envelope would let a
/// client steer the recipe through a field this server's admission path
/// does not read yet. A 400 that names the field is the only answer a
/// client can act on, and the only one that cannot be wrong quietly.
///
/// The agreement is asserted through `desired()` rather than field by
/// field, because that is the normalization the durable ask is built from:
/// two spellings that normalize the same *are* the same ask.
#[test]
fn an_envelope_that_contradicts_the_selection_beside_it_is_refused() {
    use plurx_core::playback::{DesiredQuality, MediaIntentEnvelope};
    let target = 60_000;

    let mut agreeing = request();
    agreeing.intent = Some(MediaIntentEnvelope {
        lifetime_id: "lifetime-a".to_owned(),
        recipe_revision: 1,
        destination_revision: 1,
        transport_revision: 1,
        selection: agreeing.selection.desired(),
    });
    assert!(
        agreeing.validate(None, target).is_ok(),
        "an envelope that says what the selection says is accepted"
    );

    let mut contradicting = request();
    let mut elsewhere = contradicting.selection.desired();
    elsewhere.quality = DesiredQuality::Manual { height: 480 };
    contradicting.intent = Some(MediaIntentEnvelope {
        lifetime_id: "lifetime-a".to_owned(),
        recipe_revision: 1,
        destination_revision: 1,
        transport_revision: 1,
        selection: elsewhere,
    });
    assert_eq!(
        contradicting.validate(None, target),
        Err("intent.selection"),
        "and one that says something else is refused, by name"
    );

    // An envelope that cannot mean anything is refused before its contents
    // are compared at all — otherwise a zero revision would be reported as
    // a selection disagreement, which is a different bug to chase.
    let mut malformed = request();
    malformed.intent = Some(MediaIntentEnvelope {
        lifetime_id: String::new(),
        recipe_revision: 1,
        destination_revision: 1,
        transport_revision: 1,
        selection: malformed.selection.desired(),
    });
    assert_eq!(malformed.validate(None, target), Err("intent"));

    // And the field stays optional: every deployed client sends none.
    let mut absent = request();
    absent.intent = None;
    assert!(absent.validate(None, target).is_ok());
}

#[test]
fn the_action_vocabulary_is_bounded_but_not_policed() {
    let target = 60_000;
    let mut unknown_names = request();
    // A client newer than this server names actions it has never heard of.
    // Refusing that would be a 400, which the reporter reads as terminal.
    unknown_names.supported_actions = Some(vec![
        "hold".to_owned(),
        "an_action_from_next_year".to_owned(),
    ]);
    assert!(unknown_names.validate(None, target).is_ok());
    assert!(unknown_names.accepts_hold());

    let mut too_many = request();
    too_many.supported_actions = Some(vec!["hold".to_owned(); MAX_SUPPORTED_ACTIONS + 1]);
    assert_eq!(too_many.validate(None, target), Err("supported_actions"),);

    let mut empty_name = request();
    empty_name.supported_actions = Some(vec![String::new()]);
    assert_eq!(empty_name.validate(None, target), Err("supported_actions"));

    let mut long_name = request();
    long_name.supported_actions = Some(vec!["x".repeat(MAX_ACTION_NAME_LEN + 1)]);
    assert_eq!(long_name.validate(None, target), Err("supported_actions"));

    // Absent is the passive default, and stays valid.
    let mut absent = request();
    absent.supported_actions = None;
    assert!(absent.validate(None, target).is_ok());
    assert!(!absent.accepts_hold());
}

#[test]
fn original_quality_is_explicit_even_without_a_height() {
    let original: QualitySelection = serde_json::from_value(serde_json::json!({
        "mode": "original"
    }))
    .expect("explicit Original selection");
    assert_eq!(original, QualitySelection::Original);
    assert_eq!(
        serde_json::to_value(original).expect("serialize Original"),
        serde_json::json!({"mode": "original"})
    );
}

#[test]
fn strict_request_validation_rejects_unknown_and_non_finite_values() {
    let mut json = serde_json::to_value(request()).expect("request json");
    json.as_object_mut()
        .expect("object")
        .insert("surprise".to_owned(), serde_json::json!(true));
    assert!(serde_json::from_value::<ControlRequestV1>(json).is_err());

    let mut oversized_sequence = request();
    oversized_sequence.sequence = i64::MAX as u64 + 1;
    assert_eq!(
        oversized_sequence.validate(Some(60_000), 2_000),
        Err("sequence")
    );

    let mut invalid = request();
    invalid.playback_rate = f64::NAN;
    assert_eq!(invalid.validate(Some(60_000), 2_000), Err("playback_rate"));

    let mut no_capabilities = request();
    no_capabilities.capabilities = None;
    assert_eq!(
        no_capabilities.validate(Some(60_000), 2_000),
        Err("capabilities"),
        "the first sequence snapshots bounded client capabilities"
    );

    let mut behind_playhead = request();
    behind_playhead.buffered_through_ms = behind_playhead.position_ms - 1;
    assert_eq!(
        behind_playhead.validate(Some(60_000), 2_000),
        Err("buffered_through_ms")
    );

    let mut disconnected = request();
    disconnected.buffered_from_ms = Some(disconnected.position_ms + 2_001);
    assert_eq!(
        disconnected.validate(Some(60_000), 2_000),
        Err("buffered_from_ms")
    );

    let mut code_without_detail = request();
    code_without_detail.observation = Some(ClientObservation {
        dropped_frames: None,
        decoder_state: Some(DecoderState::Failed),
        error_code: Some(ClientErrorCode::Decoder),
        error_detail: None,
    });
    assert_eq!(code_without_detail.validate(Some(60_000), 2_000), Ok(()));
}

#[test]
fn bootstrap_uses_the_owning_registry_timeout_and_preserves_it_on_takeover() {
    let session = uuid::Uuid::new_v4().to_string();
    let generation = uuid::Uuid::new_v4().to_string();
    let rolling = ControlBootstrap::new(&session, &generation, 1, ROLLING_LEASE_TIMEOUT_MS)
        .expect("rolling bootstrap");
    assert_eq!(rolling.lease_timeout_ms, ROLLING_LEASE_TIMEOUT_MS);
    let successor = rolling
        .refreshed(&session, &generation, 2)
        .expect("takeover bootstrap");
    assert_eq!(successor.control_epoch, 2);
    assert_eq!(successor.lease_timeout_ms, ROLLING_LEASE_TIMEOUT_MS);

    let vod = ControlBootstrap::new(&session, &generation, 1, VOD_LEASE_TIMEOUT_MS)
        .expect("VOD bootstrap");
    assert_eq!(vod.lease_timeout_ms, VOD_LEASE_TIMEOUT_MS);
    assert!(ControlBootstrap::new(&session, &generation, 1, 42_000).is_none());
    assert!(ControlBootstrap::new(&session, &generation, 0, VOD_LEASE_TIMEOUT_MS).is_none());
}

#[test]
fn equal_sequence_replays_but_lower_sequence_and_other_client_are_stale() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    assert_eq!(
        state.accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None),
        ),
        Ok((
            ControlDisposition::Accepted,
            1,
            ControlAction::None,
            ClientPlatform::Web,
            false,
        ))
    );
    assert_eq!(
        state.accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(None, None),
        ),
        Ok((
            ControlDisposition::Replay,
            1,
            ControlAction::None,
            ClientPlatform::Web,
            false,
        ))
    );
    assert_eq!(
        state.accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            0,
            ControlAcceptance::new(None, None),
        ),
        Err(ControlStateError::StaleSequence)
    );
    assert_eq!(
        state.accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &uuid::Uuid::new_v4().to_string(),
            2,
            ControlAcceptance::new(Some(ClientPlatform::Apple), None),
        ),
        Err(ControlStateError::StaleClient)
    );
}

#[test]
fn equal_sequence_requires_the_exact_request_fingerprint() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None).fingerprinted(&"a".repeat(64)),
        )
        .expect("first request accepted");
    assert_eq!(
        state.accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(None, None).fingerprinted(&"b".repeat(64)),
        ),
        Err(ControlStateError::StaleSequence),
        "a sequence is an idempotency key for exact bytes, not a reusable slot"
    );
    assert_eq!(
        state
            .accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                ControlAcceptance::new(None, None).fingerprinted(&"a".repeat(64)),
            )
            .expect("exact request replay")
            .0,
        ControlDisposition::Replay
    );
}

#[test]
fn a_prepared_successor_gets_one_action_identity_and_replays_it_exactly() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        i64::MAX,
        None,
    ));
    let prepared_session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id,
        deadline_ms: i64::MAX,
        session_id: prepared_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };

    let accepted = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor)),
        )
        .expect("accepted preparation action");
    let ControlAction::Prepare { action_id, .. } = &accepted.2 else {
        panic!("the occupied preparation slot must announce its successor");
    };
    assert!(uuid::Uuid::parse_str(action_id).is_ok());

    let replay = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::unavailable(None),
        )
        .expect("exact replay");
    assert_eq!(replay.0, ControlDisposition::Replay);
    assert_eq!(
        replay.2, accepted.2,
        "a missed row read cannot alter a replay"
    );

    let next = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(None, Some(&successor)),
        )
        .expect("next accepted exchange");
    assert_eq!(
        next.2, accepted.2,
        "a failed authority lookup never called the actor, so recovery on the next accepted sequence must reuse the staging's action id",
    );

    let suppressed = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL * 2,
            &request.generation,
            1,
            &request.client_instance_id,
            3,
            ControlAcceptance::new(None, None),
        )
        .expect("client omits transaction vocabulary");
    assert_eq!(suppressed.2, ControlAction::None);
    assert!(suppressed.4);

    let restored = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL * 3,
            &request.generation,
            1,
            &request.client_instance_id,
            4,
            ControlAcceptance::new(None, Some(&successor)),
        )
        .expect("client declares transaction vocabulary again");
    assert_eq!(
        restored.2, accepted.2,
        "skipping the Store read for an undeclared action must preserve its stable identity",
    );
}

#[test]
fn preparation_acknowledgements_are_fenced_recorded_and_replayed() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        i64::MAX,
        None,
    ));
    let prepared_session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id: staged_incarnation_id.clone(),
        deadline_ms: i64::MAX,
        session_id: prepared_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };
    let first = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Apple), Some(&successor)),
        )
        .expect("Prepare is announced");
    let ControlAction::Prepare { action_id, .. } = &first.2 else {
        panic!("expected Prepare");
    };
    let action_id = action_id.clone();

    let metadata = ActionAcknowledgement {
        action_id: action_id.clone(),
        state: AcknowledgementState::MetadataReady,
        buffered_through_ms: None,
        committed_media_origin_ms: None,
        first_frame_unix_ms: None,
    };
    let progress = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(None, Some(&successor)).acknowledging(metadata.clone()),
        )
        .expect("metadata progress");
    assert_eq!(progress.2, first.2);
    assert_eq!(state.preparation_directive(), None);
    assert_eq!(
        state
            .prepared_action
            .as_ref()
            .and_then(|binding| binding.acknowledgement.as_ref()),
        Some(&metadata),
        "progress is retained on the actor-owned transaction"
    );

    let buffered = ActionAcknowledgement {
        action_id: action_id.clone(),
        state: AcknowledgementState::BufferReady,
        buffered_through_ms: Some(55_000),
        committed_media_origin_ms: None,
        first_frame_unix_ms: None,
    };
    let buffered_response = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL * 2,
            &request.generation,
            1,
            &request.client_instance_id,
            3,
            ControlAcceptance::new(None, Some(&successor)).acknowledging(buffered.clone()),
        )
        .expect("buffer progress");
    assert_eq!(buffered_response.2, first.2);
    assert_eq!(
        state
            .prepared_action
            .as_ref()
            .and_then(|binding| binding.acknowledgement.as_ref()),
        Some(&buffered)
    );

    let stale = ActionAcknowledgement {
        action_id: uuid::Uuid::new_v4().to_string(),
        state: AcknowledgementState::Committed,
        buffered_through_ms: None,
        // Echo the offer this test announced; a commit naming a
        // different origin is refused.
        committed_media_origin_ms: Some(successor.media_origin_ms),
        first_frame_unix_ms: Some(10),
    };
    let ignored = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL * 3,
            &request.generation,
            1,
            &request.client_instance_id,
            4,
            ControlAcceptance::new(None, Some(&successor)).acknowledging(stale),
        )
        .expect("stale acknowledgement is harmless");
    assert_eq!(ignored.2, first.2);
    assert_eq!(state.preparation_directive(), None);

    let committed = ActionAcknowledgement {
        action_id,
        state: AcknowledgementState::Committed,
        buffered_through_ms: None,
        // Echo the offer this test announced; a commit naming a
        // different origin is refused.
        committed_media_origin_ms: Some(successor.media_origin_ms),
        first_frame_unix_ms: Some(11),
    };
    let accepted = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL * 4,
            &request.generation,
            1,
            &request.client_instance_id,
            5,
            ControlAcceptance::new(None, Some(&successor)).acknowledging(committed),
        )
        .expect("commit acknowledgement");
    assert_eq!(accepted.2, ControlAction::None);
    assert_eq!(
        state.preparation_directive(),
        Some(PreparationDirective::Commit {
            staged_incarnation_id: staged_incarnation_id.clone(),
        })
    );
    assert!(
        !state.begin_abort_preparation(&staged_incarnation_id),
        "the accepted Commit reservation wins over deadline cleanup"
    );

    let replay = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL * 4,
            &request.generation,
            1,
            &request.client_instance_id,
            5,
            ControlAcceptance::unavailable(None),
        )
        .expect("exact commit replay");
    assert_eq!(replay.0, ControlDisposition::Replay);
    assert_eq!(replay.2, ControlAction::None);
    assert_eq!(
        state.preparation_directive(),
        Some(PreparationDirective::Commit {
            staged_incarnation_id,
        })
    );
}

#[test]
fn failed_preparation_acknowledgement_moves_the_slot_to_aborting() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        i64::MAX,
        None,
    ));
    let prepared_session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id: staged_incarnation_id.clone(),
        deadline_ms: i64::MAX,
        session_id: prepared_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };
    let first = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Apple), Some(&successor)),
        )
        .expect("Prepare is announced");
    let ControlAction::Prepare { action_id, .. } = &first.2 else {
        panic!("expected Prepare");
    };
    let action_id = action_id.clone();
    state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(None, Some(&successor)).acknowledging(ActionAcknowledgement {
                action_id,
                state: AcknowledgementState::Failed,
                buffered_through_ms: None,
                committed_media_origin_ms: None,
                first_frame_unix_ms: None,
            }),
        )
        .expect("failure acknowledgement");
    assert_eq!(
        state.preparation_directive(),
        Some(PreparationDirective::Abort {
            staged_incarnation_id: staged_incarnation_id.clone(),
            acknowledgement_rejected: false,
        })
    );
    assert_eq!(
        state.preparation,
        PreparationSlot::Aborting {
            staged_incarnation_id,
        }
    );
}

#[test]
fn committed_acknowledgement_cannot_share_an_end_exchange() {
    let mut request = request();
    request.demand = PlaybackDemand::End;
    request.playback_rate = 0.0;
    // Well-formed on purpose. `ControlRequestV1::validate` runs the
    // acknowledgement's own field checks before this rule, so an
    // acknowledgement missing a field would trip on that instead and this
    // test would stop exercising the rule it is named for while still
    // passing.
    request.acknowledgement = Some(ActionAcknowledgement {
        action_id: uuid::Uuid::new_v4().to_string(),
        state: AcknowledgementState::Committed,
        buffered_through_ms: None,
        committed_media_origin_ms: Some(0),
        first_frame_unix_ms: Some(1),
    });
    assert_eq!(request.validate(None, 6_000), Err("acknowledgement.state"));
}

#[test]
fn expired_or_abort_won_commit_acknowledgement_is_rejected_and_replayed() {
    let request = request();
    for abort_wins_first in [false, true] {
        let mut state = ControlState::default();
        let started = Instant::now();
        let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
        let preparation_deadline = 10_000;
        assert!(state.stage_preparation(
            staged_incarnation_id.clone(),
            request.generation.clone(),
            preparation_deadline,
            None,
        ));
        let prepared_session_id = uuid::Uuid::new_v4().to_string();
        let successor = PreparedSuccessorAction {
            enhancement: None,
            control: None,
            staged_incarnation_id: staged_incarnation_id.clone(),
            deadline_ms: preparation_deadline,
            session_id: prepared_session_id.clone(),
            playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
            media_origin_ms: 42_000,
            effective_selection: prepared_selection(),
        };
        let first = state
            .accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                ControlAcceptance::new(Some(ClientPlatform::Apple), Some(&successor))
                    .at_unix_ms(preparation_deadline - 1),
            )
            .expect("Prepare is announced");
        let ControlAction::Prepare {
            action_id,
            media_origin_ms: offered_origin_ms,
            ..
        } = first.2
        else {
            panic!("expected Prepare");
        };
        if abort_wins_first {
            assert!(state.begin_abort_preparation(&staged_incarnation_id));
        }
        let accepted = state
            .accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &request.client_instance_id,
                2,
                ControlAcceptance::unavailable(None)
                    .at_unix_ms(preparation_deadline)
                    .acknowledging(ActionAcknowledgement {
                        action_id,
                        state: AcknowledgementState::Committed,
                        buffered_through_ms: None,
                        // Echo the offer this test announced; a commit naming a
                        // different origin is refused.
                        committed_media_origin_ms: Some(offered_origin_ms),
                        first_frame_unix_ms: Some(preparation_deadline),
                    }),
            )
            .expect("bound terminal acknowledgement does not need another Store read");
        assert_eq!(accepted.2, ControlAction::None);
        assert_eq!(
            state.preparation_directive(),
            Some(PreparationDirective::Abort {
                staged_incarnation_id: staged_incarnation_id.clone(),
                acknowledgement_rejected: true,
            })
        );
        let replay = state
            .accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &request.client_instance_id,
                2,
                ControlAcceptance::unavailable(None),
            )
            .expect("rejected commit replay");
        assert_eq!(replay.0, ControlDisposition::Replay);
        assert_eq!(
            state.preparation_directive(),
            Some(PreparationDirective::Abort {
                staged_incarnation_id,
                acknowledgement_rejected: true,
            })
        );
    }
}

#[test]
fn settled_deadline_abort_still_rejects_its_late_commit_acknowledgement() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    let preparation_deadline = 10_000;
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        preparation_deadline,
        None,
    ));
    let prepared_session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id: staged_incarnation_id.clone(),
        deadline_ms: preparation_deadline,
        session_id: prepared_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };
    let first = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Apple), Some(&successor))
                .at_unix_ms(preparation_deadline - 1),
        )
        .expect("Prepare is announced");
    let ControlAction::Prepare {
        action_id,
        media_origin_ms: offered_origin_ms,
        ..
    } = first.2
    else {
        panic!("expected Prepare");
    };

    assert!(state.begin_abort_preparation(&staged_incarnation_id));
    assert!(state.settle_preparation(&staged_incarnation_id));
    state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(None, None).at_unix_ms(preparation_deadline + 1),
        )
        .expect("an ordinary poll may follow deadline cleanup");
    let accepted = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL * 2,
            &request.generation,
            1,
            &request.client_instance_id,
            3,
            ControlAcceptance::unavailable(None)
                .at_unix_ms(preparation_deadline + 1)
                .acknowledging(ActionAcknowledgement {
                    action_id,
                    state: AcknowledgementState::Committed,
                    buffered_through_ms: None,
                    // Echo the offer this test announced; a commit naming a
                    // different origin is refused.
                    committed_media_origin_ms: Some(offered_origin_ms),
                    first_frame_unix_ms: Some(preparation_deadline + 1),
                }),
        )
        .expect("late bound acknowledgement is rejected without another Store read");
    assert_eq!(accepted.2, ControlAction::None);
    assert_eq!(
        state.preparation_directive(),
        Some(PreparationDirective::Abort {
            staged_incarnation_id,
            acknowledgement_rejected: true,
        })
    );
}

#[test]
fn owner_epoch_rollover_aborts_the_inherited_reservation_and_fences_the_old_owner() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    assert!(state.stage_preparation_for_owner(
        "successor-1".to_owned(),
        request.generation.clone(),
        i64::MAX,
        1,
        None,
    ));
    state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None),
        )
        .expect("epoch one");
    assert!(state.reserve_preparation_commit("successor-1", 1));
    assert_eq!(
        state.accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            2,
            &uuid::Uuid::new_v4().to_string(),
            2,
            ControlAcceptance::new(Some(ClientPlatform::Apple), None),
        ),
        Err(ControlStateError::StaleSequence),
        "an invalid first packet cannot consume the epoch rollover"
    );
    state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            2,
            &uuid::Uuid::new_v4().to_string(),
            1,
            ControlAcceptance::new(Some(ClientPlatform::Apple), None),
        )
        .expect("epoch two");
    assert!(!state.reject_preparation_commit_for_owner("successor-1", 1));
    assert!(!state.settle_preparation_for_owner("successor-1", 1));
    assert_eq!(
        state.preparation_directive(),
        Some(PreparationDirective::Abort {
            staged_incarnation_id: "successor-1".to_owned(),
            acknowledgement_rejected: false,
        })
    );
    assert!(state.begin_abort_preparation_for_owner("successor-1", 2));
    assert!(state.settle_preparation_for_owner("successor-1", 2));
    assert!(state.stage_preparation_for_owner(
        "successor-2".to_owned(),
        request.generation,
        i64::MAX,
        2,
        None,
    ));
}

#[test]
fn vocabulary_is_frozen_for_the_sequence_that_was_accepted() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        i64::MAX,
        None,
    ));
    let session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id,
        deadline_ms: i64::MAX,
        session_id: session_id.clone(),
        playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };

    let unsupported = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None),
        )
        .expect("unsupported request accepted");
    assert_eq!(unsupported.2, ControlAction::None);
    assert!(
        unsupported.4,
        "the transaction was suppressed by vocabulary"
    );

    let changed_retry = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(None, Some(&successor)),
        )
        .expect("same sequence replays");
    assert_eq!(changed_retry.0, ControlDisposition::Replay);
    assert_eq!(changed_retry.2, ControlAction::None);
    assert!(changed_retry.4);
}

#[test]
fn a_changed_payload_cannot_consume_the_next_sequence() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        i64::MAX,
        None,
    ));
    let session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id,
        deadline_ms: i64::MAX,
        session_id: session_id.clone(),
        playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };
    let accepted = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor)),
        )
        .expect("first action");

    let mut corrupted = successor.clone();
    corrupted.media_origin_ms += 1;
    assert_eq!(
        state.accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(None, Some(&corrupted)),
        ),
        Err(ControlStateError::Unavailable),
    );
    let recovered = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(None, Some(&successor)),
        )
        .expect("the rejected payload did not consume sequence two");
    assert_eq!(recovered.0, ControlDisposition::Accepted);
    assert_eq!(recovered.1, 2);
    assert_eq!(recovered.2, accepted.2);
}

#[test]
fn owner_epoch_rollover_aborts_instead_of_reannouncing_old_staging() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        i64::MAX,
        None,
    ));
    let session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id,
        deadline_ms: i64::MAX,
        session_id: session_id.clone(),
        playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };
    let first = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor)),
        )
        .expect("epoch-one action");
    let next_client = uuid::Uuid::new_v4().to_string();
    let rolled = state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            2,
            &next_client,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Apple), Some(&successor)),
        )
        .expect("epoch-two action");
    assert!(matches!(first.2, ControlAction::Prepare { .. }));
    assert_eq!(rolled.2, ControlAction::None);
    assert_eq!(
        state.preparation_directive(),
        Some(PreparationDirective::Abort {
            staged_incarnation_id: successor.staged_incarnation_id,
            acknowledgement_rejected: false,
        })
    );
}

#[test]
fn an_expired_preparation_cannot_cross_the_acceptance_boundary() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    assert!(state.stage_preparation(
        staged_incarnation_id.clone(),
        request.generation.clone(),
        10_000,
        None,
    ));
    let session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id,
        deadline_ms: 10_000,
        session_id: session_id.clone(),
        playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };

    let outcome = state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor)).at_unix_ms(10_000),
        )
        .expect("sequence itself is valid");
    assert_eq!(outcome.2, ControlAction::None);
    assert!(
        !outcome.4,
        "expiry is authority loss, not vocabulary suppression"
    );
}

#[test]
fn owner_epoch_rollover_resets_client_sequence_and_fences_old_owner() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None),
        )
        .expect("epoch one");
    state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            8,
            ControlAcceptance::new(None, None),
        )
        .expect("epoch one advance");
    let successor_client = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        state.accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            2,
            &successor_client,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Apple), None),
        ),
        Ok((
            ControlDisposition::Accepted,
            1,
            ControlAction::None,
            ClientPlatform::Apple,
            false,
        ))
    );
    assert_eq!(
        state.accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            9,
            ControlAcceptance::new(None, None),
        ),
        Err(ControlStateError::OwnerChanged)
    );
}

#[test]
fn first_sequence_is_one_and_rate_limit_does_not_advance_state() {
    let request = request();
    let mut state = ControlState::default();
    let started = Instant::now();
    assert_eq!(
        state.accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(Some(ClientPlatform::Web), None),
        ),
        Err(ControlStateError::StaleSequence)
    );
    state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None),
        )
        .expect("first sequence");
    assert!(matches!(
        state.accept_at(
            started + Duration::from_millis(10),
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(None, None),
        ),
        Err(ControlStateError::RateLimited(_))
    ));
    assert_eq!(
        state.accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(None, None),
        ),
        Ok((
            ControlDisposition::Accepted,
            2,
            ControlAction::None,
            ClientPlatform::Web,
            false,
        ))
    );
}

#[test]
fn first_owner_sequence_requires_platform_and_platform_cannot_change_in_place() {
    let request = request();
    let started = Instant::now();
    let mut state = ControlState::default();
    assert_eq!(
        state.accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(None, None),
        ),
        Err(ControlStateError::StaleClient)
    );
    state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None),
        )
        .expect("capability snapshot");
    assert_eq!(
        state.accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(Some(ClientPlatform::Apple), None),
        ),
        Err(ControlStateError::StaleClient)
    );
}

/// A valid relayed answer and the request it answers.
fn relay_pair() -> (ControlResponseV1, ControlRelayRequest) {
    let request = relay_request();
    let response = ControlResponseV1 {
        protocol: PROTOCOL_V1.to_owned(),
        generation: request.generation.clone(),
        control_epoch: 1,
        accepted_sequence: 1,
        server_time_unix_ms: 1_000_000,
        lease: PlaybackLeaseView {
            state: "active".to_owned(),
            renew_after_ms: NEXT_EXCHANGE_MS,
            expires_at_unix_ms: 1_060_000,
        },
        delivery: DeliveryView {
            presentation: "vod".to_owned(),
            producer_state: "complete".to_owned(),
            produced_through_ms: Some(7_200_000),
            fetched_through_ms: 25_000,
            delivered_bps: None,
            delivered_idle_ms: None,
            recent_producer_speed: None,
            client_runway_ms: 15_000,
            admitted: Some(true),
            producer_decision: None,
            hold_reason: None,
            subtitle_readiness: None,
            subtitle_revision: None,
            preparation: None,
            owner_node_hash: "n-0123456789abcdef".to_owned(),
            owner_epoch: 1,
        },
        effective_selection: EffectiveSelection {
            candidate_id: None,
            quality_auto: true,
            height: 1080,
            audio_track: Some(0),
            subtitle_burn: None,
            audio_offset_ms: 0,
            codec: "source".to_owned(),
            dynamic_range: Some("sdr".to_owned()),
        },
        action: ControlAction::None,
    };
    (response, request)
}

fn relay_request() -> ControlRelayRequest {
    let control = request();
    ControlRelayRequest {
        session_id: uuid::Uuid::new_v4().to_string(),
        generation: control.generation.clone(),
        expected_owner_node_id: "node-a".to_owned(),
        expected_owner_epoch: 1,
        deadline_unix_ms: crate::media_sessions::unix_ms().saturating_add(4_000),
        control,
    }
}

fn owned_control(request: &ControlRequestV1) -> OwnedLocalControlRequest {
    OwnedLocalControlRequest {
        generation: request.generation.clone(),
        owner_epoch: request.control_epoch,
        client_instance_id: request.client_instance_id.clone(),
        sequence: request.sequence,
        snapshot: PlaybackDemandSnapshot::from(request),
        prepared_successor: PreparedSuccessorObservation::NotRequested,
    }
}

/// M6's gate: the actor says when a selection moved, because it is the
/// only place both selections are in hand for free.
///
/// The expensive half — two store reads to resolve a candidate — is what
/// this exists to skip, on an exchange that runs about once a second per
/// client under an absolute deadline.
/// A seek is not a change of ask, however hard it looks like one.
///
/// The last of §1's eleven races, and the one whose absence was easiest to
/// miss: every other case moves *something* about the selection, so a
/// scheduler keying on the wrong field would be caught by one of them. A
/// seek moves nothing about it. The playhead jumps, the buffer empties and
/// refills, the render state goes to seeking and back — and the viewer has
/// not asked for a different recipe, so work in flight for the recipe they
/// *did* ask for has to survive it.
///
/// Same-quality on purpose. A seek that also changed the quality would
/// pass with the two conflated, which is exactly the bug: the rule is that
/// a destination change is not a recipe change, and only a seek that
/// leaves the recipe alone can say whether that holds.
///
/// Asserted on the recorded ask and not only on `changed`, because those
/// are different claims. `changed` is what the scheduler reads this
/// exchange; the digest is what survives to be compared against the next
/// one, and a seek that quietly advanced it would refuse a successor built
/// moments earlier for a recipe nobody has left.
#[test]
fn a_same_quality_seek_does_not_move_the_ask() {
    let request = request();
    let asked_for = selection_at(QualitySelection::Manual { height: 720 });
    let mut state = ControlState::default();
    let started = Instant::now();

    state
        .accept_at(
            started,
            &request.generation,
            1,
            &request.client_instance_id,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None).asking(&asked_for),
        )
        .expect("the first exchange is accepted");
    let settled = state
        .desired_digest_for_test()
        .map(str::to_owned)
        .expect("the ask the viewer arrived with");

    // The seek: a later exchange carrying the very same selection.
    state
        .accept_at(
            started + MIN_CONTROL_INTERVAL,
            &request.generation,
            1,
            &request.client_instance_id,
            2,
            ControlAcceptance::new(Some(ClientPlatform::Web), None).asking(&asked_for),
        )
        .expect("the seek exchange is accepted");

    assert_eq!(
        state.desired_digest_for_test(),
        Some(settled.as_str()),
        "seeking is asking for a different position, not a different recipe"
    );

    // And what the scheduler reads agrees: nothing to rebuild, and no
    // durable ask to write for a request the viewer never made.
    let observation = state.observe(&asked_for, None);
    assert!(
        !observation.changed,
        "a seek must not spend the work the gate exists to save"
    );
}

#[test]
fn the_actor_reports_when_a_selection_moved() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let base = request();

    let mut first = base.clone();
    first.sequence = 1;
    let outcome = actor
        .control_at(started + Duration::from_secs(1), owned_control(&first))
        .expect("sequence 1 accepted");
    assert!(
        !outcome.selection.changed,
        "a session that has just been created from an intent has not since \
             departed from it",
    );

    let mut same = base.clone();
    same.sequence = 2;
    // A different playhead, buffer and render state — everything an
    // ordinary exchange carries — with the same selection.
    same.position_ms = base.position_ms + 4_000;
    same.buffered_through_ms = base.buffered_through_ms + 4_000;
    let outcome = actor
        .control_at(started + Duration::from_secs(2), owned_control(&same))
        .expect("sequence 2 accepted");
    assert!(
        !outcome.selection.changed,
        "an ordinary exchange must not spend the reads the gate exists to save",
    );

    let mut moved = base.clone();
    moved.sequence = 3;
    moved.selection.audio_track = Some(base.selection.audio_track.unwrap_or(0) + 1);
    let outcome = actor
        .control_at(started + Duration::from_secs(3), owned_control(&moved))
        .expect("sequence 3 accepted");
    assert!(outcome.selection.changed, "the viewer picked another track");

    // And it is a comparison against the *previous* selection, not a
    // latch: holding the new selection is not a fresh change.
    let mut held = moved.clone();
    held.sequence = 4;
    let outcome = actor
        .control_at(started + Duration::from_secs(4), owned_control(&held))
        .expect("sequence 4 accepted");
    assert!(
        !outcome.selection.changed,
        "the selection is now the previous one",
    );
}

/// The comparison is against what the client last asked for, never against
/// what it is being served — and this is the case that decides it.
///
/// An explicit rung snaps onto the ladder, so a client asking 1079 is
/// delivered 1080 and keeps asking 1079. A gate that compared the ask to
/// the delivered height would read *changed* on every exchange for the
/// rest of that session, spend two store reads each time, and produce a
/// candidate identical to what is already playing.
#[test]
fn a_snapped_ask_is_not_a_change_every_second() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let mut base = request();
    base.selection.quality = QualitySelection::Manual {
        height: crate::transcode::snap_height(1079) - 1,
    };
    assert_ne!(
        crate::transcode::snap_height(1079),
        1079,
        "the fixture is only meaningful if 1079 actually snaps",
    );

    for (sequence, seconds) in [(1, 1), (2, 2), (3, 3)] {
        let mut exchange = base.clone();
        exchange.sequence = sequence;
        let outcome = actor
            .control_at(
                started + Duration::from_secs(seconds),
                owned_control(&exchange),
            )
            .expect("accepted");
        assert!(
            !outcome.selection.changed,
            "sequence {sequence}: an unchanged ask is unchanged however it \
                 was served",
        );
    }
}

/// A replay is the same exchange arriving twice. It changed the selection
/// the first time or not at all, and answering `true` again would spend
/// the reads twice for one viewer action.
#[test]
fn a_replay_is_not_a_second_change() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let base = request();
    let mut first = base.clone();
    first.sequence = 1;
    actor
        .control_at(started + Duration::from_secs(1), owned_control(&first))
        .expect("sequence 1 accepted");

    let mut moved = base.clone();
    moved.sequence = 2;
    moved.selection.audio_offset_ms = 250;
    let accepted = actor
        .control_at(started + Duration::from_secs(2), owned_control(&moved))
        .expect("sequence 2 accepted");
    assert!(accepted.selection.changed);

    let replayed = actor
        .control_at(started + Duration::from_secs(3), owned_control(&moved))
        .expect("sequence 2 replayed");
    assert_eq!(replayed.disposition, ControlDisposition::Replay);
    assert!(
        !replayed.selection.changed,
        "one viewer action, one resolution",
    );
    assert!(
        replayed.selection.capabilities.is_none(),
        "and a replay reports no observation at all, not a stale one",
    );
}

/// The capability document travels with the observation, and it is the
/// **retained** one.
///
/// This is the property that decides whether the shadow measures anything
/// at all. Both shipped clients strip `capabilities` on every exchange
/// after the first when the document has not changed — exactly as the wire
/// permits — and the gate never fires on the first. So a consumer reading
/// this exchange's document would see `None` every single time it was
/// asked, and the metric would read zero forever while looking healthy.
#[test]
fn the_observation_carries_the_retained_capability_document() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let base = request();

    let mut first = base.clone();
    first.sequence = 1;
    first.capabilities = first.capabilities.map(|mut caps| {
        caps.dual_player_preparation = true;
        caps
    });
    actor
        .control_at(started + Duration::from_secs(1), owned_control(&first))
        .expect("sequence 1 accepted");

    // The exchange that actually changes something is a later one, and it
    // carries no capabilities — which is what the clients send.
    let mut moved = base.clone();
    moved.sequence = 2;
    moved.capabilities = None;
    moved.selection.audio_track = Some(base.selection.audio_track.unwrap_or(0) + 1);
    let outcome = actor
        .control_at(started + Duration::from_secs(2), owned_control(&moved))
        .expect("sequence 2 accepted");
    assert!(outcome.selection.changed);
    assert!(
        outcome
            .selection
            .capabilities
            .expect("the session was told once")
            .dual_player_preparation,
        "the retained document, not this exchange's absent one",
    );
}

/// The wire lets a client send capabilities once. Anything that reads
/// the live snapshot therefore sees `None` from sequence 2 onward — and
/// for `dual_player_preparation` that means refusing every transition a
/// capable client ever makes, which is every transition that will
/// actually happen.
#[test]
fn the_capability_document_is_retained_across_exchanges_that_omit_it() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert!(
        actor.retained_capabilities().is_none(),
        "a session that has not been told holds nothing"
    );

    let base = request();
    let mut first = base.clone();
    first.sequence = 1;
    first.capabilities = first.capabilities.map(|mut caps| {
        caps.dual_player_preparation = true;
        caps
    });
    actor
        .control_at(started + Duration::from_secs(1), owned_control(&first))
        .expect("sequence 1 accepted");
    assert!(
        actor
            .retained_capabilities()
            .expect("retained after sequence 1")
            .dual_player_preparation
    );

    // Two exchanges that say nothing about capabilities, which is what
    // the wire contract permits and what real clients do.
    for (sequence, seconds) in [(2, 2), (3, 3)] {
        let mut later = base.clone();
        later.sequence = sequence;
        later.capabilities = None;
        actor
            .control_at(
                started + Duration::from_secs(seconds),
                owned_control(&later),
            )
            .expect("later exchange accepted");
        assert!(
            actor
                .retained_capabilities()
                .expect("still retained")
                .dual_player_preparation,
            "sequence {sequence} omitted capabilities; it did not withdraw them",
        );
    }

    // A client that sends a new document has changed its answer about a
    // device that changed, so the newer one wins.
    let mut revised = base.clone();
    revised.sequence = 4;
    revised.capabilities = revised.capabilities.map(|mut caps| {
        caps.dual_player_preparation = false;
        caps
    });
    actor
        .control_at(started + Duration::from_secs(4), owned_control(&revised))
        .expect("revision accepted");
    assert!(
        !actor
            .retained_capabilities()
            .expect("retained after revision")
            .dual_player_preparation,
        "last write wins over Some",
    );
}

#[test]
fn a_seek_storm_settles_on_one_target_and_supersedes_every_earlier_one() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let base = request();
    let mut asked = Vec::new();
    for step in 0..20u64 {
        let mut seek = base.clone();
        seek.sequence = step + 1;
        seek.render_state = RenderState::Seeking;
        let target = 600_000 + (step as i64 % 5) * 300_000 - (step as i64) * 7_000;
        seek.seek_target_ms = Some(target);
        seek.position_ms = target;
        seek.buffered_from_ms = None;
        seek.buffered_through_ms = target;
        let outcome = actor
            .control_at(
                // Above the server's own 250 ms exchange floor. A storm
                // faster than that is already refused as `RateLimited`, so
                // spacing these tighter would measure the rate limiter
                // rather than the latch -- and is worth knowing on its own:
                // the cadence already bounds how many restarts one storm
                // can ask for.
                started + Duration::from_millis(300 * (step + 1)),
                owned_control(&seek),
            )
            .expect("each seek in the storm is accepted");
        assert_eq!(outcome.disposition, ControlDisposition::Accepted);
        asked.push((seek.sequence, target));
    }
    let settled = actor.settled_target.expect("a storm settles a target");
    let (last_sequence, last_target) = *asked.last().expect("twenty seeks");
    assert_eq!(settled.sequence, last_sequence);
    assert_eq!(settled.anchor_ms, last_target);
    let superseded = asked
        .iter()
        .filter(|(sequence, target)| settled.supersedes(*sequence, *target))
        .count();
    assert_eq!(superseded, 19, "nineteen of twenty seeks are obsolete");
    assert!(!settled.supersedes(last_sequence, last_target));
}

#[test]
fn a_settled_target_never_supersedes_an_exchange_it_has_not_seen() {
    let settled = SettledTarget {
        sequence: 12,
        anchor_ms: 900_000,
    };
    assert!(settled.supersedes(11, 100_000));
    assert!(!settled.supersedes(12, 100_000), "equal is not later");
    assert!(
        !settled.supersedes(13, 100_000),
        "later demand is not stale"
    );
}

#[test]
fn a_seek_that_lands_beside_its_target_is_not_a_new_destination() {
    let settled = SettledTarget {
        sequence: 40,
        anchor_ms: 1_800_000,
    };
    assert!(!settled.supersedes(39, 1_799_640));
    assert!(!settled.supersedes(39, 1_800_000 + SETTLED_TARGET_SLACK_MS));
    assert!(settled.supersedes(39, 1_800_000 + SETTLED_TARGET_SLACK_MS + 1));
}

#[test]
fn the_latch_follows_ordinary_playback_once_the_seek_is_over() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let base = request();
    let mut seek = base.clone();
    seek.sequence = 1;
    seek.render_state = RenderState::Seeking;
    seek.seek_target_ms = Some(1_500_000);
    seek.position_ms = 1_500_000;
    seek.buffered_from_ms = None;
    seek.buffered_through_ms = 1_500_000;
    actor
        .control_at(started + Duration::from_secs(1), owned_control(&seek))
        .expect("seek accepted");
    assert_eq!(
        actor.settled_target.expect("seek settles").anchor_ms,
        1_500_000
    );
    let mut playing = base.clone();
    playing.sequence = 2;
    playing.position_ms = 1_512_000;
    playing.buffered_from_ms = Some(1_511_000);
    playing.buffered_through_ms = 1_540_000;
    actor
        .control_at(started + Duration::from_secs(2), owned_control(&playing))
        .expect("playback accepted");
    let settled = actor.settled_target.expect("playback settles too");
    assert_eq!(settled.sequence, 2);
    assert_eq!(settled.anchor_ms, 1_512_000);
}

fn staged_actor() -> (RollingControlActor, Instant) {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert!(actor.stage_preparation(
        "successor-1".to_owned(),
        "current-1".to_owned(),
        i64::MAX,
        None,
    ));
    (actor, started)
}

/// The slot is only useful if the executor can actually reach it, and the
/// executor reaches it through the actor's bounded mailbox rather than by
/// touching state. These drive a real spawned actor.
#[tokio::test]
async fn a_staged_successor_round_trips_through_the_actor_mailbox() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    assert!(
        handle
            .stage_preparation(
                "successor-1".to_owned(),
                "current-1".to_owned(),
                i64::MAX,
                None
            )
            .await
    );
    assert!(
        !handle.may_commit_preparation("successor-1").await,
        "staging alone is not commit authority"
    );
    assert!(
        !handle.may_commit_preparation("successor-2").await,
        "a stale executor must not be told it may commit"
    );

    let request = request();
    let prepared_session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id: "successor-1".to_owned(),
        deadline_ms: i64::MAX,
        session_id: prepared_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };
    let accepted = handle
        .control(LocalControlRequest {
            session_id: "unused",
            generation: &request.generation,
            owner_node_id: "node-a",
            owner_epoch: request.control_epoch,
            client_instance_id: &request.client_instance_id,
            sequence: request.sequence,
            snapshot: PlaybackDemandSnapshot::from(&request),
            prepared_successor: PreparedSuccessorObservation::Ready(Box::new(successor)),
        })
        .await
        .expect("rolling actor accepts the preparation");
    assert!(matches!(accepted.action, ControlAction::Prepare { .. }));
    let replay = handle
        .control(LocalControlRequest {
            session_id: "unused",
            generation: &request.generation,
            owner_node_id: "node-a",
            owner_epoch: request.control_epoch,
            client_instance_id: &request.client_instance_id,
            sequence: request.sequence,
            snapshot: PlaybackDemandSnapshot::from(&request),
            prepared_successor: PreparedSuccessorObservation::Unavailable,
        })
        .await
        .expect("rolling actor replays the preparation");
    assert_eq!(replay.disposition, ControlDisposition::Replay);
    assert_eq!(replay.action, accepted.action);

    assert_eq!(
        handle
            .control(LocalControlRequest {
                session_id: "unused",
                generation: &request.generation,
                owner_node_id: "node-a",
                owner_epoch: request.control_epoch,
                client_instance_id: &request.client_instance_id,
                sequence: request.sequence,
                snapshot: PlaybackDemandSnapshot::from(&request),
                prepared_successor: PreparedSuccessorObservation::NotRequested,
            })
            .await,
        Err(ControlStateError::Unavailable),
        "a retry cannot remove the vocabulary that accepted the transaction",
    );

    assert!(handle.settle_preparation("successor-1", true).await);
    assert!(
        handle
            .stage_preparation(
                "successor-2".to_owned(),
                "successor-1".to_owned(),
                i64::MAX,
                None
            )
            .await,
        "a committed successor frees the slot for the next preparation"
    );
    handle.abort_actor_for_test();
}

#[tokio::test]
async fn preparation_ownership_transfers_before_a_dropped_actor_reply() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let mut request = request();
    let staged_incarnation_id = "successor-1".to_owned();
    assert!(
        handle
            .stage_preparation(
                staged_incarnation_id.clone(),
                request.generation.clone(),
                i64::MAX,
                None,
            )
            .await
    );
    let prepared_session_id = uuid::Uuid::new_v4().to_string();
    let successor = PreparedSuccessorAction {
        enhancement: None,
        control: None,
        staged_incarnation_id: staged_incarnation_id.clone(),
        deadline_ms: i64::MAX,
        session_id: prepared_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{prepared_session_id}/index.m3u8"),
        media_origin_ms: 42_000,
        effective_selection: prepared_selection(),
    };
    let first = handle
        .control_before(
            LocalControlRequest {
                session_id: "unused",
                generation: &request.generation,
                owner_node_id: "node-a",
                owner_epoch: 1,
                client_instance_id: &request.client_instance_id,
                sequence: 1,
                snapshot: PlaybackDemandSnapshot::from(&request),
                prepared_successor: PreparedSuccessorObservation::Ready(Box::new(successor)),
            },
            i64::MAX,
            None,
            None,
        )
        .await
        .expect("Prepare accepted");
    let ControlAction::Prepare {
        action_id,
        media_origin_ms: offered_origin_ms,
        ..
    } = first.action
    else {
        panic!("expected Prepare");
    };
    tokio::time::sleep(MIN_CONTROL_INTERVAL).await;
    request.sequence = 2;
    request.capabilities = None;
    request.acknowledgement = Some(ActionAcknowledgement {
        action_id,
        state: AcknowledgementState::Committed,
        buffered_through_ms: None,
        // Echo the offer this test announced; a commit naming a
        // different origin is refused.
        committed_media_origin_ms: Some(offered_origin_ms),
        first_frame_unix_ms: Some(crate::media_sessions::unix_ms()),
    });
    let (reply, response) = tokio::sync::oneshot::channel();
    let admission = Arc::new(DropReplyOnPreparationAdmission {
        receiver: std::sync::Mutex::new(Some(response)),
        accepted: AtomicUsize::new(0),
    });
    handle
        .enqueue_command(RollingControlCommand::Control {
            request: Box::new(OwnedLocalControlRequest {
                generation: request.generation.clone(),
                owner_epoch: request.control_epoch,
                client_instance_id: request.client_instance_id.clone(),
                sequence: request.sequence,
                snapshot: PlaybackDemandSnapshot::from(&request),
                prepared_successor: PreparedSuccessorObservation::Unavailable,
            }),
            deadline_unix_ms: i64::MAX,
            terminal_admission: None,
            preparation_admission: Some(admission.clone()),
            reply,
        })
        .await
        .expect("acknowledgement command queued");
    tokio::time::timeout(Duration::from_secs(1), async {
        while admission.accepted.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("durable owner receives the accepted directive");
    assert_eq!(admission.accepted.load(Ordering::Acquire), 1);
    assert!(handle.may_commit_preparation(&staged_incarnation_id).await);
    handle.abort_actor_for_test();
}

/// A commit that lost its CAS reports `committed: false`, and that must
/// free the slot exactly as a completed abort does. The successor still
/// exists durably, so leaving the slot occupied would strand the playback
/// with a preparation nothing can finish.
#[tokio::test]
async fn a_lost_commit_frees_the_slot_and_refuses_a_retry() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    assert!(
        handle
            .stage_preparation(
                "successor-1".to_owned(),
                "current-1".to_owned(),
                i64::MAX,
                None
            )
            .await
    );
    assert!(handle.settle_preparation("successor-1", false).await);
    assert!(
        !handle.may_commit_preparation("successor-1").await,
        "a lost commit cannot be retried into a win"
    );
    assert!(
        handle
            .stage_preparation(
                "successor-2".to_owned(),
                "current-1".to_owned(),
                i64::MAX,
                None
            )
            .await
    );
    handle.abort_actor_for_test();
}

/// The end-to-end shape of "a disconnect does not imply a commit", driven
/// through the mailbox the executor actually uses.
#[tokio::test]
async fn an_ended_playback_refuses_an_unacknowledged_preparation() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    assert!(
        handle
            .stage_preparation(
                "successor-1".to_owned(),
                "current-1".to_owned(),
                i64::MAX,
                None
            )
            .await
    );
    assert!(
        !handle.may_commit_preparation("successor-1").await,
        "staging is not a commit reservation"
    );

    let ended = handle.terminate(RollingTerminalCause::End).await;
    assert!(ended.is_ok());

    assert!(
        !handle.may_commit_preparation("successor-1").await,
        "the executor's gate closes with the playback"
    );
    handle.abort_actor_for_test();
}

/// One preparation per playback is the store's invariant. The actor
/// refuses a second rather than believing in one the store would reject —
/// a slot holding a successor the ledger never accepted is how a commit
/// ends up naming the wrong incarnation.
#[test]
fn the_slot_holds_one_successor_and_refuses_a_competitor() {
    let (mut actor, _) = staged_actor();
    assert!(!actor.stage_preparation(
        "successor-2".to_owned(),
        "current-1".to_owned(),
        i64::MAX,
        None,
    ));
    assert_eq!(
        actor.control.preparation.staged_incarnation_id(),
        Some("successor-1")
    );
}

/// A commit names the exact successor it prepared. A stale executor
/// speaking for a preparation that has since been replaced must not be
/// able to advance the pointer to an incarnation the actor has forgotten.
#[test]
fn only_the_successor_the_slot_holds_may_commit() {
    let (mut actor, _) = staged_actor();
    assert!(!actor.control.may_commit_preparation("successor-1"));
    assert!(!actor.control.reserve_preparation_commit("successor-2", 0));
    assert!(actor.control.reserve_preparation_commit("successor-1", 0));
    assert!(actor.control.may_commit_preparation("successor-1"));
    assert!(!actor.control.may_commit_preparation("successor-2"));
}

/// **Disconnect does not imply commit.** Whatever ends the playback, a
/// successor that was staged and never committed is torn down, and a
/// commit arriving afterwards is refused — otherwise a viewer who closed
/// the tab could publish a generation they never waited for.
#[test]
fn a_terminal_playback_aborts_a_staged_successor_and_refuses_a_late_commit() {
    for cause in [
        RollingTerminalCause::End,
        RollingTerminalCause::LeaseExpired,
    ] {
        let (mut actor, _) = staged_actor();
        assert!(!actor.control.may_commit_preparation("successor-1"));

        let outcome = actor.terminate(cause);
        assert!(matches!(outcome, RollingTerminalOutcome::Won(_)));

        assert!(
            matches!(actor.control.preparation, PreparationSlot::Aborting { .. }),
            "{cause:?} must leave the successor aborting"
        );
        assert!(
            !actor.control.may_commit_preparation("successor-1"),
            "{cause:?} must refuse a late commit"
        );
        assert!(!actor.control.reserve_preparation_commit("successor-1", 0));
    }
}

/// An abort under way cannot become a commit. The transition is one-way
/// because the alternative is a race in which a slow abort and a slow
/// commit both believe they won.
#[test]
fn an_abort_in_flight_never_becomes_a_commit() {
    let (mut actor, _) = staged_actor();
    assert!(
        actor.abort_preparation("successor-1"),
        "first abort moves it"
    );
    assert!(
        !actor.abort_preparation("successor-1"),
        "a retried abort reports no movement rather than a spurious loss"
    );
    assert!(!actor.control.may_commit_preparation("successor-1"));
    assert!(actor.control.preparation.may_abort("successor-1"));
}

/// A stale executor cannot tear down a successor that replaced the one it
/// knew about, and cannot clear a slot it does not hold.
#[test]
fn a_stale_executor_moves_nothing() {
    let (mut actor, _) = staged_actor();
    assert!(!actor.abort_preparation("successor-2"));
    assert!(!actor.settle_preparation("successor-2"));
    assert_eq!(
        actor.control.preparation.staged_incarnation_id(),
        Some("successor-1")
    );
}

/// Settling clears the slot for a committed successor and a completed
/// abort alike: after either, this playback has no staged generation and
/// may prepare again.
#[test]
fn settling_frees_the_slot_for_the_next_preparation() {
    for abort_first in [false, true] {
        let (mut actor, _) = staged_actor();
        if abort_first {
            assert!(actor.abort_preparation("successor-1"));
        }
        assert!(actor.settle_preparation("successor-1"));
        assert_eq!(actor.control.preparation, PreparationSlot::Empty);
        assert!(actor.stage_preparation(
            "successor-2".to_owned(),
            "current-1".to_owned(),
            i64::MAX,
            None,
        ));
    }
}

#[test]
fn rolling_actor_keeps_the_complete_demand_and_replay_never_renews() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let request = request();
    let accepted = actor
        .control_at(started + Duration::from_secs(1), owned_control(&request))
        .expect("first control accepted");
    assert_eq!(accepted.disposition, ControlDisposition::Accepted);
    assert_eq!(accepted.lease.mode, RollingLeaseMode::Explicit);
    assert_eq!(accepted.lease.accepted_demand_sequence, Some(1));
    assert_eq!(accepted.lease.demand_observation_age, Some(Duration::ZERO));
    let accepted_flow_ticket = accepted.flow_ticket;
    let demand = accepted.lease.demand.expect("demand snapshot");
    assert_eq!(demand.position_ms, request.position_ms);
    assert_eq!(demand.buffered_from_ms, request.buffered_from_ms);
    assert_eq!(demand.buffered_through_ms, request.buffered_through_ms);
    assert_eq!(demand.render_state, request.render_state);
    assert_eq!(demand.selection, request.selection);
    assert_eq!(demand.capabilities, request.capabilities);

    let replay = actor
        .control_at(started + Duration::from_secs(11), owned_control(&request))
        .expect("equal sequence is replayed");
    assert_eq!(replay.disposition, ControlDisposition::Replay);
    assert_eq!(replay.lease.idle_for, Duration::from_secs(10));
    assert_eq!(replay.lease.remaining, Duration::from_secs(20));
    assert_eq!(replay.lease.timeout_ms(), ROLLING_EXPLICIT_LEASE_TIMEOUT_MS);
    assert_eq!(replay.lease.last_renewal_kind, "control");
    assert_eq!(replay.lease.accepted_demand_sequence, Some(1));
    assert_eq!(
        replay.lease.demand_observation_age,
        Some(Duration::from_secs(10))
    );
    assert_eq!(replay.flow_ticket, accepted_flow_ticket);
    for _ in 0..100 {
        let replay = actor
            .control_at(started + Duration::from_secs(11), owned_control(&request))
            .expect("repeated equal sequence is replayed");
        assert_eq!(replay.flow_ticket, accepted_flow_ticket);
    }
    assert_eq!(
        actor.flow_sync.requested.load(Ordering::Acquire),
        accepted_flow_ticket,
        "replay waits on accepted convergence without scheduling new flow work"
    );

    let mut stale = request;
    stale.sequence = 0;
    assert_eq!(
        actor.control_at(started + Duration::from_secs(12), owned_control(&stale)),
        Err(ControlStateError::StaleSequence)
    );
    assert_eq!(
        actor
            .snapshot_at(started + Duration::from_secs(12))
            .idle_for,
        Duration::from_secs(11)
    );
}

fn publication(
    producer_attempt: u64,
    playlist_ready: bool,
    published_segment: i64,
    published_end_ms: i64,
    resolved_fetched_segment: Option<i64>,
) -> RollingPublicationObservation {
    RollingPublicationObservation {
        producer_attempt,
        publication_commit: playlist_ready,
        demand_sequence: None,
        produced_segment: Some(published_segment),
        produced_end_ms: Some(published_end_ms),
        playlist_ready,
        published_segment: Some(published_segment),
        published_end_ms: Some(published_end_ms),
        published_first_segment: Some(0),
        published_start_ms: Some(0),
        media_origin_ms: 0,
        next_media_sequence: published_segment + 1,
        resolved_fetched_segment,
        resolved_fetched_end_ms: resolved_fetched_segment.map(|_| published_end_ms),
    }
}

#[test]
fn rolling_delivery_attempt_fences_stale_publication_and_fetch() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let first = actor
        .begin_producer_attempt_at(started)
        .expect("initial producer attempt");
    assert_eq!(first, 1);
    assert!(actor.observe_publication_at(started, publication(first, false, 0, 2_000, None),));

    let second = actor
        .begin_producer_attempt_at(started + Duration::from_secs(1))
        .expect("prepublication replacement");
    assert_eq!(second, 2);
    assert_eq!(actor.delivery.published_segment, None);
    assert!(!actor.observe_publication_at(
        started + Duration::from_secs(2),
        publication(first, true, 8, 18_000, None),
    ));
    assert!(!actor.commit_media_at(
        started + Duration::from_secs(2),
        "segment",
        first,
        Some(8),
        Some(18_000),
    ));
    assert_eq!(
        actor.delivery,
        RollingDeliverySnapshot {
            producer_attempt: second,
            ..RollingDeliverySnapshot::default()
        }
    );

    assert!(actor.observe_publication_at(
        started + Duration::from_secs(2),
        publication(second, true, 1, 4_000, None),
    ));
    assert_eq!(actor.delivery.published_segment, Some(1));
    assert_eq!(actor.delivery.published_end_ms, Some(4_000));
    assert_eq!(actor.delivery.next_media_sequence, 2);
    assert_eq!(
        actor.begin_producer_attempt_at(started + Duration::from_secs(3)),
        Err(ProducerAttemptRejection::PlaylistPublished),
        "client-visible publication returns an exact rejection cause"
    );
}

#[test]
fn rolling_publication_budget_position_projection_freezes_when_not_fresh_or_rendering() {
    let mut demand = PlaybackDemandSnapshot::test_default(ClientPlatform::Web);
    demand.position_ms = 10_000;
    demand.playback_rate = 2.0;
    demand.demand = PlaybackDemand::Active;
    demand.render_state = RenderState::Rendering;
    assert_eq!(
        rolling_estimated_position_ms(&demand, Duration::from_secs(10)),
        30_000
    );
    assert_eq!(
        rolling_estimated_position_ms(&demand, Duration::from_secs(90)),
        70_000,
        "continued HTTP activity cannot extend a stale demand beyond 30 seconds"
    );

    demand.demand = PlaybackDemand::Hold;
    assert_eq!(
        rolling_estimated_position_ms(&demand, Duration::from_secs(10)),
        10_000
    );
    demand.demand = PlaybackDemand::Active;
    demand.render_state = RenderState::Waiting;
    assert_eq!(
        rolling_estimated_position_ms(&demand, Duration::from_secs(10)),
        10_000
    );
}

#[test]
fn rolling_publication_budget_media_renewal_does_not_refresh_demand_age() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let control = request();
    actor
        .control_at(started + Duration::from_secs(1), owned_control(&control))
        .expect("demand accepted");
    assert!(actor.commit_media_at(started + Duration::from_secs(10), "segment", 0, None, None,));
    let snapshot = actor.snapshot_at(started + Duration::from_secs(20));
    assert_eq!(snapshot.idle_for, Duration::from_secs(10));
    assert_eq!(
        snapshot.demand_observation_age,
        Some(Duration::from_secs(19)),
        "media keeps the lease live without inventing a fresh playhead"
    );
}

#[test]
fn rolling_publication_budget_actor_fences_demand_identity_and_protected_start() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");
    let mut first = request();
    first.position_ms = 100_000;
    first.buffered_from_ms = Some(100_000);
    first.buffered_through_ms = 108_000;
    let accepted = actor
        .control_at(started + Duration::from_secs(1), owned_control(&first))
        .expect("first demand");
    assert_eq!(accepted.lease.accepted_demand_sequence, Some(1));

    let candidate = |demand_sequence, published_segment, start_ms| RollingPublicationObservation {
        producer_attempt: attempt,
        publication_commit: true,
        demand_sequence: Some(demand_sequence),
        produced_segment: Some(published_segment),
        produced_end_ms: Some(180_000),
        playlist_ready: true,
        published_segment: Some(published_segment),
        published_end_ms: Some(180_000),
        published_first_segment: Some(start_ms / 10_000),
        published_start_ms: Some(start_ms),
        media_origin_ms: 0,
        next_media_sequence: published_segment.saturating_add(1),
        resolved_fetched_segment: None,
        resolved_fetched_end_ms: None,
    };
    assert!(
        actor.observe_publication_at(started + Duration::from_secs(2), candidate(1, 10, 60_000),)
    );

    let mut second = first;
    second.sequence = 2;
    second.position_ms = 120_000;
    second.buffered_from_ms = Some(120_000);
    second.buffered_through_ms = 128_000;
    actor
        .control_at(started + Duration::from_secs(3), owned_control(&second))
        .expect("newer demand");
    let frozen = actor.delivery.clone();
    assert!(
        !actor.observe_publication_at(started + Duration::from_secs(4), candidate(1, 11, 70_000),)
    );
    assert_eq!(actor.delivery, frozen, "stale demand mutated delivery");
    assert!(
        !actor.observe_publication_at(started + Duration::from_secs(4), candidate(2, 11, 90_000),)
    );
    assert_eq!(
        actor.delivery, frozen,
        "over-pruned prefix mutated delivery"
    );
    assert!(
        actor.observe_publication_at(started + Duration::from_secs(4), candidate(2, 11, 80_000),)
    );

    let mut third = second;
    third.sequence = 3;
    third.position_ms = 200_000;
    third.buffered_from_ms = Some(200_000);
    third.buffered_through_ms = 208_000;
    actor
        .control_at(started + Duration::from_secs(5), owned_control(&third))
        .expect("nonzero-origin demand");
    assert!(actor.observe_publication_at(
        started + Duration::from_secs(5),
        RollingPublicationObservation {
            producer_attempt: attempt,
            publication_commit: true,
            demand_sequence: Some(3),
            produced_segment: Some(12),
            produced_end_ms: Some(180_000),
            playlist_ready: true,
            published_segment: Some(12),
            published_end_ms: Some(180_000),
            published_first_segment: Some(6),
            published_start_ms: Some(60_000),
            media_origin_ms: 100_000,
            next_media_sequence: 13,
            resolved_fetched_segment: None,
            resolved_fetched_end_ms: None,
        }
    ));
}

fn producer_progress(
    producer_attempt: u64,
    out_time_ms: i64,
    speed_milli: i64,
    recent_speed_milli: i64,
    observed_at: Instant,
) -> RollingProducerProgressObservation {
    RollingProducerProgressObservation {
        producer_attempt,
        out_time_ms: Some(out_time_ms),
        speed_milli: Some(speed_milli),
        recent_speed_milli: Some(recent_speed_milli),
        observed_at,
    }
}

fn producer_exit(
    producer_attempt: u64,
    success: bool,
    code: Option<i32>,
    signal: Option<i32>,
    observed_at: Instant,
) -> RollingProducerExitObservation {
    RollingProducerExitObservation {
        producer_attempt,
        success,
        code,
        signal,
        observed_at,
    }
}

#[derive(Clone, Copy, Debug)]
enum TerminalModelEvent {
    ProducerExit,
    LeaseExpired,
    ControlEnd,
    AuthorityFence,
    Publication,
    Replacement,
    Control,
}

#[derive(Debug)]
struct TerminalModel {
    terminal: Option<RollingTerminalCause>,
    terminal_wins: usize,
    producer_attempt: u64,
    playlist_ready: bool,
    producer_exit: bool,
    flow_requests: u64,
}

impl TerminalModel {
    fn new() -> Self {
        Self {
            terminal: None,
            terminal_wins: 0,
            producer_attempt: 1,
            playlist_ready: false,
            producer_exit: false,
            flow_requests: 0,
        }
    }

    fn terminal(&mut self, cause: RollingTerminalCause) -> bool {
        if self.terminal.is_some() {
            return false;
        }
        self.terminal = Some(cause);
        self.terminal_wins += 1;
        self.flow_requests += 1;
        true
    }
}

fn assert_terminal_model_order(order: &[TerminalModelEvent]) {
    let started = Instant::now();
    let retired_fence = Arc::new(AtomicBool::new(false));
    let mut actor = RollingControlActor::new(started, "session-start", Arc::clone(&retired_fence));
    let initial_attempt = actor
        .begin_producer_attempt_at(started)
        .expect("model initial producer");
    assert_eq!(initial_attempt, 1);
    let mut model = TerminalModel::new();
    let control = request();

    for (index, event) in order.iter().copied().enumerate() {
        let step = MIN_CONTROL_INTERVAL + Duration::from_millis(1);
        let now = started + step.saturating_mul((index + 1) as u32);
        match event {
            TerminalModelEvent::ProducerExit => {
                let expected = model.terminal.is_none()
                    && model.producer_attempt == initial_attempt
                    && !model.producer_exit;
                let accepted = actor
                    .observe_producer_exit_at(
                        now,
                        producer_exit(initial_attempt, false, Some(7), None, now),
                    )
                    .accepted();
                assert_eq!(accepted, expected, "order {order:?}");
                model.producer_exit |= expected;
            }
            TerminalModelEvent::LeaseExpired => {
                let expected = model.terminal(RollingTerminalCause::LeaseExpired);
                if expected {
                    actor.last_renewal = now - actor.mode.timeout();
                }
                let claim = actor.claim_expiry_at(now);
                let snapshot = match claim {
                    RollingExpiryClaim::Claimed(snapshot) if expected => snapshot,
                    RollingExpiryClaim::Retired(snapshot) if !expected => snapshot,
                    other => panic!("unexpected expiry result {other:?} for order {order:?}"),
                };
                assert_eq!(snapshot.terminal, model.terminal, "order {order:?}");
                assert_eq!(actor.terminal, model.terminal, "order {order:?}");
            }
            TerminalModelEvent::ControlEnd => {
                let expected = model.terminal(RollingTerminalCause::End);
                let mut end = control.clone();
                end.sequence = actor.control.last_sequence.saturating_add(1);
                end.demand = PlaybackDemand::End;
                end.playback_rate = 0.0;
                end.render_state = RenderState::Ended;
                let outcome = actor.control_at(now, owned_control(&end));
                assert_eq!(outcome.is_ok(), expected, "order {order:?}");
                if let Ok(outcome) = outcome {
                    assert_eq!(outcome.disposition, ControlDisposition::Accepted);
                    assert_eq!(outcome.lease.terminal, Some(RollingTerminalCause::End));
                }
            }
            TerminalModelEvent::AuthorityFence => {
                let expected = model.terminal(RollingTerminalCause::AuthorityFence);
                let outcome = actor.terminate(RollingTerminalCause::AuthorityFence);
                assert_eq!(outcome.won(), expected, "order {order:?}");
                assert_eq!(
                    outcome.cause(),
                    model.terminal.expect("terminal cause"),
                    "late fence must report the retained winner for order {order:?}"
                );
            }
            TerminalModelEvent::Publication => {
                let expected =
                    model.terminal.is_none() && model.producer_attempt == initial_attempt;
                let mut observation = publication(initial_attempt, true, 1, 4_000, None);
                observation.demand_sequence = actor.accepted_demand_sequence;
                let accepted = actor.observe_publication_at(now, observation);
                assert_eq!(accepted, expected, "order {order:?}");
                model.playlist_ready |= expected;
            }
            TerminalModelEvent::Replacement => {
                let expected = model.terminal.is_none() && !model.playlist_ready;
                let admitted = actor.begin_producer_attempt_at(now).is_ok();
                assert_eq!(admitted, expected, "order {order:?}");
                if expected {
                    model.producer_attempt += 1;
                    model.playlist_ready = false;
                    model.producer_exit = false;
                }
            }
            TerminalModelEvent::Control => {
                let expected = model.terminal.is_none();
                let mut active = control.clone();
                active.sequence = actor.control.last_sequence.saturating_add(1);
                let accepted = actor.control_at(now, owned_control(&active)).is_ok();
                assert_eq!(accepted, expected, "order {order:?}");
                if expected {
                    model.flow_requests += 1;
                }
            }
        }

        assert_eq!(actor.terminal, model.terminal, "order {order:?}");
        assert_eq!(actor.retired, model.terminal.is_some(), "order {order:?}");
        assert_eq!(
            actor.expiration_claimed,
            model.terminal == Some(RollingTerminalCause::LeaseExpired),
            "order {order:?}"
        );
        assert_eq!(
            retired_fence.load(Ordering::Acquire),
            model.terminal.is_some(),
            "order {order:?}"
        );
        assert_eq!(
            actor.delivery.producer_attempt, model.producer_attempt,
            "order {order:?}"
        );
        assert_eq!(
            actor.delivery.playlist_ready, model.playlist_ready,
            "order {order:?}"
        );
        assert_eq!(
            actor.delivery.producer_exit.is_some(),
            model.producer_exit,
            "order {order:?}"
        );
        assert_eq!(
            actor.flow_sync.requested.load(Ordering::Acquire),
            model.flow_requests,
            "order {order:?}"
        );
        assert_eq!(model.terminal_wins.min(1), model.terminal_wins);
    }
    assert_eq!(model.terminal_wins, 1, "order {order:?}");
}

fn explore_terminal_model_orders(events: &mut [TerminalModelEvent], from: usize) -> usize {
    if from == events.len() {
        assert_terminal_model_order(events);
        return 1;
    }
    let mut explored = 0;
    for index in from..events.len() {
        events.swap(from, index);
        explored += explore_terminal_model_orders(events, from + 1);
        events.swap(from, index);
    }
    explored
}

#[test]
fn terminal_model_explores_every_event_order_with_one_immutable_winner() {
    let mut events = [
        TerminalModelEvent::ProducerExit,
        TerminalModelEvent::LeaseExpired,
        TerminalModelEvent::ControlEnd,
        TerminalModelEvent::AuthorityFence,
        TerminalModelEvent::Publication,
        TerminalModelEvent::Replacement,
        TerminalModelEvent::Control,
    ];
    assert_eq!(explore_terminal_model_orders(&mut events, 0), 5_040);
}

#[tokio::test]
async fn concurrent_end_and_authority_fence_have_one_actor_winner() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let end = {
        let handle = handle.clone();
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            handle.end().await.expect("end verdict")
        })
    };
    let fence = {
        let handle = handle.clone();
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            handle.authority_fence().await.expect("fence verdict")
        })
    };
    barrier.wait().await;
    let end = end.await.expect("end task");
    let fence = fence.await.expect("fence task");
    assert_eq!(usize::from(end.won()) + usize::from(fence.won()), 1);
    assert_eq!(end.cause(), fence.cause());
    assert!(matches!(
        end.cause(),
        RollingTerminalCause::End | RollingTerminalCause::AuthorityFence
    ));
    assert_eq!(
        handle.snapshot().await.expect("terminal snapshot").terminal,
        Some(end.cause())
    );
}

#[tokio::test]
async fn accepted_control_end_is_terminal_and_exactly_replayable() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let mut end = request();
    end.demand = PlaybackDemand::End;
    end.playback_rate = 0.0;
    end.render_state = RenderState::Ended;
    let local = || LocalControlRequest {
        session_id: "unused",
        generation: &end.generation,
        owner_node_id: "node-a",
        owner_epoch: end.control_epoch,
        client_instance_id: &end.client_instance_id,
        sequence: end.sequence,
        snapshot: PlaybackDemandSnapshot::from(&end),
        prepared_successor: PreparedSuccessorObservation::NotRequested,
    };

    let accepted = handle.control(local()).await.expect("accepted end");
    assert_eq!(accepted.disposition, ControlDisposition::Accepted);
    assert_eq!(accepted.lease.terminal, Some(RollingTerminalCause::End));
    assert_eq!(accepted.lease.last_renewal_kind, "control-end");
    assert_eq!(
        accepted.lease.demand.as_ref().map(|state| state.demand),
        Some(PlaybackDemand::End)
    );
    assert!(accepted.lease.retired);
    assert!(handle.is_retired());

    let replay = handle.control(local()).await.expect("exact end replay");
    assert_eq!(replay.disposition, ControlDisposition::Replay);
    assert_eq!(replay.accepted_sequence, accepted.accepted_sequence);
    assert_eq!(replay.flow_ticket, accepted.flow_ticket);
    assert_eq!(replay.lease.terminal, Some(RollingTerminalCause::End));

    let mut active_same_sequence = end.clone();
    active_same_sequence.demand = PlaybackDemand::Active;
    active_same_sequence.playback_rate = 1.0;
    active_same_sequence.render_state = RenderState::Rendering;
    assert_eq!(
        handle
            .control(LocalControlRequest {
                session_id: "unused",
                generation: &active_same_sequence.generation,
                owner_node_id: "node-a",
                owner_epoch: active_same_sequence.control_epoch,
                client_instance_id: &active_same_sequence.client_instance_id,
                sequence: active_same_sequence.sequence,
                snapshot: PlaybackDemandSnapshot::from(&active_same_sequence),
                prepared_successor: PreparedSuccessorObservation::NotRequested,
            })
            .await,
        Err(ControlStateError::RollingEnded(RollingTerminalCause::End)),
        "a sequence replay cannot substitute a different demand payload"
    );

    let _ = local;
    end.sequence += 1;
    assert_eq!(
        handle
            .control(LocalControlRequest {
                session_id: "unused",
                generation: &end.generation,
                owner_node_id: "node-a",
                owner_epoch: end.control_epoch,
                client_instance_id: &end.client_instance_id,
                sequence: end.sequence,
                snapshot: PlaybackDemandSnapshot::from(&end),
                prepared_successor: PreparedSuccessorObservation::NotRequested,
            })
            .await,
        Err(ControlStateError::RollingEnded(RollingTerminalCause::End)),
        "only the exact accepted terminal sequence may replay"
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_commands_claim_due_expiry_before_end_or_authority_fence() {
    for cause in [
        RollingTerminalCause::End,
        RollingTerminalCause::AuthorityFence,
    ] {
        let handle = RollingControlHandle::spawn_for_test("session-start");
        handle
            .set_renewal_for_test(
                rolling_now() - ROLLING_LEGACY_LEASE_TIMEOUT,
                "deadline-race",
            )
            .await;
        let outcome = match cause {
            RollingTerminalCause::End => handle.end().await,
            RollingTerminalCause::AuthorityFence => handle.authority_fence().await,
            RollingTerminalCause::LeaseExpired
            | RollingTerminalCause::StartupExpired
            | RollingTerminalCause::PauseExpired => {
                unreachable!()
            }
        }
        .expect("terminal verdict");
        assert_eq!(
            outcome,
            RollingTerminalOutcome::AlreadyTerminal(RollingTerminalCause::LeaseExpired)
        );
        let snapshot = handle.snapshot().await.expect("terminal snapshot");
        assert_eq!(snapshot.terminal, Some(RollingTerminalCause::LeaseExpired));
        assert!(snapshot.expiration_claimed);
    }
}

#[tokio::test]
async fn late_terminal_commands_return_the_immutable_winning_cause() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    assert_eq!(
        handle.end().await.expect("end verdict"),
        RollingTerminalOutcome::Won(RollingTerminalCause::End)
    );
    assert_eq!(
        handle.authority_fence().await.expect("late fence verdict"),
        RollingTerminalOutcome::AlreadyTerminal(RollingTerminalCause::End)
    );
}

#[test]
fn rolling_producer_progress_and_exit_are_exact_attempt_terminal_facts() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let predecessor = actor
        .begin_producer_attempt_at(started)
        .expect("predecessor attempt");
    assert!(actor.observe_producer_progress_at(
        started + Duration::from_secs(1),
        producer_progress(
            predecessor,
            1_000,
            900,
            800,
            started + Duration::from_secs(1),
        ),
    ));
    let moving = actor.snapshot_at(started + Duration::from_secs(3)).delivery;
    assert_eq!(moving.producer_out_time_ms, Some(1_000));
    assert_eq!(moving.producer_speed_milli, Some(900));
    assert_eq!(moving.producer_recent_speed_milli, Some(800));
    assert_eq!(moving.producer_progress_idle_ms, 2_000);

    assert!(actor.observe_producer_progress_at(
        started + Duration::from_secs(4),
        producer_progress(predecessor, 900, 700, 600, started + Duration::from_secs(4),),
    ));
    let non_regressing = actor.snapshot_at(started + Duration::from_secs(5)).delivery;
    assert_eq!(non_regressing.producer_out_time_ms, Some(1_000));
    assert_eq!(non_regressing.producer_progress_idle_ms, 4_000);
    assert_eq!(non_regressing.producer_speed_milli, Some(700));

    let successor = actor
        .begin_producer_attempt_at(started + Duration::from_secs(5))
        .expect("successor attempt");
    assert!(!actor.observe_producer_progress_at(
        started + Duration::from_secs(6),
        producer_progress(
            predecessor,
            20_000,
            2_000,
            2_000,
            started + Duration::from_secs(6),
        ),
    ));
    assert!(!actor
        .observe_producer_exit_at(
            started + Duration::from_secs(6),
            producer_exit(
                predecessor,
                false,
                Some(1),
                None,
                started + Duration::from_secs(6),
            ),
        )
        .accepted());
    assert!(actor.observe_producer_progress_at(
        started + Duration::from_secs(6),
        producer_progress(
            successor,
            500,
            1_100,
            1_000,
            started + Duration::from_secs(6),
        ),
    ));
    let exit = producer_exit(
        successor,
        false,
        None,
        Some(9),
        started + Duration::from_secs(7),
    );
    assert!(actor
        .observe_producer_exit_at(started + Duration::from_secs(7), exit.clone())
        .accepted());
    assert!(
        actor
            .observe_producer_exit_at(started + Duration::from_secs(8), exit)
            .accepted(),
        "an identical re-observation is idempotent"
    );
    assert!(!actor
        .observe_producer_exit_at(
            started + Duration::from_secs(8),
            producer_exit(
                successor,
                true,
                Some(0),
                None,
                started + Duration::from_secs(8),
            ),
        )
        .accepted());
    assert!(!actor.observe_producer_progress_at(
        started + Duration::from_secs(8),
        producer_progress(
            successor,
            1_000,
            1_200,
            1_100,
            started + Duration::from_secs(8),
        ),
    ));
    let terminal = actor.snapshot_at(started + Duration::from_secs(9)).delivery;
    assert_eq!(terminal.producer_out_time_ms, Some(500));
    assert_eq!(
        terminal.producer_exit,
        Some(RollingProducerExitSnapshot {
            success: false,
            code: None,
            signal: Some(9),
            observed_idle_ms: 2_000,
        })
    );
    assert!(actor.terminate(RollingTerminalCause::End).won());
    assert!(!actor.observe_producer_progress_at(
        started + Duration::from_secs(10),
        producer_progress(
            successor,
            2_000,
            1_300,
            1_200,
            started + Duration::from_secs(10),
        ),
    ));
    assert!(!actor
        .observe_producer_exit_at(
            started + Duration::from_secs(10),
            producer_exit(
                successor,
                false,
                None,
                Some(9),
                started + Duration::from_secs(10),
            ),
        )
        .accepted());
}

#[tokio::test]
async fn producer_ingress_coalesces_without_mailbox_capacity_or_stale_eviction() {
    let ingress = RollingProducerIngress::new();
    let started = Instant::now();
    ingress.publish_progress(producer_progress(2, 100, 900, 800, started));
    for out_time_ms in 101..=10_000 {
        ingress.publish_progress(producer_progress(2, out_time_ms, 900, 800, started));
    }
    ingress.publish_progress(producer_progress(
        2,
        9_000,
        700,
        600,
        started + Duration::from_secs(1),
    ));
    ingress.publish_progress(producer_progress(1, 99_999, 9_000, 9_000, started));
    ingress.publish_exit(producer_exit(2, false, Some(7), None, started));
    ingress.publish_exit(producer_exit(
        2,
        true,
        Some(0),
        None,
        started + Duration::from_secs(1),
    ));

    let events = ingress.next().await;
    assert_eq!(events.len(), 2, "one progress slot and one exit slot");
    assert!(matches!(
        &events[0],
        RollingProducerEvent::Progress(RollingProducerProgressObservation {
            producer_attempt: 2,
            out_time_ms: Some(10_000),
            speed_milli: Some(700),
            recent_speed_milli: Some(600),
            ..
        })
    ));
    assert!(matches!(
        &events[1],
        RollingProducerEvent::Exit(RollingProducerExitObservation {
            producer_attempt: 2,
            success: false,
            code: Some(7),
            ..
        })
    ));
}

#[test]
fn producer_progress_batch_proves_contiguous_deadlines_and_freezes_the_first_gap() {
    let started = Instant::now();
    let contiguous = RollingProducerIngress::new();
    for (out_time_ms, seconds) in [(100, 0), (200, 9), (300, 18)] {
        contiguous.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                out_time_ms,
                1_000,
                900,
                started + Duration::from_secs(seconds),
            )),
            ProducerIngressSlot::Progress,
            started + Duration::from_secs(seconds),
        );
    }
    let contiguous_blocks = contiguous.drain_blocks();
    let [RollingProducerIngressBlock::Progress(contiguous)] = contiguous_blocks.as_slice() else {
        panic!("one contiguous progress block");
    };
    assert_eq!(
        contiguous
            .first_advancing
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(100)
    );
    assert_eq!(
        contiguous
            .covered_last
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(300)
    );
    assert_eq!(
        contiguous.covered_deadline,
        Some(started + Duration::from_secs(28))
    );
    assert_eq!(contiguous.first_gap, None);

    let gapped = RollingProducerIngress::new();
    for (out_time_ms, seconds) in [(100, 0), (200, 11), (300, 12)] {
        gapped.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                out_time_ms,
                1_000,
                900,
                started + Duration::from_secs(seconds),
            )),
            ProducerIngressSlot::Progress,
            started + Duration::from_secs(seconds),
        );
    }
    let gapped_blocks = gapped.drain_blocks();
    let [RollingProducerIngressBlock::Progress(gapped)] = gapped_blocks.as_slice() else {
        panic!("one gapped progress block");
    };
    assert_eq!(
        gapped
            .covered_last
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(100),
        "late progress cannot extend the proved prefix"
    );
    assert_eq!(
        gapped
            .first_gap
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(200),
        "the first missed link is immutable"
    );
    assert_eq!(
        gapped.latest_telemetry.observation.out_time_ms,
        Some(300),
        "later telemetry remains visible without bridging the gap"
    );

    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    for event in gapped_blocks
        .clone()
        .into_iter()
        .flat_map(RollingProducerIngressBlock::into_events)
    {
        actor.handle_producer_event(event);
    }
    assert_eq!(
        actor
            .snapshot_at(started + Duration::from_secs(13))
            .delivery
            .producer_out_time_ms,
        Some(300),
        "the inactive deadline slice preserves the old latest-progress status"
    );
}

#[test]
fn producer_deadline_starts_exactly_and_retains_scheduler_delayed_coordinate() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    assert_eq!(
        actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Starting,
            instant: started + PRODUCER_STARTUP_BUDGET,
        })
    );
    assert_eq!(
        actor.settle_due_deadlines_at(started + PRODUCER_STARTUP_BUDGET - Duration::from_millis(1)),
        None
    );

    let dispatched_at = started + PRODUCER_STARTUP_BUDGET + Duration::from_secs(7);
    assert_eq!(
        actor.settle_due_deadlines_at(dispatched_at),
        Some(ProducerDeadlineDue {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Starting,
            deadline: started + PRODUCER_STARTUP_BUDGET,
        }),
        "scheduler delay cannot move the verdict coordinate"
    );
    assert_eq!(actor.producer_progress_deadline, None);
    assert_eq!(actor.settle_due_deadlines_at(dispatched_at), None);
    assert_eq!(
        actor.producer_deadline_due,
        Some(ProducerDeadlineDue {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Starting,
            deadline: started + PRODUCER_STARTUP_BUDGET,
        }),
        "repeated cutoff cannot replace the original due coordinate"
    );
}

#[test]
fn producer_progress_rearms_from_fenced_publication_and_late_progress_cannot_rescue() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));

    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            100,
            1_000,
            900,
            started + Duration::from_secs(60),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(5),
    );
    actor.handle_producer_blocks_at(started + Duration::from_secs(6), ingress.drain_blocks());
    assert_eq!(
        actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: started + Duration::from_secs(15),
        }),
        "the fenced publication time, not delayed source telemetry, owns rearm"
    );

    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            200,
            1_100,
            1_000,
            started + Duration::from_secs(7),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(16),
    );
    actor.handle_producer_blocks_at(started + Duration::from_secs(16), ingress.drain_blocks());
    assert_eq!(
        actor.producer_deadline_due,
        Some(ProducerDeadlineDue {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            deadline: started + Duration::from_secs(15),
        })
    );
    assert_eq!(actor.producer_progress_deadline, None);
    assert_eq!(actor.delivery.producer_out_time_ms, Some(200));
}

#[test]
fn exact_boundary_progress_is_applied_before_the_due_cutoff() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 1_000, 900, started)),
        ProducerIngressSlot::Progress,
        started + PRODUCER_STARTUP_BUDGET,
    );

    actor.handle_producer_blocks_at(
        started + PRODUCER_STARTUP_BUDGET + Duration::from_secs(5),
        ingress.drain_blocks(),
    );
    assert_eq!(actor.producer_deadline_due, None);
    assert_eq!(
        actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: started + PRODUCER_STARTUP_BUDGET + PRODUCER_PROGRESS_BUDGET,
        })
    );
}

#[test]
fn producer_deadline_accepts_contiguous_coverage_and_rejects_the_first_gap() {
    let started = Instant::now();
    let mut contiguous_actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(contiguous_actor.begin_producer_attempt_at(started), Ok(1));
    let contiguous = RollingProducerIngress::new();
    for (out_time_ms, seconds) in [(100, 25), (200, 34), (300, 43)] {
        contiguous.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                out_time_ms,
                1_000,
                900,
                started + Duration::from_secs(seconds),
            )),
            ProducerIngressSlot::Progress,
            started + Duration::from_secs(seconds),
        );
    }
    contiguous_actor
        .handle_producer_blocks_at(started + Duration::from_secs(44), contiguous.drain_blocks());
    assert_eq!(
        contiguous_actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: started + Duration::from_secs(53),
        })
    );

    let mut gapped_actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(gapped_actor.begin_producer_attempt_at(started), Ok(1));
    let gapped = RollingProducerIngress::new();
    for (out_time_ms, seconds) in [(100, 25), (200, 36), (300, 37)] {
        gapped.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                out_time_ms,
                1_000,
                900,
                started + Duration::from_secs(seconds),
            )),
            ProducerIngressSlot::Progress,
            started + Duration::from_secs(seconds),
        );
    }
    gapped_actor
        .handle_producer_blocks_at(started + Duration::from_secs(38), gapped.drain_blocks());
    assert_eq!(
        gapped_actor.producer_deadline_due,
        Some(ProducerDeadlineDue {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            deadline: started + Duration::from_secs(35),
        })
    );
    assert_eq!(gapped_actor.producer_progress_deadline, None);
    assert_eq!(gapped_actor.delivery.producer_out_time_ms, Some(300));
}

#[test]
fn non_success_exit_classifies_immediately_and_later_progress_cannot_rearm() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 1_000, 900, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(1),
    );
    ingress.publish_at(
        RollingProducerEvent::Exit(producer_exit(
            1,
            false,
            Some(7),
            None,
            started + Duration::from_secs(2),
        )),
        ProducerIngressSlot::Exit,
        started + Duration::from_secs(2),
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            200,
            1_100,
            1_000,
            started + Duration::from_secs(3),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(3),
    );
    actor.handle_producer_blocks_at(started + Duration::from_secs(3), ingress.drain_blocks());
    assert_eq!(actor.delivery.producer_out_time_ms, Some(100));
    assert_eq!(
        actor.producer_process_exit_due,
        Some(ProducerProcessExitDue {
            producer_attempt: 1,
            published_at: started + Duration::from_secs(2),
            code: Some(7),
            signal: None,
        })
    );
    assert_eq!(actor.producer_progress_deadline, None);
    assert_eq!(actor.producer_deadline_due, None);
}

#[test]
fn non_success_exit_at_exact_deadline_wins_over_generic_timeout() {
    let started = Instant::now();
    let deadline = started + PRODUCER_STARTUP_BUDGET;
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Exit(producer_exit(1, false, Some(9), None, deadline)),
        ProducerIngressSlot::Exit,
        deadline,
    );
    actor.handle_producer_blocks_at(deadline + Duration::from_secs(1), ingress.drain_blocks());

    assert_eq!(actor.producer_deadline_due, None);
    assert_eq!(
        actor.producer_process_exit_due,
        Some(ProducerProcessExitDue {
            producer_attempt: 1,
            published_at: deadline,
            code: Some(9),
            signal: None,
        })
    );
}

#[test]
fn duplicate_progress_updates_telemetry_without_moving_the_deadline() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(5),
    );
    actor.handle_producer_blocks_at(started + Duration::from_secs(6), ingress.drain_blocks());
    let armed = actor.producer_progress_deadline;

    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 1_100, 1_000, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(8),
    );
    actor.handle_producer_blocks_at(started + Duration::from_secs(9), ingress.drain_blocks());
    assert_eq!(actor.producer_progress_deadline, armed);
    assert_eq!(actor.delivery.producer_speed_milli, Some(1_100));
}

#[test]
fn successful_exit_uses_publication_time_and_duplicate_cannot_extend_classification() {
    let started = Instant::now();
    let exit_at = started + PRODUCER_STARTUP_BUDGET - Duration::from_secs(1);
    let classification_at = exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
    let exit = producer_exit(1, true, Some(0), None, exit_at);

    let mut before_classification =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(
        before_classification.begin_producer_attempt_at(started),
        Ok(1)
    );
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Exit(exit.clone()),
        ProducerIngressSlot::Exit,
        exit_at,
    );
    before_classification.handle_producer_blocks_at(
        classification_at - Duration::from_millis(1),
        ingress.drain_blocks(),
    );
    assert_eq!(
        before_classification.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::ClassifyingExit,
            instant: classification_at,
        }),
        "delayed dispatch cannot turn a timely exit into startup timeout"
    );

    assert_eq!(
        before_classification
            .observe_producer_exit_at(exit_at + Duration::from_secs(1), exit.clone(),),
        ProducerExitAcceptance::Duplicate
    );
    assert_eq!(
        before_classification.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::ClassifyingExit,
            instant: classification_at,
        }),
        "an idempotent exit cannot move the original classification deadline"
    );

    let mut after_classification =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(
        after_classification.begin_producer_attempt_at(started),
        Ok(1)
    );
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Exit(exit),
        ProducerIngressSlot::Exit,
        exit_at,
    );
    after_classification.handle_producer_blocks_at(
        classification_at + Duration::from_secs(1),
        ingress.drain_blocks(),
    );
    assert_eq!(
        after_classification.producer_deadline_due,
        Some(ProducerDeadlineDue {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::ClassifyingExit,
            deadline: classification_at,
        })
    );
}

#[test]
fn duplicate_exit_before_one_drain_is_counted_as_coalesced_and_preserves_first() {
    let started = Instant::now();
    let first = producer_exit(1, true, Some(0), None, started);
    let before = ROLLING_PRODUCER_EVENT_COALESCED[1].load(Ordering::Relaxed);
    let ingress = RollingProducerIngress::new();

    ingress.publish_at(
        RollingProducerEvent::Exit(first.clone()),
        ProducerIngressSlot::Exit,
        started + Duration::from_secs(1),
    );
    ingress.publish_at(
        RollingProducerEvent::Exit(first.clone()),
        ProducerIngressSlot::Exit,
        started + Duration::from_secs(2),
    );

    assert_eq!(
        ingress.drain(),
        vec![RollingProducerEvent::Exit(first)],
        "the first exact terminal observation owns the exit barrier"
    );
    assert!(
        ROLLING_PRODUCER_EVENT_COALESCED[1].load(Ordering::Relaxed) > before,
        "a duplicate removed before actor delivery is accounted as coalesced"
    );
}

#[test]
fn stale_predecessor_exit_cannot_settle_before_timely_successor_progress() {
    let started = Instant::now();
    let successor_started = started + Duration::from_secs(10);
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    assert_eq!(actor.begin_producer_attempt_at(successor_started), Ok(2));

    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Exit(producer_exit(
            1,
            false,
            Some(7),
            None,
            started + Duration::from_secs(31),
        )),
        ProducerIngressSlot::Exit,
        started + Duration::from_secs(31),
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            2,
            100,
            1_000,
            900,
            started + Duration::from_secs(35),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(35),
    );
    actor.handle_producer_blocks_at(started + Duration::from_secs(43), ingress.drain_blocks());

    assert_eq!(actor.producer_deadline_due, None);
    assert_eq!(
        actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 2,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: started + Duration::from_secs(45),
        })
    );
    assert_eq!(actor.delivery.producer_out_time_ms, Some(100));
}

#[test]
fn cutoff_captures_progress_published_while_waiting_for_the_transition_fence() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let ingress = Arc::clone(&actor.producer_events);
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 1_000, 900, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(25),
    );

    let transition = Arc::clone(&actor.producer_transition);
    let transition_guard = transition
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (started_cutoff, cutoff_started) = std::sync::mpsc::channel();
    let cutoff = std::thread::spawn(move || {
        started_cutoff.send(()).expect("announce cutoff");
        actor.run_due_first_cutoff_at(started + Duration::from_secs(40));
        actor
    });
    cutoff_started.recv().expect("cutoff started");
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 200, 1_100, 1_000, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(34),
    );
    drop(transition_guard);
    let actor = cutoff.join().expect("cutoff actor");

    assert_eq!(actor.producer_deadline_due, None);
    assert_eq!(actor.delivery.producer_out_time_ms, Some(200));
    assert_eq!(
        actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: started + Duration::from_secs(44),
        })
    );
}

#[test]
fn an_unacknowledged_hold_signal_becomes_a_typed_flow_stop_deadline() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    assert!(matches!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue { .. }
    ));
    assert!(actor.pending_producer_action.is_some());

    // Before the budget: nothing lapses.
    assert!(!actor.settle_producer_action_deadline_at(started + Duration::from_secs(5)));
    assert!(actor.pending_producer_action.is_some());

    // At the budget: one typed failure, and the signal token is fenced so
    // a late acknowledgement cannot revive the producer.
    assert!(actor.settle_producer_action_deadline_at(started + Duration::from_secs(11)));
    assert!(actor.pending_producer_action.is_none());
    assert!(!actor.producer_signal_authorized);
    assert!(!actor.producer_flow_signal_outstanding);
}

#[test]
fn an_unacknowledged_resume_signal_is_distinguishable_from_a_hold() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    // Reach Held through an acknowledgement so the resume intention is a
    // real divergence rather than a no-op.
    actor.apply_producer_flow_applied_at(
        started + Duration::from_secs(1),
        ProducerFlowApplied {
            revision: 1,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
            published_at: started + Duration::from_secs(1),
        },
    );
    assert!(matches!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(2),
            1,
            ProducerPhysicalFlowState::Running,
        ),
        ProducerFlowIntentionOutcome::Issue { hold: false, .. }
    ));
    assert_eq!(
        actor.pending_producer_action.map(|action| action.kind),
        Some(ProducerActionKind::SignalResume),
    );
    assert_eq!(
        ProducerActionKind::SignalResume.deadline_reason(),
        ProducerDecisionReason::FlowResumeDeadline,
    );
    assert_eq!(
        ProducerActionKind::SignalStop.deadline_reason(),
        ProducerDecisionReason::FlowStopDeadline,
    );
}

#[test]
fn an_acknowledged_signal_leaves_no_action_deadline_behind() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    assert!(matches!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue { .. }
    ));
    actor.apply_producer_flow_applied_at(
        started + Duration::from_secs(2),
        ProducerFlowApplied {
            revision: 1,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
            published_at: started + Duration::from_secs(2),
        },
    );
    assert!(
        actor.pending_producer_action.is_none(),
        "an acknowledged transaction owes nothing"
    );
    assert!(!actor.settle_producer_action_deadline_at(started + Duration::from_secs(30)));
}

#[test]
fn a_lapsed_action_from_a_previous_attempt_is_observation_only() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    assert!(matches!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue { .. }
    ));
    // Succession takes the slot; the predecessor's deadline is stale.
    assert_eq!(
        actor.begin_producer_attempt_at(started + Duration::from_secs(2)),
        Ok(2)
    );
    assert!(actor.pending_producer_action.is_none());
    assert!(!actor.settle_producer_action_deadline_at(started + Duration::from_secs(30)));
    assert!(
        actor.producer_signal_authorized,
        "a successor must not inherit its predecessor's fenced token"
    );
}

#[test]
fn the_next_wake_accounts_for_an_outstanding_action() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let without = actor.next_deadline();
    assert!(matches!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue { .. }
    ));
    let with = actor.next_deadline();
    assert!(
        with <= without,
        "a signal that never comes back has to be able to wake the actor"
    );
    assert!(with <= started + Duration::from_secs(11));
}

#[test]
fn a_flow_intention_never_moves_the_applied_state_or_the_deadline() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let armed = actor.producer_progress_deadline;
    assert!(armed.is_some(), "an admitted attempt arms a deadline");

    // Requesting a hold does not change the running deadline.
    assert_eq!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue {
            revision: 1,
            hold: true
        },
    );
    assert_eq!(actor.producer_progress_deadline, armed);
    assert_eq!(
        actor.producer_physical_flow,
        ProducerPhysicalFlowState::Running
    );
}

#[test]
fn a_flow_intention_coalesces_while_a_signal_is_outstanding() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));

    assert_eq!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue {
            revision: 1,
            hold: true
        },
    );
    // A second desire while that signal is in flight must not authorize a
    // second syscall; it becomes the newest revision instead.
    assert_eq!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(2),
            1,
            ProducerPhysicalFlowState::Running,
        ),
        ProducerFlowIntentionOutcome::Coalesced,
    );
    assert_eq!(actor.producer_flow_intention.revision, 2);
    assert_eq!(
        actor.producer_flow_intention.desired,
        ProducerPhysicalFlowState::Running
    );

    // The hold acknowledgement lands; the newest desire still diverges, so
    // exactly one further signal is owed.
    actor.apply_producer_flow_applied_at(
        started + Duration::from_secs(3),
        ProducerFlowApplied {
            revision: 1,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
            published_at: started + Duration::from_secs(3),
        },
    );
    assert!(actor.settle_producer_flow_signal(1));
}

#[test]
fn a_failed_signal_releases_the_outstanding_claim() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    assert!(matches!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue { .. }
    ));
    // No barrier is published for a failed syscall. Without settling here
    // the actor would believe a signal were forever in flight.
    assert!(
        actor.settle_producer_flow_signal(1),
        "the desire still diverges, so another signal is owed"
    );
    assert_eq!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(2),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue {
            revision: 2,
            hold: true
        },
    );
}

#[test]
fn a_flow_intention_for_another_attempt_is_rejected() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    assert_eq!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            2,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Rejected,
        "a predecessor or successor attempt may not signal this child"
    );
}

#[test]
fn attempt_succession_clears_a_predecessor_flow_intention() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    assert!(matches!(
        actor.apply_producer_flow_intention_at(
            started + Duration::from_secs(1),
            1,
            ProducerPhysicalFlowState::Held,
        ),
        ProducerFlowIntentionOutcome::Issue { .. }
    ));
    assert_eq!(
        actor.begin_producer_attempt_at(started + Duration::from_secs(2)),
        Ok(2)
    );
    assert_eq!(actor.producer_flow_intention.revision, 0);
    assert_eq!(
        actor.producer_flow_intention.desired,
        ProducerPhysicalFlowState::Running
    );
    assert!(
        !actor.producer_flow_signal_outstanding,
        "a successor must not inherit an outstanding signal claim"
    );
}

#[test]
fn exact_physical_hold_disarms_and_resume_grants_a_fresh_progress_budget() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));

    actor.apply_producer_flow_applied_at(
        started + Duration::from_secs(25),
        ProducerFlowApplied {
            revision: 1,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
            published_at: started + Duration::from_secs(25),
        },
    );
    assert_eq!(
        actor.settle_due_deadlines_at(started + Duration::from_secs(40)),
        None,
        "intentional physical suspension cannot become a producer timeout"
    );
    assert_eq!(actor.producer_progress_deadline, None);

    actor.apply_producer_flow_applied_at(
        started + Duration::from_secs(45),
        ProducerFlowApplied {
            revision: 2,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Running,
            published_at: started + Duration::from_secs(45),
        },
    );
    assert_eq!(
        actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: started + Duration::from_secs(55),
        })
    );

    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 1_000, 900, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(50),
    );
    actor.handle_producer_blocks_at(started + Duration::from_secs(51), ingress.drain_blocks());
    assert_eq!(actor.producer_deadline_due, None);
    assert_eq!(
        actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: started + Duration::from_secs(60),
        })
    );
}

#[test]
fn coalesced_hold_then_resume_still_grants_the_resume_budget() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let ingress = Arc::clone(&actor.producer_events);
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(20),
    );
    ingress.publish_flow_at(
        RollingProducerFlowObservation {
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
        },
        started + Duration::from_secs(25),
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 200, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(30),
    );
    ingress.publish_flow_at(
        RollingProducerFlowObservation {
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Running,
        },
        started + Duration::from_secs(45),
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 300, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(50),
    );

    actor.run_due_first_cutoff_at(started + Duration::from_secs(51));
    assert_eq!(actor.producer_deadline_due, None);
    assert_eq!(actor.delivery.producer_out_time_ms, Some(300));
    assert_eq!(
        actor.producer_physical_flow,
        ProducerPhysicalFlowState::Running
    );
    assert_eq!(
        actor.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: started + Duration::from_secs(60),
        })
    );
}

#[test]
fn stale_flow_revision_or_attempt_cannot_change_state_or_rearm_deadline() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    actor.apply_producer_flow_applied_at(
        started + Duration::from_secs(5),
        ProducerFlowApplied {
            revision: 2,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
            published_at: started + Duration::from_secs(5),
        },
    );
    assert_eq!(actor.producer_flow_revision, 2);
    assert_eq!(
        actor.producer_physical_flow,
        ProducerPhysicalFlowState::Held
    );
    assert_eq!(actor.producer_progress_deadline, None);

    for applied in [
        ProducerFlowApplied {
            revision: 1,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Running,
            published_at: started + Duration::from_secs(6),
        },
        ProducerFlowApplied {
            revision: 3,
            producer_attempt: 0,
            state: ProducerPhysicalFlowState::Running,
            published_at: started + Duration::from_secs(7),
        },
    ] {
        actor.apply_producer_flow_applied_at(applied.published_at, applied);
        assert_eq!(actor.producer_flow_revision, 2);
        assert_eq!(
            actor.producer_physical_flow,
            ProducerPhysicalFlowState::Held
        );
        assert_eq!(actor.producer_progress_deadline, None);
    }
}

#[tokio::test]
async fn flow_barrier_capacity_defers_then_wakes_a_third_signal_before_application() {
    let ingress = RollingProducerIngress::new();
    assert!(ingress.reserve_flow_barrier());
    assert!(ingress.reserve_flow_barrier());
    assert!(!ingress.reserve_flow_barrier());
    let retired = AtomicBool::new(false);
    let (actor, _receiver) = tokio::sync::mpsc::channel(1);

    let wait_then_reserve = async {
        assert!(
            ingress
                .wait_for_flow_barrier_capacity(&retired, &actor)
                .await
        );
        assert!(ingress.reserve_flow_barrier());
        ingress.finish_reserved_flow_barrier(
            RollingProducerFlowObservation {
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
            },
            false,
        );
    };
    let release_capacity = async {
        tokio::task::yield_now().await;
        ingress.finish_reserved_flow_barrier(
            RollingProducerFlowObservation {
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
            },
            false,
        );
    };
    tokio::join!(wait_then_reserve, release_capacity);
    ingress.finish_reserved_flow_barrier(
        RollingProducerFlowObservation {
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Running,
        },
        false,
    );
    assert!(ingress.drain_blocks().is_empty());
}

#[test]
fn command_envelope_retains_flow_capacity_until_actor_application() {
    let ingress = RollingProducerIngress::new();
    for held in [true, false] {
        assert!(ingress.reserve_flow_barrier());
        ingress.finish_reserved_flow_barrier(
            RollingProducerFlowObservation {
                producer_attempt: 1,
                state: if held {
                    ProducerPhysicalFlowState::Held
                } else {
                    ProducerPhysicalFlowState::Running
                },
            },
            true,
        );
    }
    let (reply, _response) = tokio::sync::oneshot::channel();
    let envelope = ingress.seal_command(RollingControlCommand::Snapshot { reply });
    assert_eq!(envelope.sealed_flow_barriers, 2);
    assert!(
        !ingress.reserve_flow_barrier(),
        "moving barriers into the bounded mailbox must not authorize another syscall"
    );
    ingress.release_sealed_flow_barriers(envelope.sealed_flow_barriers);
    assert!(ingress.reserve_flow_barrier());
    ingress.finish_reserved_flow_barrier(
        RollingProducerFlowObservation {
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Running,
        },
        false,
    );
}

#[tokio::test]
async fn closed_actor_mailbox_fences_and_releases_a_full_capacity_waiter() {
    let control = RollingControlHandle::unavailable_for_test();
    assert!(control.reserve_producer_flow_capacity_for_test());
    assert!(control.reserve_producer_flow_capacity_for_test());

    assert!(!control.wait_for_producer_flow_capacity().await);
    assert!(control.is_retired());

    control.release_producer_flow_capacity_for_test();
    control.release_producer_flow_capacity_for_test();
}

#[test]
fn flow_application_never_outranks_exact_session_expiry() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));

    actor.apply_producer_flow_applied_at(
        started + ROLLING_LEGACY_LEASE_TIMEOUT,
        ProducerFlowApplied {
            revision: 1,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
            published_at: started + Duration::from_secs(1),
        },
    );

    assert!(actor.retired);
    assert_eq!(actor.terminal, Some(RollingTerminalCause::LeaseExpired));
    assert_eq!(actor.producer_flow_revision, 0);
    assert_eq!(
        actor.producer_physical_flow,
        ProducerPhysicalFlowState::Running
    );
}

#[test]
fn physical_hold_before_or_at_deadline_disarms_but_late_hold_preserves_due() {
    let started = Instant::now();
    for (offset_ms, due) in [(-1_i64, false), (0, false), (1, true)] {
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let published_at = if offset_ms < 0 {
            started + PRODUCER_STARTUP_BUDGET - Duration::from_millis(offset_ms.unsigned_abs())
        } else {
            started + PRODUCER_STARTUP_BUDGET + Duration::from_millis(offset_ms as u64)
        };
        actor.apply_producer_flow_applied_at(
            published_at,
            ProducerFlowApplied {
                revision: 1,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
                published_at,
            },
        );
        assert_eq!(actor.producer_deadline_due.is_some(), due);
        assert_eq!(actor.producer_progress_deadline, None);

        actor.apply_producer_flow_applied_at(
            published_at + Duration::from_secs(1),
            ProducerFlowApplied {
                revision: 2,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Running,
                published_at: published_at + Duration::from_secs(1),
            },
        );
        assert_eq!(actor.producer_progress_deadline.is_none(), due);
    }
}

#[test]
fn provisional_deadline_observation_never_outranks_session_lifecycle() {
    let started = Instant::now();
    for terminal_at in [
        started + PRODUCER_STARTUP_BUDGET - Duration::from_millis(1),
        started + PRODUCER_STARTUP_BUDGET,
        started + PRODUCER_STARTUP_BUDGET + Duration::from_millis(1),
    ] {
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        if terminal_at > started + PRODUCER_STARTUP_BUDGET {
            assert!(actor
                .settle_due_deadlines_at(started + PRODUCER_STARTUP_BUDGET)
                .is_some());
        }
        assert!(actor.terminate(RollingTerminalCause::End).won());
        assert_eq!(actor.settle_due_deadlines_at(terminal_at), None);
        assert_eq!(actor.producer_deadline_due, None);
        assert_eq!(actor.producer_process_exit_due, None);
    }
}

#[tokio::test(start_paused = true)]
async fn lifecycle_commands_before_at_and_after_provisional_producer_due_still_win() {
    for offset in [-1_i64, 0, 1] {
        let handle = RollingControlHandle::spawn_for_test("session-start");
        assert_eq!(handle.begin_producer_attempt().await, Ok(1));
        let advance = if offset < 0 {
            PRODUCER_STARTUP_BUDGET - Duration::from_millis(offset.unsigned_abs())
        } else {
            PRODUCER_STARTUP_BUDGET + Duration::from_millis(offset as u64)
        };
        tokio::time::advance(advance).await;
        tokio::task::yield_now().await;

        assert_eq!(
            handle.end().await.expect("lifecycle command"),
            RollingTerminalOutcome::Won(RollingTerminalCause::End),
            "the action-passive producer observation must not own lifecycle ordering at offset {offset}"
        );
        let snapshot = handle.snapshot().await.expect("terminal snapshot");
        assert_eq!(snapshot.terminal, Some(RollingTerminalCause::End));
        assert!(snapshot.retired);
    }
}

#[test]
fn lease_terminal_wins_an_exact_tie_with_the_producer_deadline() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    actor.last_renewal = started - (ROLLING_LEGACY_LEASE_TIMEOUT - PRODUCER_STARTUP_BUDGET);
    let tied = started + PRODUCER_STARTUP_BUDGET;
    assert_eq!(actor.deadline(), tied);
    assert_eq!(actor.next_deadline(), tied);

    assert_eq!(actor.settle_due_deadlines_at(tied), None);
    assert_eq!(actor.terminal, Some(RollingTerminalCause::LeaseExpired));
    assert_eq!(actor.producer_deadline_due, None);
    assert_eq!(actor.producer_progress_deadline, None);
}

#[test]
fn command_envelope_splits_progress_and_preserves_global_ingress_order() {
    let started = Instant::now();
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started,
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            200,
            950,
            850,
            started + Duration::from_secs(1),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(1),
    );
    let (reply, _response) = tokio::sync::oneshot::channel();
    let envelope = ingress.seal_command(RollingControlCommand::Snapshot { reply });
    assert_eq!(envelope.sealed_flow_barriers, 0);
    let [RollingProducerIngressBlock::Progress(preceding)] = envelope.preceding_producer.as_slice()
    else {
        panic!("command seals exactly its preceding progress batch");
    };
    assert!(preceding.last_sequence() < envelope.sequence);
    assert_eq!(
        preceding
            .latest_progress
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(200)
    );

    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            300,
            1_000,
            900,
            started + Duration::from_secs(2),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(2),
    );
    let successor_blocks = ingress.drain_blocks();
    let [RollingProducerIngressBlock::Progress(successor)] = successor_blocks.as_slice() else {
        panic!("post-command progress starts a successor batch");
    };
    assert!(successor.first_sequence() > envelope.sequence);
    assert_eq!(
        successor
            .first_advancing
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(300)
    );
}

#[tokio::test]
async fn queued_command_cutoff_cannot_drain_its_successor_progress_early() {
    let started = rolling_now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let ingress = Arc::clone(&actor.producer_events);
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(1),
    );
    let (reply, response) = tokio::sync::oneshot::channel();
    let envelope = ingress.seal_command(RollingControlCommand::Snapshot { reply });
    let command_sequence = envelope.sequence;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    sender.try_send(envelope).expect("command queued");
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 200, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(2),
    );

    let selected = actor
        .command_before_cutoff(&mut receiver)
        .expect("queued command wins cutoff");
    assert!(
        ingress
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .progress
            .is_some(),
        "post-command progress remains in ingress"
    );
    actor.handle_command(selected).await;
    let snapshot = response.await.expect("snapshot reply");
    assert_eq!(snapshot.delivery.producer_out_time_ms, Some(100));
    assert_eq!(
        snapshot.producer_control.last_applied_sequence,
        command_sequence
    );

    actor.run_due_first_cutoff_at(started + Duration::from_secs(3));
    assert_eq!(actor.delivery.producer_out_time_ms, Some(200));
    assert!(actor.last_applied_ingress_sequence > command_sequence);
}

#[tokio::test(start_paused = true)]
async fn command_publication_time_not_dispatch_delay_owns_lease_ordering() {
    let published_at = rolling_now();
    let accepted_at = published_at - ROLLING_LEGACY_LEASE_TIMEOUT + Duration::from_millis(2);
    let mut actor = RollingControlActor::new(
        accepted_at,
        "session-start",
        Arc::new(AtomicBool::new(false)),
    );
    let (snapshot_reply, snapshot_response) = tokio::sync::oneshot::channel();
    let snapshot_envelope = RollingControlEnvelope {
        sequence: 1,
        published_at,
        preceding_producer: Vec::new(),
        sealed_flow_barriers: 0,
        command: RollingControlCommand::Snapshot {
            reply: snapshot_reply,
        },
    };
    let (reply, response) = tokio::sync::oneshot::channel();
    let envelope = RollingControlEnvelope {
        sequence: 2,
        published_at: published_at + Duration::from_millis(1),
        preceding_producer: Vec::new(),
        sealed_flow_barriers: 0,
        command: RollingControlCommand::Renew {
            kind: "sequenced-before-deadline",
            source: RollingRenewalSource::Internal,
            reply,
        },
    };
    tokio::time::advance(Duration::from_millis(3)).await;
    actor.handle_command(snapshot_envelope).await;
    assert!(
        !snapshot_response
            .await
            .expect("pre-deadline snapshot reply")
            .retired,
        "dispatch delay cannot move a sequenced snapshot past a later renewal"
    );
    actor.handle_command(envelope).await;
    assert!(response.await.expect("renewal reply"));
    assert!(!actor.retired);
    assert_eq!(actor.last_renewal, published_at + Duration::from_millis(1));

    let tied = rolling_now();
    let mut tied_actor = RollingControlActor::new(
        tied - ROLLING_LEGACY_LEASE_TIMEOUT,
        "session-start",
        Arc::new(AtomicBool::new(false)),
    );
    let (reply, response) = tokio::sync::oneshot::channel();
    tied_actor
        .handle_command(RollingControlEnvelope {
            sequence: 1,
            published_at: tied,
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::Terminal {
                cause: RollingTerminalCause::End,
                reply,
            },
        })
        .await;
    assert_eq!(
        response.await.expect("terminal reply"),
        RollingTerminalOutcome::AlreadyTerminal(RollingTerminalCause::LeaseExpired)
    );
}

#[tokio::test(start_paused = true)]
async fn real_actor_control_after_producer_deadline_exposes_due_projection() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let attempt = handle
        .begin_producer_attempt()
        .await
        .expect("producer attempt");
    tokio::time::advance(PRODUCER_STARTUP_BUDGET + Duration::from_millis(1)).await;

    let request = request();
    let outcome = handle
        .control(LocalControlRequest {
            session_id: "unused",
            generation: &request.generation,
            owner_node_id: "node-a",
            owner_epoch: request.control_epoch,
            client_instance_id: &request.client_instance_id,
            sequence: request.sequence,
            snapshot: PlaybackDemandSnapshot::from(&request),
            prepared_successor: PreparedSuccessorObservation::NotRequested,
        })
        .await
        .expect("post-deadline control remains compatibility-passive");

    assert_eq!(outcome.lease.producer_control.deadline_attempt, None);
    assert_eq!(outcome.lease.producer_control.deadline_mode, None);
    assert_eq!(outcome.lease.producer_control.due_attempt, Some(attempt));
    assert_eq!(outcome.lease.producer_control.due_mode, Some("starting"));
}

#[tokio::test(start_paused = true)]
async fn real_actor_timer_and_mailbox_tie_settles_due_before_control() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let pause = crate::seam_hooks::AsyncPause::new("producer attempt reply");
    handle.pause_producer_attempt_reply(Arc::clone(&pause));
    let begin = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.begin_producer_attempt().await })
    };
    let held = pause.reached().await;
    let attempt = handle.current_producer_attempt();
    assert!(attempt > 0);

    tokio::time::advance(PRODUCER_STARTUP_BUDGET).await;
    let request = request();
    let (reply, response) = tokio::sync::oneshot::channel();
    assert!(
        handle.try_enqueue_command_for_test(RollingControlCommand::Control {
            request: Box::new(owned_control(&request)),
            deadline_unix_ms: i64::MAX,
            terminal_admission: None,
            preparation_admission: None,
            reply,
        })
    );
    held.release();
    assert_eq!(begin.await.expect("begin task"), Ok(attempt));

    let outcome = response
        .await
        .expect("control response")
        .expect("control accepted");
    assert_eq!(outcome.lease.producer_control.deadline_attempt, None);
    assert_eq!(outcome.lease.producer_control.due_attempt, Some(attempt));
    assert_eq!(outcome.lease.producer_control.due_mode, Some("starting"));
}

#[tokio::test(start_paused = true)]
async fn real_actor_predeadline_command_keeps_publication_time_after_delayed_dispatch() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let pause = crate::seam_hooks::AsyncPause::new("producer attempt reply");
    handle.pause_producer_attempt_reply(Arc::clone(&pause));
    let begin = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.begin_producer_attempt().await })
    };
    let held = pause.reached().await;
    let attempt = handle.current_producer_attempt();
    assert!(attempt > 0);

    tokio::time::advance(PRODUCER_STARTUP_BUDGET - Duration::from_millis(1)).await;
    let request = request();
    let (reply, response) = tokio::sync::oneshot::channel();
    assert!(
        handle.try_enqueue_command_for_test(RollingControlCommand::Control {
            request: Box::new(owned_control(&request)),
            deadline_unix_ms: i64::MAX,
            terminal_admission: None,
            preparation_admission: None,
            reply,
        })
    );
    tokio::time::advance(Duration::from_millis(2)).await;
    held.release();
    assert_eq!(begin.await.expect("begin task"), Ok(attempt));

    let outcome = response
        .await
        .expect("control response")
        .expect("control accepted");
    assert_eq!(
        outcome.lease.producer_control.deadline_attempt,
        Some(attempt)
    );
    assert_eq!(
        outcome.lease.producer_control.deadline_mode,
        Some("starting")
    );
    assert_eq!(outcome.lease.producer_control.due_attempt, None);
    assert_eq!(outcome.lease.producer_control.due_mode, None);
}

#[test]
fn producer_operational_projection_is_bounded_and_tracks_actor_phase() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    let starting = actor.producer_operational_snapshot_at(started);
    assert_eq!(starting.phase, "starting");
    assert_eq!(starting.deadline_mode, Some("starting"));
    assert!(starting.observation_only);

    assert!(actor.observe_producer_progress_at(
        started + Duration::from_secs(1),
        producer_progress(1, 100, 900, 800, started + Duration::from_secs(1)),
    ));
    assert_eq!(
        actor
            .producer_operational_snapshot_at(started + Duration::from_secs(1))
            .phase,
        "running"
    );
    actor.apply_producer_flow_applied_at(
        started + Duration::from_secs(2),
        ProducerFlowApplied {
            revision: 1,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
            published_at: started + Duration::from_secs(2),
        },
    );
    let held = actor.producer_operational_snapshot_at(started + Duration::from_secs(2));
    assert_eq!(held.phase, "held");
    assert_eq!(held.physical_flow, "held");

    assert!(actor.terminate(RollingTerminalCause::End).won());
    let terminal = actor.producer_operational_snapshot_at(started + Duration::from_secs(3));
    assert_eq!(terminal.phase, "terminal");
    assert_eq!(terminal.deadline_mode, None);
}

#[test]
fn producer_progress_watermark_rejects_a_cross_drain_repeat_as_deadline_evidence() {
    let started = Instant::now();
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started,
    );
    let initial_blocks = ingress.drain_blocks();
    let [RollingProducerIngressBlock::Progress(initial)] = initial_blocks.as_slice() else {
        panic!("initial progress batch");
    };
    assert_eq!(
        initial
            .first_advancing
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(100)
    );

    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            100,
            950,
            850,
            started + Duration::from_secs(5),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(5),
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            150,
            1_000,
            900,
            started + Duration::from_secs(14),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(14),
    );
    let repeated_blocks = ingress.drain_blocks();
    let [RollingProducerIngressBlock::Progress(repeated)] = repeated_blocks.as_slice() else {
        panic!("post-drain progress batch");
    };
    assert_eq!(
        repeated
            .first_advancing
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(150)
    );
    assert_eq!(
        repeated
            .first_advancing
            .as_ref()
            .map(|sample| sample.published_at),
        Some(started + Duration::from_secs(14)),
        "the repeated 100 at 5s cannot manufacture a deadline link"
    );
    assert_eq!(
        repeated.covered_deadline,
        Some(started + Duration::from_secs(24))
    );
}

#[test]
fn producer_progress_watermark_resets_for_successor_and_rejects_late_predecessor() {
    let started = Instant::now();
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 50_000, 900, 800, started)),
        ProducerIngressSlot::Progress,
        started,
    );
    let predecessor_blocks = ingress.drain_blocks();
    let [RollingProducerIngressBlock::Progress(predecessor)] = predecessor_blocks.as_slice() else {
        panic!("predecessor progress batch");
    };
    assert_eq!(
        predecessor
            .latest_progress
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(50_000)
    );

    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            2,
            100,
            1_000,
            900,
            started + Duration::from_secs(1),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(1),
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            60_000,
            2_000,
            1_900,
            started + Duration::from_secs(2),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(2),
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            2,
            200,
            1_100,
            1_000,
            started + Duration::from_secs(3),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_secs(3),
    );
    let successor_blocks = ingress.drain_blocks();
    let [RollingProducerIngressBlock::Progress(successor)] = successor_blocks.as_slice() else {
        panic!("successor progress batch");
    };
    assert_eq!(successor.producer_attempt, 2);
    assert_eq!(
        successor
            .first_advancing
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(100),
        "a successor's restarted timeline initializes fresh coverage"
    );
    assert_eq!(
        successor
            .covered_last
            .as_ref()
            .and_then(|sample| sample.observation.out_time_ms),
        Some(200),
        "late predecessor output cannot evict or extend the successor batch"
    );
    let state = ingress
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(state.progress_watermark_attempt, 2);
    assert_eq!(state.progress_watermark_out_time_ms, Some(200));
}

#[tokio::test]
async fn producer_ingress_survives_a_full_actor_mailbox_with_command_causality() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let pause = crate::seam_hooks::AsyncPause::new("producer attempt reply");
    handle.pause_producer_attempt_reply(Arc::clone(&pause));
    let begin = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.begin_producer_attempt().await })
    };
    let held = pause.reached().await;
    let attempt = handle.current_producer_attempt();
    assert!(attempt > 0, "attempt admission precedes its paused reply");

    for _ in 0..ROLLING_ACTOR_MAILBOX_CAPACITY {
        let (reply, _response) = tokio::sync::oneshot::channel();
        assert!(
            handle.try_enqueue_command_for_test(RollingControlCommand::Snapshot { reply }),
            "paused actor mailbox has the advertised bounded capacity"
        );
    }
    let (overflow_reply, _overflow_response) = tokio::sync::oneshot::channel();
    assert!(
        !handle.try_enqueue_command_for_test(RollingControlCommand::Snapshot {
            reply: overflow_reply
        })
    );

    handle.observe_producer_progress(attempt, Some(5_000), Some(1_250), Some(1_100));
    handle.observe_producer_progress(attempt, Some(6_000), Some(1_200), Some(1_050));
    held.release();
    assert_eq!(
        begin.await.expect("attempt task"),
        Ok(attempt),
        "the command establishing the attempt remains ordered before its events"
    );
    let delivery = tokio::time::timeout(Duration::from_secs(2), handle.snapshot())
        .await
        .expect("actor drains bounded commands")
        .expect("actor snapshot")
        .delivery;
    assert_eq!(delivery.producer_attempt, attempt);
    assert_eq!(delivery.producer_out_time_ms, Some(6_000));
    assert_eq!(delivery.producer_speed_milli, Some(1_200));
    assert_eq!(delivery.producer_recent_speed_milli, Some(1_050));
}

#[tokio::test]
async fn stale_predecessor_exit_cannot_steal_successor_progress_before_drain() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let predecessor = handle
        .begin_producer_attempt()
        .await
        .expect("predecessor attempt");
    handle.observe_producer_exit(predecessor, false, Some(11), None);
    let successor = handle
        .begin_producer_attempt()
        .await
        .expect("successor attempt");
    assert!(successor > predecessor);

    handle.observe_producer_progress(successor, Some(7_000), Some(1_100), Some(1_050));
    handle.observe_producer_exit(predecessor, false, Some(12), None);
    handle.observe_producer_exit(successor, false, Some(13), None);
    let snapshot = handle.snapshot().await.expect("ordered actor snapshot");
    assert_eq!(snapshot.delivery.producer_attempt, successor);
    assert_eq!(snapshot.delivery.producer_out_time_ms, Some(7_000));
    let exit = snapshot.delivery.producer_exit.expect("successor exit");
    assert!(!exit.success);
    assert_eq!(exit.code, Some(13));
    assert_eq!(exit.signal, None);
}

#[test]
fn exit_seals_preceding_progress_and_later_progress_stays_terminal() {
    let started = Instant::now();
    let ingress = RollingProducerIngress::new();
    ingress.publish_progress(producer_progress(1, 100, 900, 800, started));
    ingress.publish_exit(producer_exit(
        1,
        false,
        Some(7),
        None,
        started + Duration::from_secs(1),
    ));
    ingress.publish_progress(producer_progress(
        1,
        200,
        1_000,
        900,
        started + Duration::from_secs(2),
    ));
    let events = ingress.drain();
    assert_eq!(
        events.len(),
        3,
        "the exit barrier retains one batch on each side"
    );
    assert!(matches!(&events[0], RollingProducerEvent::Progress(_)));
    assert!(matches!(&events[1], RollingProducerEvent::Exit(_)));
    assert!(matches!(&events[2], RollingProducerEvent::Progress(_)));

    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(
        actor.begin_producer_attempt_at(started),
        Ok(1),
        "fixture uses the ingress attempt"
    );
    for event in events {
        actor.handle_producer_event(event);
    }
    let delivery = actor.snapshot_at(started + Duration::from_secs(3)).delivery;
    assert_eq!(delivery.producer_out_time_ms, Some(100));
    assert_eq!(
        delivery.producer_exit.as_ref().map(|exit| exit.code),
        Some(Some(7))
    );
}

#[test]
fn rolling_equal_fetch_resolves_its_pending_end() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");
    assert!(actor.commit_media_at(
        started + Duration::from_secs(1),
        "segment",
        attempt,
        Some(3),
        None,
    ));
    assert!(actor.commit_media_at(
        started + Duration::from_secs(2),
        "segment",
        attempt,
        Some(3),
        Some(9_000),
    ));
    assert_eq!(actor.delivery.fetched_segment, Some(3));
    assert_eq!(actor.delivery.fetched_end_ms, 9_000);
    assert_eq!(actor.delivery.pending_fetched_segment, None);
}

#[test]
fn rolling_producer_install_requires_the_exact_live_attempt() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");
    assert_eq!(
        actor.authorize_producer_install_at(
            started + Duration::from_secs(1),
            attempt.saturating_sub(1),
        ),
        Err(ProducerAttemptRejection::StaleAttempt)
    );
    assert!(actor
        .authorize_producer_install_at(started + Duration::from_secs(1), attempt,)
        .is_ok());
    assert_eq!(
        actor.authorize_producer_install_at(
            started + Duration::from_secs(1) + ROLLING_LEGACY_LEASE_TIMEOUT,
            attempt,
        ),
        Err(ProducerAttemptRejection::SessionEnded)
    );
    assert!(actor.retired);
}

#[test]
fn rolling_delivery_resolves_fetch_after_publication_and_stays_monotonic() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");
    assert!(actor.commit_media_at(
        started + Duration::from_secs(1),
        "segment",
        attempt,
        Some(3),
        None,
    ));
    assert_eq!(actor.delivery.fetched_segment, Some(3));
    assert_eq!(actor.delivery.fetched_end_ms, 0);
    assert_eq!(actor.delivery.pending_fetched_segment, Some(3));

    assert!(actor.observe_publication_at(
        started + Duration::from_secs(1),
        publication(attempt, true, 2, 6_000, Some(2)),
    ));
    assert_eq!(actor.delivery.fetched_end_ms, 0);
    assert_eq!(actor.delivery.pending_fetched_segment, Some(3));

    assert!(actor.observe_publication_at(
        started + Duration::from_secs(1),
        publication(attempt, true, 3, 9_000, Some(3)),
    ));
    assert_eq!(actor.delivery.fetched_end_ms, 9_000);
    assert_eq!(actor.delivery.pending_fetched_segment, None);

    assert!(actor.commit_media_at(
        started + Duration::from_secs(2),
        "segment-range",
        attempt,
        None,
        None,
    ));
    assert!(actor.observe_publication_at(
        started + Duration::from_secs(2),
        publication(attempt, true, 2, 7_000, None),
    ));
    assert_eq!(actor.delivery.published_segment, Some(3));
    assert_eq!(actor.delivery.published_end_ms, Some(9_000));
    assert_eq!(actor.delivery.fetched_segment, Some(3));
    assert_eq!(actor.delivery.fetched_end_ms, 9_000);
}

#[test]
fn rolling_delivery_retirement_rejects_late_mutations() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");
    assert!(actor.terminate(RollingTerminalCause::End).won());
    assert!(!actor.observe_publication_at(
        started + Duration::from_secs(1),
        publication(attempt, true, 1, 4_000, None),
    ));
    assert!(!actor.commit_media_at(
        started + Duration::from_secs(1),
        "segment",
        attempt,
        Some(1),
        Some(4_000),
    ));
    assert_eq!(actor.delivery.published_segment, None);
    assert_eq!(actor.delivery.fetched_segment, None);
}

#[test]
fn rolling_delivery_expiry_rejects_late_publication() {
    let started = Instant::now();
    let retired_fence = Arc::new(AtomicBool::new(false));
    let mut actor = RollingControlActor::new(started, "session-start", Arc::clone(&retired_fence));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");

    assert!(!actor.observe_publication_at(
        started + ROLLING_LEGACY_LEASE_TIMEOUT,
        publication(attempt, true, 1, 4_000, None),
    ));
    assert!(actor.retired);
    assert!(actor.expiration_claimed);
    assert!(retired_fence.load(Ordering::Acquire));
    assert_eq!(actor.delivery.published_segment, None);
}

#[test]
fn rolling_absolute_expiry_subtracts_reply_delay_from_the_deadline() {
    let started = Instant::now();
    let actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let snapshot = actor.snapshot_at(started + Duration::from_secs(10));
    assert_eq!(snapshot.remaining, Duration::from_secs(50));
    assert_eq!(
        snapshot.expires_at_unix_ms_at(started + Duration::from_secs(25), 1_000_000),
        1_035_000,
        "a delayed oneshot reply cannot move the real monotonic deadline"
    );
}

#[test]
fn rolling_actor_claims_expiry_once_and_fences_every_later_renewal() {
    let started = Instant::now();
    let retired_fence = Arc::new(AtomicBool::new(false));
    let mut actor = RollingControlActor::new(started, "session-start", Arc::clone(&retired_fence));
    let claimed =
        actor.claim_expiry_at(started + ROLLING_LEGACY_LEASE_TIMEOUT + Duration::from_millis(1));
    assert!(matches!(
        claimed,
        RollingExpiryClaim::Claimed(ref lease)
            if lease.retired && lease.expiration_claimed && !lease.expired()
    ));
    assert!(retired_fence.load(Ordering::Acquire));
    assert!(matches!(
        actor.claim_expiry_at(started + ROLLING_LEGACY_LEASE_TIMEOUT + Duration::from_secs(1)),
        RollingExpiryClaim::Retired(_)
    ));
    assert!(!actor.renew_at(
        started + Duration::from_secs(62),
        "segment",
        RollingRenewalSource::Media,
    ));
    assert_eq!(
        actor.control_at(started + Duration::from_secs(63), owned_control(&request())),
        Err(ControlStateError::RollingEnded(
            RollingTerminalCause::LeaseExpired
        ))
    );

    let mut renewed =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert!(renewed.renew_at(
        started + Duration::from_secs(59),
        "playlist",
        RollingRenewalSource::Media,
    ));
    assert_eq!(
        renewed.claim_expiry_at(started + Duration::from_secs(61)),
        RollingExpiryClaim::Live
    );
    assert!(matches!(
        renewed.claim_expiry_at(started + Duration::from_secs(120)),
        RollingExpiryClaim::Claimed(_)
    ));
}

#[test]
fn explicit_deadline_cannot_be_revived_between_repair_ticks() {
    let started = Instant::now();
    let retired_fence = Arc::new(AtomicBool::new(false));
    let mut actor = RollingControlActor::new(started, "session-start", Arc::clone(&retired_fence));
    let request = request();
    let accepted_at = started + Duration::from_secs(20);
    let accepted = actor
        .control_at(accepted_at, owned_control(&request))
        .expect("legacy lease accepts the first explicit control");
    assert_eq!(accepted.lease.mode, RollingLeaseMode::Explicit);
    assert_eq!(accepted.lease.remaining, ROLLING_EXPLICIT_LEASE_TIMEOUT);

    let deadline = accepted_at + ROLLING_EXPLICIT_LEASE_TIMEOUT;
    assert!(actor.renew_at(
        deadline - Duration::from_millis(1),
        "segment",
        RollingRenewalSource::Media,
    ));
    let renewed_deadline = deadline - Duration::from_millis(1) + ROLLING_EXPLICIT_LEASE_TIMEOUT;
    assert!(!actor.renew_at(renewed_deadline, "playlist", RollingRenewalSource::Media,));
    assert!(retired_fence.load(Ordering::Acquire));
    assert_eq!(
        actor.control_at(
            renewed_deadline + Duration::from_millis(1),
            owned_control(&request)
        ),
        Err(ControlStateError::RollingEnded(
            RollingTerminalCause::LeaseExpired
        ))
    );
}

#[test]
fn thirty_minute_foreground_play_pause_remains_live_on_delivered_heartbeats() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    actor.begin_producer_attempt_at(started).expect("producer");
    actor.startup.activate_at(started);
    let mut request = request();
    request.render_state = RenderState::Rendering;
    request.position_ms = 1_000;
    actor
        .control_at(started, owned_control(&request))
        .expect("presentation baseline");
    request.sequence += 1;
    request.position_ms += 300;
    let presented = actor
        .control_at(
            started + Duration::from_millis(300),
            owned_control(&request),
        )
        .expect("advancing rendering observation");
    assert_eq!(
        presented.lease.startup.phase,
        RollingStartupPhase::Presented
    );

    let mut now = started + Duration::from_secs(1);
    for sequence in 3..=362 {
        request.sequence = sequence;
        // Continuous Hold retains its existing finite grace after presentation,
        // too. Real foreground playback resumes between bounded pause episodes;
        // a heartbeat cannot renew the pause grace by itself.
        let active = (sequence - 3) % 30 == 0;
        request.demand = if active {
            PlaybackDemand::Active
        } else {
            PlaybackDemand::Hold
        };
        request.playback_rate = if active { 1.0 } else { 0.0 };
        request.render_state = if active {
            RenderState::Rendering
        } else {
            RenderState::Waiting
        };
        let outcome = actor
            .control_at(now, owned_control(&request))
            .expect("delivered foreground heartbeat remains admissible");
        assert_eq!(outcome.disposition, ControlDisposition::Accepted);
        assert_eq!(
            outcome.lease.demand.as_ref().map(|demand| demand.demand),
            Some(request.demand)
        );
        now += Duration::from_secs(5);
    }
    let lease = actor.snapshot_at(now);
    assert!(!lease.expired());
    assert_eq!(lease.mode, RollingLeaseMode::Explicit);
    assert_eq!(lease.remaining, Duration::from_secs(25));
}

#[tokio::test]
async fn rolling_mailbox_orders_media_retirement_and_observation() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    assert!(handle.renew_media("playlist").await);
    assert_eq!(
        handle.snapshot().await.expect("snapshot").last_renewal_kind,
        "playlist"
    );
    assert!(handle.end().await.expect("end verdict").won());
    assert!(!handle.renew_media("segment").await);
    assert!(handle.snapshot().await.is_some_and(|lease| lease.retired));
}

/// The race test's hooks: the first waiter to pass its check holds the
/// pause; later ones pass through once it is released.
struct FlowWaitPause(Arc<crate::seam_hooks::AsyncPause>);

impl RollingFlowSyncHooks for FlowWaitPause {
    fn after_wait_check(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(async move {
            self.0.hold().await;
        })
    }
}

#[tokio::test]
async fn flow_completion_between_check_and_await_cannot_be_lost() {
    let pause = crate::seam_hooks::AsyncPause::new("flow wait after check");
    let sync = Arc::new(RollingFlowSync::with_hooks(Box::new(FlowWaitPause(
        Arc::clone(&pause),
    ))));
    let ticket = sync.request();
    let waiter = {
        let sync = Arc::clone(&sync);
        tokio::spawn(async move { sync.wait_for(ticket).await })
    };

    // The waiter has registered but has not awaited the notification.
    // `notify_waiters` in this exact interval must still release it.
    let held = pause.reached().await;
    sync.complete(ticket);
    held.release();
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("registered completion wake was retained")
        .expect("waiter task");
}

/// The race test's scenario through the production constructor: the
/// no-op hook is ready at once, so the first poll leaves the waiter parked
/// on its registered notification, and the completion in that gap wakes
/// it. Polled by hand, no task and no timer.
#[test]
fn rolling_flow_sync_shipped_shape() {
    use std::future::Future;
    let sync = RollingFlowSync::new();
    let ticket = sync.request();
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let mut waiter = std::pin::pin!(sync.wait_for(ticket));
    assert!(
        waiter.as_mut().poll(&mut context).is_pending(),
        "an unapplied ticket parks its waiter"
    );
    sync.complete(ticket);
    assert!(
        waiter.as_mut().poll(&mut context).is_ready(),
        "the production hook let the waiter reach its registered notification"
    );
}

#[tokio::test]
async fn unavailable_mailbox_fence_orders_after_an_inflight_producer_signal() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let transition = handle.lock_producer_transition();
    assert!(handle.producer_transition_is_live(&transition));
    let (fenced, observed) = std::sync::mpsc::channel();
    let worker = {
        let handle = handle.clone();
        std::thread::spawn(move || {
            handle.fence_unavailable();
            fenced.send(()).expect("fence observation");
        })
    };

    assert!(
        observed.recv_timeout(Duration::from_millis(50)).is_err(),
        "fail-closed retirement cannot publish through the signal gate"
    );
    assert!(!handle.is_retired());
    drop(transition);
    observed
        .recv_timeout(Duration::from_secs(1))
        .expect("fence follows the signal transition");
    worker.join().expect("fence thread");
    assert!(handle.is_retired());
}

#[tokio::test(start_paused = true)]
async fn actor_timer_publishes_the_fence_at_each_modes_exact_deadline() {
    let legacy = RollingControlHandle::spawn_for_test("session-start");
    tokio::task::yield_now().await;
    tokio::time::advance(ROLLING_LEGACY_LEASE_TIMEOUT).await;
    tokio::task::yield_now().await;
    assert!(legacy.is_retired(), "legacy deadline is actor-owned");
    assert!(legacy
        .snapshot()
        .await
        .is_some_and(|lease| { lease.retired && lease.expiration_claimed }));

    let explicit = RollingControlHandle::spawn_for_test("session-start");
    let request = request();
    explicit
        .control(LocalControlRequest {
            session_id: "unused",
            generation: &request.generation,
            owner_node_id: "node-a",
            owner_epoch: request.control_epoch,
            client_instance_id: &request.client_instance_id,
            sequence: request.sequence,
            snapshot: PlaybackDemandSnapshot::from(&request),
            prepared_successor: PreparedSuccessorObservation::NotRequested,
        })
        .await
        .expect("explicit mode accepted");
    tokio::time::advance(ROLLING_EXPLICIT_LEASE_TIMEOUT - Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(!explicit.is_retired());
    let before_deadline = explicit.snapshot().await.expect("live explicit snapshot");
    assert_eq!(before_deadline.remaining, Duration::from_millis(1));
    let wall_before = crate::media_sessions::unix_ms();
    let advertised_expiry = before_deadline.expires_at_unix_ms();
    let wall_after = crate::media_sessions::unix_ms();
    assert!(
        advertised_expiry >= wall_before && advertised_expiry <= wall_after.saturating_add(1),
        "wire expiry uses the same paused monotonic clock as the actor"
    );
    {
        let producer_transition = explicit.lock_producer_transition();
        let deadline = producer_transition.lease_deadline;
        assert!(explicit.producer_transition_is_live_at(
            &producer_transition,
            deadline - Duration::from_millis(1)
        ));
        assert!(
            !explicit.producer_transition_is_live_at(&producer_transition, deadline),
            "a producer signal is denied at the exact deadline"
        );
    }
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(explicit.is_retired(), "explicit deadline is actor-owned");
}

#[tokio::test(start_paused = true)]
async fn snapshot_command_claims_expiry_at_the_exact_actor_deadline() {
    let deadline = rolling_now();
    let accepted_at = deadline - ROLLING_EXPLICIT_LEASE_TIMEOUT;
    let retired_fence = Arc::new(AtomicBool::new(false));
    let mut actor =
        RollingControlActor::new(accepted_at, "session-start", Arc::clone(&retired_fence));
    let accepted = actor
        .control_at(accepted_at, owned_control(&request()))
        .expect("control establishes an explicit lease");
    assert_eq!(accepted.lease.deadline, deadline);

    // Drive the Snapshot command itself at the exact deadline. This is
    // independent of timer scheduling and fails if the handler ever
    // returns a zero-remaining live lease instead of claiming expiry.
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command_for_test(RollingControlCommand::Snapshot { reply })
        .await;
    let deadline_snapshot = response.await.expect("snapshot command reply");
    assert!(deadline_snapshot.retired, "snapshot linearizes expiry");
    assert!(deadline_snapshot.expiration_claimed);
    assert!(retired_fence.load(Ordering::Acquire));
}

#[tokio::test]
async fn dropped_replies_preserve_the_shared_fence_and_committed_sequence() {
    let expiry = RollingControlHandle::spawn_for_test("session-start");
    expiry
        .set_renewal_for_test(
            Instant::now() - ROLLING_LEGACY_LEASE_TIMEOUT - Duration::from_secs(1),
            "before-expiry",
        )
        .await;
    let (reply, dropped) = tokio::sync::oneshot::channel();
    expiry
        .enqueue_command(RollingControlCommand::ClaimExpiry { reply })
        .await
        .expect("expiry command queued");
    drop(dropped);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !expiry.is_retired() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expiry publishes shared fence before its dropped reply");
    assert!(matches!(
        expiry.claim_expiry().await,
        Ok(RollingExpiryClaim::Retired(_))
    ));

    let retirement = RollingControlHandle::spawn_for_test("session-start");
    let (reply, dropped) = tokio::sync::oneshot::channel();
    retirement
        .enqueue_command(RollingControlCommand::Terminal {
            cause: RollingTerminalCause::End,
            reply,
        })
        .await
        .expect("retirement command queued");
    drop(dropped);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !retirement.is_retired() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retirement publishes shared fence before its dropped reply");
    assert_eq!(
        retirement
            .snapshot()
            .await
            .expect("committed terminal snapshot")
            .terminal,
        Some(RollingTerminalCause::End)
    );

    let control = RollingControlHandle::spawn_for_test("session-start");
    let request = request();
    let never_polled = control.control(LocalControlRequest {
        session_id: "unused",
        generation: &request.generation,
        owner_node_id: "node-a",
        owner_epoch: request.control_epoch,
        client_instance_id: &request.client_instance_id,
        sequence: request.sequence,
        snapshot: PlaybackDemandSnapshot::from(&request),
        prepared_successor: PreparedSuccessorObservation::NotRequested,
    });
    drop(never_polled);
    assert_eq!(
        control.snapshot().await.expect("snapshot").mode,
        RollingLeaseMode::Legacy,
        "cancellation before enqueue mutates nothing"
    );

    let (reply, dropped) = tokio::sync::oneshot::channel();
    drop(dropped);
    control
        .enqueue_command(RollingControlCommand::Control {
            request: Box::new(owned_control(&request)),
            deadline_unix_ms: i64::MAX,
            terminal_admission: None,
            preparation_admission: None,
            reply,
        })
        .await
        .expect("control command queued");
    tokio::time::sleep(MIN_CONTROL_INTERVAL).await;
    let replay = control
        .control(LocalControlRequest {
            session_id: "unused",
            generation: &request.generation,
            owner_node_id: "node-a",
            owner_epoch: request.control_epoch,
            client_instance_id: &request.client_instance_id,
            sequence: request.sequence,
            snapshot: PlaybackDemandSnapshot::from(&request),
            prepared_successor: PreparedSuccessorObservation::NotRequested,
        })
        .await
        .expect("request remains admissible");
    assert_eq!(replay.disposition, ControlDisposition::Accepted);

    let expired = RollingControlHandle::spawn_for_test("session-start");
    let (reply, response) = tokio::sync::oneshot::channel();
    expired
        .enqueue_command(RollingControlCommand::Control {
            request: Box::new(owned_control(&request)),
            deadline_unix_ms: crate::media_sessions::unix_ms(),
            terminal_admission: None,
            preparation_admission: None,
            reply,
        })
        .await
        .expect("expired control command queued");
    assert_eq!(
        response.await.expect("expired control reply"),
        Err(ControlStateError::Unavailable)
    );
    assert_eq!(
        expired.snapshot().await.expect("expired snapshot").mode,
        RollingLeaseMode::Legacy,
        "a queued control cannot mutate after its inherited deadline"
    );

    let terminal_control = RollingControlHandle::spawn_for_test("session-start");
    let mut end = request.clone();
    end.demand = PlaybackDemand::End;
    end.playback_rate = 0.0;
    end.render_state = RenderState::Ended;
    let (reply, dropped) = tokio::sync::oneshot::channel();
    let admission = Arc::new(DropReplyOnTerminalAdmission {
        receiver: std::sync::Mutex::new(Some(dropped)),
        accepted: AtomicUsize::new(0),
    });
    terminal_control
        .enqueue_command(RollingControlCommand::Control {
            request: Box::new(owned_control(&end)),
            deadline_unix_ms: i64::MAX,
            terminal_admission: Some(admission.clone()),
            preparation_admission: None,
            reply,
        })
        .await
        .expect("terminal control queued");
    tokio::time::timeout(Duration::from_secs(1), async {
        while !terminal_control.is_retired() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actor transfers accepted End before its reply send");
    assert_eq!(admission.accepted.load(Ordering::Acquire), 1);
    let recovered = terminal_control
        .control(LocalControlRequest {
            session_id: "unused",
            generation: &end.generation,
            owner_node_id: "node-a",
            owner_epoch: end.control_epoch,
            client_instance_id: &end.client_instance_id,
            sequence: end.sequence,
            snapshot: PlaybackDemandSnapshot::from(&end),
            prepared_successor: PreparedSuccessorObservation::NotRequested,
        })
        .await
        .expect("lost terminal response is exactly replayable");
    assert_eq!(recovered.disposition, ControlDisposition::Replay);
    assert_eq!(recovered.lease.terminal, Some(RollingTerminalCause::End));
}

#[tokio::test]
async fn mailbox_expiry_and_media_renewal_have_one_ordered_winner() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    handle
        .set_renewal_for_test(
            Instant::now() - ROLLING_LEGACY_LEASE_TIMEOUT - Duration::from_secs(1),
            "before-race",
        )
        .await;
    let renewal = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.renew_media("segment").await })
    };
    let expiry = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.claim_expiry().await })
    };
    let renewed = renewal.await.expect("renewal task");
    let claim = expiry.await.expect("expiry task").expect("actor available");
    assert!(
        !renewed,
        "a renewal after the deadline cannot revive the lease"
    );
    assert!(
        matches!(
            claim,
            RollingExpiryClaim::Claimed(_) | RollingExpiryClaim::Retired(_)
        ),
        "one command claims expiry and the other observes its fence"
    );
}

#[test]
fn relayed_exchange_inherits_remaining_budget_instead_of_restarting_it() {
    assert_eq!(
        inherited_exchange_budget(14_000, 13_000),
        Some(Duration::from_secs(1))
    );
    assert_eq!(inherited_exchange_budget(14_000, 14_000), None);
    assert_eq!(inherited_exchange_budget(14_000, 15_000), None);
    assert_eq!(
        inherited_exchange_budget(60_000, 10_000),
        Some(EXCHANGE_DEADLINE),
        "even a malformed future deadline cannot extend the public budget"
    );
}

#[test]
fn relay_answers_are_schema_bounded_and_tuple_bound() {
    let request = relay_request();
    assert!(request.is_valid());
    let response = ControlResponseV1 {
        protocol: PROTOCOL_V1.to_owned(),
        generation: request.generation.clone(),
        control_epoch: 1,
        accepted_sequence: 1,
        server_time_unix_ms: 1_000_000,
        lease: PlaybackLeaseView {
            state: "active".to_owned(),
            renew_after_ms: NEXT_EXCHANGE_MS,
            expires_at_unix_ms: 1_060_000,
        },
        delivery: DeliveryView {
            presentation: "vod".to_owned(),
            producer_state: "complete".to_owned(),
            produced_through_ms: Some(7_200_000),
            fetched_through_ms: 25_000,
            delivered_bps: None,
            delivered_idle_ms: None,
            recent_producer_speed: None,
            client_runway_ms: 15_000,
            admitted: Some(true),
            producer_decision: None,
            hold_reason: None,
            subtitle_readiness: None,
            subtitle_revision: None,
            preparation: None,
            owner_node_hash: "n-0123456789abcdef".to_owned(),
            owner_epoch: 1,
        },
        effective_selection: EffectiveSelection {
            candidate_id: None,
            quality_auto: true,
            height: 1080,
            audio_track: Some(0),
            subtitle_burn: None,
            audio_offset_ms: 0,
            codec: "source".to_owned(),
            dynamic_range: Some("sdr".to_owned()),
        },
        action: ControlAction::None,
    };
    assert!(response.is_valid_for(&request));

    let mut wrong_epoch = response.clone();
    wrong_epoch.control_epoch = 2;
    assert!(!wrong_epoch.is_valid_for(&request));
    let mut exited = response.clone();
    exited.delivery.presentation = "live-recovery".to_owned();
    exited.delivery.producer_state = "exited".to_owned();
    assert!(
        exited.is_valid_for(&request),
        "a remote owner may truthfully report an unsuccessful producer exit"
    );
    let mut invented_state = response.clone();
    invented_state.delivery.producer_state = "probably_running".to_owned();
    assert!(!invented_state.is_valid_for(&request));

    // A relayed hold is an instruction arriving from another node. The
    // relaying node forwards it only when the same response's delivery
    // reports the same hold and the client actually asked for the action —
    // otherwise a peer could manufacture an instruction out of a fact it
    // never sent, and the client would obey it.
    let mut accepting = request.clone();
    accepting.control.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
    let mut held = response.clone();
    held.delivery.hold_reason = Some("working_set".to_owned());
    held.action = ControlAction::Hold {
        reason: HoldReason::WorkingSet,
        revisit_after_ms: HoldReason::WorkingSet.revisit_after_ms(),
    };
    assert!(held.is_valid_for(&accepting));

    let mut disagreeing = held.clone();
    disagreeing.delivery.hold_reason = Some("no_room".to_owned());
    assert!(
        !disagreeing.is_valid_for(&accepting),
        "the action and the delivery fact it came from must agree"
    );

    let mut unreported = held.clone();
    unreported.delivery.hold_reason = None;
    assert!(!unreported.is_valid_for(&accepting));

    assert!(
        !held.is_valid_for(&request),
        "a passive client must never be relayed an action it cannot apply"
    );

    let mut terminal_request = request.clone();
    terminal_request.control.demand = PlaybackDemand::End;
    terminal_request.control.playback_rate = 0.0;
    terminal_request.control.render_state = RenderState::Ended;
    let mut terminal_response = exited;
    terminal_response.accepted_sequence = terminal_request.control.sequence;
    terminal_response.lease.state = "ended".to_owned();
    terminal_response.lease.expires_at_unix_ms = terminal_response.server_time_unix_ms;
    assert!(terminal_response.is_valid_for(&terminal_request));
    let mut active_terminal_response = terminal_response.clone();
    active_terminal_response.lease.state = "active".to_owned();
    assert!(!active_terminal_response.is_valid_for(&terminal_request));
    assert!(
        !terminal_response.is_valid_for(&request),
        "an ended lease is only valid for the exact terminal demand"
    );

    let mut oversized_sequence = terminal_request.clone();
    oversized_sequence.control.sequence = i64::MAX as u64 + 1;
    assert!(!oversized_sequence.is_valid());

    let unavailable = ControlErrorBody {
        terminal_reason: None,
        code: "control_unavailable".to_owned(),
        message: "owner deadline".to_owned(),
        generation: None,
        control_epoch: None,
        retry_after_ms: Some(500),
        invalid_field: None,
    };
    assert!(unavailable.is_valid_for_status(503));
    assert!(!unavailable.is_valid_for_status(409));
}

/// The control plane answers hard owner loss too (plan §10.3). It carried
/// the same defect as the media plane and for longer: 425
/// `owner_transition` with a 500 ms hint, which all three reporters treat
/// as retryable, against an owner that is gone and that no successor can
/// replace.
#[test]
fn a_lost_owner_is_a_distinct_control_answer_from_a_transition() {
    let lost = ControlErrorBody {
        terminal_reason: None,
        code: "owner_lost".to_owned(),
        message: "the node holding this media session is gone".to_owned(),
        generation: None,
        control_epoch: None,
        retry_after_ms: None,
        invalid_field: None,
    };
    assert!(
        lost.is_valid_for_status(410),
        "the code has to be admitted at its own status or the relay refuses it"
    );
    assert!(
        !lost.is_valid_for_status(425),
        "and must not be accepted at the retryable status it replaces"
    );
    let pause_expired = ControlErrorBody {
        terminal_reason: None,
        code: "pause_grace_expired".to_owned(),
        ..lost.clone()
    };
    assert!(pause_expired.is_valid_for_status(410));
    assert!(!pause_expired.is_valid_for_status(425));

    // The rollout property, pinned rather than discovered: the relay's
    // validator is a strict allowlist, so an ingress node that predates
    // this code rejects a new owner's answer and degrades it to
    // `control_unavailable` — a retry, which is exactly today's
    // behaviour. The mixed fleet is therefore never *wrong*, only
    // temporarily too soft, and it settles the moment ingress upgrades.
    let previously_valid = ["session_ended", "owner_transition"];
    for code in previously_valid {
        let body = ControlErrorBody {
            terminal_reason: None,
            code: code.to_owned(),
            ..lost.clone()
        };
        let status = if code == "session_ended" { 410 } else { 425 };
        assert!(
            body.is_valid_for_status(status),
            "{code} keeps the status it always had"
        );
    }
}

/// The shadow metric's full cross product is published from boot, and its
/// labels are the decision's own vocabulary.
///
/// Published at zero rather than on first observation so fleet rollout can
/// compare capable and incapable clients without absent series.
#[test]
fn the_shadow_decision_metric_publishes_every_axis_and_outcome() {
    let metrics = prometheus();
    for name in [
        "plurx_playback_preparation_decisions_total",
        "plurx_playback_preparation_counterfactual_total",
    ] {
        for platform in CLIENT_PLATFORM_LABELS {
            for axis in [
                PreparationAxis::ResolutionOrBitrate,
                PreparationAxis::AudioTrackOrOffset,
                PreparationAxis::SubtitleBurn,
                PreparationAxis::DeliveryMethod,
                PreparationAxis::DynamicRange,
            ] {
                for outcome in ["prepare", FallbackReason::ClientCannotPrepare.as_str()] {
                    let series = format!(
                        "{name}{{platform=\"{platform}\",axis=\"{}\",outcome=\"{outcome}\"}}",
                        axis.as_str()
                    );
                    assert!(metrics.contains(&series), "missing {series}");
                }
            }
        }
    }
    // Each family's samples are contiguous behind its own HELP/TYPE, which
    // the exposition format asks for and which an interleaved emission
    // silently broke.
    for name in [
        "plurx_playback_preparation_decisions_total",
        "plurx_playback_preparation_counterfactual_total",
    ] {
        let samples: Vec<usize> = metrics
            .lines()
            .enumerate()
            .filter(|(_, line)| line.starts_with(&format!("{name}{{")))
            .map(|(index, _)| index)
            .collect();
        // Every platform × five axes × two outcomes.
        assert_eq!(
            samples.len(),
            CLIENT_PLATFORM_LABELS.len() * 5 * 2,
            "{name}"
        );
        assert_eq!(
            samples.last().expect("samples") - samples[0],
            samples.len() - 1,
            "{name} samples are not contiguous",
        );
        let help = metrics
            .lines()
            .position(|line| line.starts_with(&format!("# HELP {name} ")))
            .expect("help");
        assert!(help < samples[0], "{name} HELP follows its samples");
    }
}

#[test]
fn suppressed_action_help_describes_every_action_transaction() {
    assert!(prometheus().contains(
        "# HELP plurx_playback_control_actions_suppressed_total Exchanges where an action transaction was available but the client had not declared its vocabulary, so it was told nothing."
    ));
}

/// `Unchanged` is not an outcome, and recording it would put a bar
/// labelled "nothing to do" beside four that mean something.
///
/// It is reachable: the actor gates on the *selection*, and a selection
/// can move on an axis `EffectiveSelection` cannot see — a seek, a
/// different file. The decision then correctly answers `Unchanged`, and
/// the right thing to do with that is nothing.
#[test]
fn an_unchanged_decision_is_not_recorded() {
    // One cell, owned by this test alone. The counters are process-global
    // and `cargo test` runs tests in parallel, so a family sum is a race
    // with every other test that records anything.
    let cell = |metrics: &str| {
        series_value(
            metrics,
            "plurx_playback_preparation_decisions_total",
            "android",
            PreparationAxis::DynamicRange,
            FallbackReason::ClientCannotPrepare.as_str(),
        )
    };
    let before = cell(&prometheus());
    record_preparation_decision(ClientPlatform::Android, PreparationDecision::Unchanged);
    assert_eq!(before, cell(&prometheus()));

    record_preparation_decision(
        ClientPlatform::Android,
        PreparationDecision::Fallback {
            axis: PreparationAxis::DynamicRange,
            reason: FallbackReason::ClientCannotPrepare,
        },
    );
    assert_eq!(cell(&prometheus()), before + 1, "and a real decision is");
}

/// Read one `[platform][axis][outcome]` series out of the exposition.
///
/// Deliberately not a family sum: these counters are process-global and
/// the test binary is parallel, so any two tests sharing a total race.
/// Each test owns one cell instead.
fn series_value(
    metrics: &str,
    name: &str,
    platform: &str,
    axis: PreparationAxis,
    outcome: &str,
) -> u64 {
    let series = format!(
        "{name}{{platform=\"{platform}\",axis=\"{}\",outcome=\"{outcome}\"}} ",
        axis.as_str()
    );
    metrics
        .lines()
        .find_map(|line| line.strip_prefix(&series))
        .unwrap_or_else(|| panic!("missing {series}"))
        .parse()
        .expect("counter value")
}

/// The counterfactual removes only the explicit client-capability gate.
#[test]
fn the_counterfactual_reaches_the_rules_the_client_gate_hides() {
    // A link with no room for a second pipeline: the floor wants twice
    // what is being delivered.
    let strained = PreparationConditions {
        observed_download_bps: Some(12_000_000),
        delivered_bps: Some(12_000_000),
    };
    assert_eq!(
        decide_preparation(view(&playing(2160)), view(&playing(1080)), None, strained),
        PreparationDecision::Fallback {
            axis: PreparationAxis::ResolutionOrBitrate,
            reason: FallbackReason::ClientCannotPrepare,
        },
        "the retained client capability is the only refusal",
    );
    assert_eq!(
        decide_preparation_after_client_release(
            view(&playing(2160)),
            view(&playing(1080)),
            strained,
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::ResolutionOrBitrate,
        },
        "link telemetry is advisory",
    );

    let mut audio = playing(2160);
    audio.audio_track = Some(1);
    assert_eq!(
        decide_preparation(view(&playing(2160)), view(&audio), None, roomy()),
        PreparationDecision::Fallback {
            axis: PreparationAxis::AudioTrackOrOffset,
            reason: FallbackReason::ClientCannotPrepare,
        },
    );
    assert_eq!(
        decide_preparation_after_client_release(view(&playing(2160)), view(&audio), roomy()),
        PreparationDecision::Prepare {
            axis: PreparationAxis::AudioTrackOrOffset,
        },
        "axis receipts are advisory",
    );

    assert_eq!(
        decide_preparation_after_client_release(
            view(&playing(2160)),
            view(&playing(1080)),
            roomy(),
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::ResolutionOrBitrate,
        },
        "and this is the volume a client release would actually unlock",
    );
}

/// The two decisions differ at the client gate and nowhere else.
#[test]
fn the_counterfactual_differs_only_at_the_client_gate() {
    let mut two_axes = playing(1080);
    two_axes.audio_track = Some(1);
    for conditions in [
        roomy(),
        PreparationConditions {
            observed_download_bps: Some(12_000_000),
            delivered_bps: Some(12_000_000),
        },
    ] {
        assert!(matches!(
            decide_preparation(view(&playing(2160)), view(&two_axes), None, conditions),
            PreparationDecision::Fallback {
                reason: FallbackReason::ClientCannotPrepare,
                ..
            }
        ));
        assert!(matches!(
            decide_preparation_after_client_release(
                view(&playing(2160)),
                view(&two_axes),
                conditions
            ),
            PreparationDecision::Prepare { .. }
        ));
        assert_eq!(
            decide_preparation_after_client_release(
                view(&playing(2160)),
                view(&playing(2160)),
                conditions,
            ),
            PreparationDecision::Unchanged,
        );
    }
}

/// Each counter is moved by its own recorder and by nothing else.
#[test]
fn the_two_preparation_counters_are_recorded_separately() {
    // One cell of each family, owned by this test alone — see
    // `series_value` on why a family sum would race.
    let decision = PreparationDecision::Fallback {
        axis: PreparationAxis::SubtitleBurn,
        reason: FallbackReason::ClientCannotPrepare,
    };
    let cell = |metrics: &str, name: &str| {
        series_value(
            metrics,
            name,
            "web",
            PreparationAxis::SubtitleBurn,
            FallbackReason::ClientCannotPrepare.as_str(),
        )
    };
    const DECISIONS: &str = "plurx_playback_preparation_decisions_total";
    const COUNTERFACTUAL: &str = "plurx_playback_preparation_counterfactual_total";

    let before = prometheus();
    record_preparation_counterfactual(ClientPlatform::Web, decision);
    let after = prometheus();
    assert_eq!(
        cell(&after, DECISIONS),
        cell(&before, DECISIONS),
        "the counterfactual does not move the decision counter",
    );
    assert_eq!(
        cell(&after, COUNTERFACTUAL),
        cell(&before, COUNTERFACTUAL) + 1,
    );

    record_preparation_decision(ClientPlatform::Web, decision);
    assert_eq!(
        cell(&prometheus(), COUNTERFACTUAL),
        cell(&after, COUNTERFACTUAL),
        "and the decision does not move the counterfactual",
    );
    record_preparation_counterfactual(ClientPlatform::Web, PreparationDecision::Unchanged);
    assert_eq!(
        cell(&prometheus(), COUNTERFACTUAL),
        cell(&after, COUNTERFACTUAL),
        "`Unchanged` is not an outcome in either counter",
    );
    // And the platform is a real dimension, not a constant: the same
    // decision on another platform lands in another cell.
    let apple_before = series_value(
        &prometheus(),
        COUNTERFACTUAL,
        "apple",
        PreparationAxis::SubtitleBurn,
        FallbackReason::ClientCannotPrepare.as_str(),
    );
    record_preparation_counterfactual(ClientPlatform::Apple, decision);
    assert_eq!(
        series_value(
            &prometheus(),
            COUNTERFACTUAL,
            "apple",
            PreparationAxis::SubtitleBurn,
            FallbackReason::ClientCannotPrepare.as_str(),
        ),
        apple_before + 1,
    );
}

#[test]
fn metrics_expose_bounded_platform_and_relay_dimensions() {
    let metrics = prometheus();
    assert!(metrics.contains(
        "plurx_playback_control_platform_exchanges_total{outcome=\"accepted\",platform=\"web\"}"
    ));
    // A withheld hold is neither sent nor suppressed, so it needs its own
    // series or it would read as holds simply having stopped happening.
    for reason in [
        "demand",
        "time",
        "bytes",
        "global",
        "ahead",
        "working_set",
        "no_room",
    ] {
        for platform in CLIENT_PLATFORM_LABELS {
            assert!(
                metrics.contains(&format!(
                    "plurx_playback_control_recovery_withheld_total{{reason=\"{reason}\",platform=\"{platform}\"}}"
                )),
                "{reason}/{platform} has no withheld series",
            );
        }
    }
    assert!(metrics.contains("plurx_playback_control_relays_total{outcome=\"invalid_response\"}"));
    assert!(metrics.contains("# TYPE plurx_playback_control_relay_seconds histogram"));
    assert!(metrics.contains("plurx_playback_rolling_lease_renewals_total{source=\"control\"}"));
    assert!(metrics.contains("plurx_playback_rolling_lease_expirations_total"));
    assert!(metrics.contains(
        "plurx_playback_rolling_terminal_events_total{event=\"authority_fence\",outcome=\"already_terminal\"}"
    ));
    assert!(metrics.contains(
        "plurx_playback_rolling_producer_transitions_total{transition=\"hold\",reason=\"demand\"}"
    ));
    assert!(
        metrics.contains("plurx_playback_rolling_producer_event_ingress_total{event=\"progress\"}")
    );
    assert!(
        metrics.contains("plurx_playback_rolling_producer_event_coalesced_total{event=\"exit\"}")
    );
    assert!(metrics.contains(
        "plurx_playback_rolling_producer_event_outcomes_total{event=\"exit\",outcome=\"rejected\"}"
    ));
    assert!(metrics.contains("plurx_playback_rolling_producer_flow_deferrals_total"));
    assert!(metrics.contains(
        "plurx_playback_rolling_producer_deadline_observations_total{mode=\"advancing\"}"
    ));
    assert!(metrics.contains(
        "plurx_playback_rolling_producer_exit_classifications_total{outcome=\"partial_success\"}"
    ));
    assert!(metrics.contains("plurx_playback_rolling_control_commands_total{kind=\"snapshot\"}"));
    assert!(metrics.contains(
        "plurx_playback_rolling_control_commands_total{kind=\"classify_producer_exit\"}"
    ));
}

async fn activate_route(
    store: &plurx_core::store::SqliteStore,
    now_ms: i64,
    lease_expires_at_ms: i64,
) -> (String, String) {
    activate_route_with_recipe(store, now_ms, lease_expires_at_ms, None).await
}

/// `recipe` decides, for an expired route, whether a successor could ever
/// take it over — and so which of the two owner-transition answers the
/// authority check returns. `None` is the `{}` recipe every other test
/// here uses, which no takeover can act on.
async fn activate_route_with_recipe(
    store: &plurx_core::store::SqliteStore,
    now_ms: i64,
    lease_expires_at_ms: i64,
    recipe: Option<&dyn Fn(&str) -> String>,
) -> (String, String) {
    use plurx_core::store::MediaSessionStore as _;

    let incarnation = uuid::Uuid::new_v4().to_string();
    let session = uuid::Uuid::new_v4().to_string();
    let fingerprint = "a".repeat(64);
    store
        .claim_media_session_request(
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
            &incarnation,
            &fingerprint,
            "player-a",
            &incarnation,
            now_ms,
            now_ms.saturating_add(60_000),
        )
        .await
        .expect("claim route");
    assert!(store
        .assign_media_session_request_owner(
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
            &incarnation,
            &incarnation,
            "node-a",
            now_ms,
        )
        .await
        .expect("assign owner"));
    let activation = plurx_core::domain::MediaSessionActivation {
        recovery_epoch: String::new(),
        expected_desired_revision: None,
        incarnation_id: incarnation.clone(),
        session_id: session.clone(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        playback_id: "player-a".to_owned(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: false,
        request_id: Some(incarnation.clone()),
        request_fingerprint: fingerprint,
        owner_node_id: "node-a".to_owned(),
        recipe_json: recipe.map_or_else(|| "{}".to_owned(), |build| build(&incarnation)),
        response_json: "{}".to_owned(),
        publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms,
        lease_expires_at_ms,
    };
    store
        .activate_media_session(&activation)
        .await
        .expect("activate route")
        .expect("activation accepted");
    store
        .settle_media_session_activation(
            &activation,
            plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: 0,
            },
            now_ms,
        )
        .await
        .expect("confirm route")
        .expect("route confirmed");
    (incarnation, session)
}

#[tokio::test]
async fn durable_authority_fences_generation_owner_expiry_and_end() {
    use plurx_core::store::{MediaSessionStore as _, SqliteStore};

    let store = SqliteStore::open_in_memory().expect("store");
    let now = crate::media_sessions::unix_ms();
    let (generation, session) = activate_route(&store, now, now + 60_000).await;
    assert_eq!(
        verify_authority(&store, &session, &generation, "node-a", 1).await,
        Ok(())
    );
    assert_eq!(
        verify_authority(
            &store,
            &session,
            &uuid::Uuid::new_v4().to_string(),
            "node-a",
            1
        )
        .await,
        Err(ControlStateError::StaleGeneration)
    );
    assert_eq!(
        verify_authority(&store, &session, &generation, "node-b", 1).await,
        Err(ControlStateError::OwnerChanged)
    );
    assert_eq!(
        verify_authority(&store, &session, &generation, "node-a", 2).await,
        Err(ControlStateError::OwnerChanged)
    );

    store
        .end_media_session(&session, "deleted", now + 1)
        .await
        .expect("end route")
        .expect("route existed");
    assert_eq!(
        verify_authority(&store, &session, &generation, "node-a", 1).await,
        Err(ControlStateError::SessionEnded)
    );

    // An expired lease is an owner transition only while a successor could
    // still answer. This route's `{}` recipe is one no takeover can act
    // on, so nothing is coming and the answer says so rather than asking
    // for a retry the client would repeat forever.
    let expired_store = SqliteStore::open_in_memory().expect("expired store");
    let (expired_generation, expired_session) = activate_route(&expired_store, 1, 2).await;
    assert_eq!(
        verify_authority(
            &expired_store,
            &expired_session,
            &expired_generation,
            "node-a",
            1,
        )
        .await,
        Err(ControlStateError::OwnerLost)
    );

    // The same expiry with a recipe the takeover path accepts stays the
    // retryable answer it has always been.
    let eligible_store = SqliteStore::open_in_memory().expect("eligible store");
    let build = |incarnation: &str| {
        let route = crate::media_sessions::takeover_eligible_route(
            &uuid::Uuid::new_v4().to_string(),
            incarnation,
        );
        route.recipe_json
    };
    let (eligible_generation, eligible_session) =
        activate_route_with_recipe(&eligible_store, 1, 2, Some(&build)).await;
    assert_eq!(
        verify_authority(
            &eligible_store,
            &eligible_session,
            &eligible_generation,
            "node-a",
            1,
        )
        .await,
        Err(ControlStateError::OwnerTransition),
        "a rolling session a survivor can still claim keeps its retry"
    );
}

fn playing(height: i64) -> EffectiveSelection {
    EffectiveSelection {
        candidate_id: None,
        quality_auto: false,
        height,
        audio_track: Some(0),
        subtitle_burn: None,
        audio_offset_ms: 0,
        codec: "server_selected".to_owned(),
        dynamic_range: Some("hdr10".to_owned()),
    }
}

/// A recipe's grade intent. `sdr()` is the default both sides use, so a
/// test that does not mention the grade is testing a transition that does
/// not cross it.
fn sdr() -> GradeIntent {
    GradeIntent {
        hdr10: false,
        preserve_dolby_vision: false,
        convert_dolby_vision: false,
    }
}

fn hdr10() -> GradeIntent {
    GradeIntent {
        hdr10: true,
        ..sdr()
    }
}

fn view(selection: &EffectiveSelection) -> RecipeView<'_> {
    RecipeView {
        selection,
        grade: sdr(),
    }
}

fn view_at(selection: &EffectiveSelection, grade: GradeIntent) -> RecipeView<'_> {
    RecipeView { selection, grade }
}

fn can_prepare(dual_player_preparation: bool) -> DynamicCapabilities {
    DynamicCapabilities {
        presentation_target: None,
        decoder_caps: None,
        platform: ClientPlatform::Apple,
        max_height: 2160,
        codecs: vec![CodecPolicy::H264, CodecPolicy::Hevc],
        dynamic_ranges: vec![DynamicRangePolicy::Sdr, DynamicRangePolicy::Hdr10],
        dual_player_preparation,
    }
}

/// Representative advisory telemetry. Admission deliberately ignores it.
fn roomy() -> PreparationConditions {
    PreparationConditions {
        observed_download_bps: Some(80_000_000),
        delivered_bps: Some(12_000_000),
    }
}

/// The axis M6 prepares across on a client that says it can, in **both**
/// directions. Upshift is the direction that raises demand, so exercising
/// both makes an accidental direction veto visible.
#[test]
fn a_resolution_change_on_a_capable_client_is_prepared() {
    for (from, to) in [(2160, 1080), (1080, 2160)] {
        assert_eq!(
            decide_preparation(
                view(&playing(from)),
                view(&playing(to)),
                Some(&can_prepare(true)),
                roomy(),
            ),
            PreparationDecision::Prepare {
                axis: PreparationAxis::ResolutionOrBitrate,
            },
            "{from} -> {to}",
        );
    }
    // Auto ↔ manual at the same delivered height is still a change: the
    // ladder the successor climbs differs even when the first rung matches.
    let mut auto = playing(1080);
    auto.quality_auto = true;
    assert_eq!(
        decide_preparation(
            view(&playing(1080)),
            view(&auto),
            Some(&can_prepare(true)),
            roomy(),
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::ResolutionOrBitrate,
        },
    );
}

/// The capability is read, never re-derived. This is the test that fails
/// if someone infers "Apple can prepare" from the platform enum — the
/// mistake M5.5 exists because of.
#[test]
fn a_client_that_did_not_say_it_can_prepare_gets_the_fallback() {
    assert_eq!(
        decide_preparation(
            view(&playing(2160)),
            view(&playing(1080)),
            Some(&can_prepare(false)),
            roomy(),
        ),
        PreparationDecision::Fallback {
            axis: PreparationAxis::ResolutionOrBitrate,
            reason: FallbackReason::ClientCannotPrepare,
        },
        "the platform is Apple and Apple measured true; the literal still says false",
    );
    assert_eq!(
        decide_preparation(view(&playing(2160)), view(&playing(1080)), None, roomy()),
        PreparationDecision::Fallback {
            axis: PreparationAxis::ResolutionOrBitrate,
            reason: FallbackReason::ClientCannotPrepare,
        },
        "a session that has never been told has not been told",
    );
    // And the capability outranks the axis: an incapable client crossing
    // any axis is reported as incapable, because that is the only fact
    // that would change if the literal flipped.
    let playing = playing(2160);
    assert_eq!(
        decide_preparation(
            view(&playing),
            view_at(&playing, hdr10()),
            Some(&can_prepare(false)),
            roomy(),
        ),
        PreparationDecision::Fallback {
            axis: PreparationAxis::DynamicRange,
            reason: FallbackReason::ClientCannotPrepare,
        },
    );
}

/// The measured product: height and delivery method moving together.
///
/// This is the transition a viewer's quality change actually makes — the
/// top rung direct-plays and the lower rungs transcode, so the delivery
/// method moves with the height every time, and shadow mode measured that
/// a pure resolution change does not occur on a real library at all.
///
/// Earlier Apple TV observations remain advisory evidence about resource
/// cost, not permission to admit this viewer request. The client capability
/// and the real reserve/prime outcomes are the enforceable boundaries.
#[test]
fn resolution_together_with_delivery_method_is_prepared() {
    let caps = can_prepare(true);
    let mut transcoded = playing(1080);
    transcoded.codec = "server_selected".to_owned();
    let mut direct = playing(2160);
    direct.codec = "source".to_owned();

    assert_eq!(
        decide_preparation(view(&direct), view(&transcoded), Some(&caps), roomy()),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DeliveryMethod,
        },
        "the axis label stays the hardest member, so the metric is comparable",
    );
    // Link telemetry is advisory and cannot veto the viewer's request.
    assert_eq!(
        decide_preparation(
            view(&direct),
            view(&transcoded),
            Some(&caps),
            PreparationConditions {
                observed_download_bps: Some(12_000_000),
                delivered_bps: Some(12_000_000),
            },
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DeliveryMethod,
        },
        "low observed throughput is advisory",
    );
    // So does the capability — and this is the one transition where the
    // two counters now disagree, which is the whole reason the
    // counterfactual exists. Before the pair was admitted both read
    // `multiple_axes`; now the production decision reads
    // `client_cannot_prepare` and the counterfactual reads `prepare`, and
    // that difference is the volume a client release would unlock.
    assert_eq!(
        decide_preparation(
            view(&direct),
            view(&transcoded),
            Some(&can_prepare(false)),
            roomy(),
        ),
        PreparationDecision::Fallback {
            axis: PreparationAxis::DeliveryMethod,
            reason: FallbackReason::ClientCannotPrepare,
        },
    );
    assert_eq!(
        decide_preparation_after_client_release(view(&direct), view(&transcoded), roomy()),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DeliveryMethod,
        },
        "the counterfactual is what reports the unlocked volume",
    );
}

/// Replanning back to Original is admitted like every explicit change.
#[test]
fn raising_quality_back_to_a_direct_playing_source_is_prepared() {
    let caps = can_prepare(true);
    let mut direct = playing(2160);
    direct.codec = "source".to_owned();
    let mut transcoded = playing(1080);
    transcoded.codec = "server_selected".to_owned();

    assert_eq!(
        decide_preparation(view(&transcoded), view(&direct), Some(&caps), roomy()),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DeliveryMethod,
        },
        "the ordinary planner owns the new recipe",
    );
    // The rule is about the successor's method, not about going up: a
    // higher rung that is still server-selected stays admitted.
    let mut higher_rung = playing(1440);
    higher_rung.codec = "server_selected".to_owned();
    assert_eq!(
        decide_preparation(view(&direct), view(&higher_rung), Some(&caps), roomy()),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DeliveryMethod,
        },
    );
    // And a pure resolution change never touches the method, so the
    // direction rule cannot reach it in either direction.
    assert_eq!(
        decide_preparation(
            view(&playing(1080)),
            view(&playing(2160)),
            Some(&caps),
            roomy()
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::ResolutionOrBitrate,
        },
    );
}

/// The grade was not in the measured product, and adding it is not implied.
///
/// The 2026-09-03 run was SDR H.264 throughout. An HDR or Dolby Vision
/// title's quality change crosses resolution, delivery method **and**
/// grade, and nobody has run that. It stays `multiple_axes`, which is what
/// stops a passing SDR receipt from being read as permission for the
/// harder case.
#[test]
fn adding_the_grade_to_the_measured_product_is_not_admitted() {
    let caps = can_prepare(true);
    let mut direct = playing(2160);
    direct.codec = "source".to_owned();
    let mut transcoded = playing(1080);
    transcoded.codec = "server_selected".to_owned();

    assert_eq!(
        decide_preparation(
            view(&direct),
            view_at(&transcoded, hdr10()),
            Some(&caps),
            roomy(),
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DynamicRange,
        },
        "compound grade changes are one successor recipe",
    );
    // Nor does the pair admit some *other* pair that happens to be two.
    let mut audio = playing(1080);
    audio.codec = "server_selected".to_owned();
    audio.audio_track = Some(1);
    assert_eq!(
        decide_preparation(view(&direct), view(&audio), Some(&caps), roomy()),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DeliveryMethod,
        },
        "compound audio changes are one successor recipe",
    );
}

/// Every axis is prepared on a capable client, and names its hardest move.
#[test]
fn every_axis_is_prepared_and_names_itself() {
    let caps = can_prepare(true);
    let mut method = playing(2160);
    method.codec = "source".to_owned();
    let mut audio = playing(2160);
    audio.audio_track = Some(1);
    let mut offset = playing(2160);
    offset.audio_offset_ms = 250;
    let mut burn = playing(2160);
    burn.subtitle_burn = Some(3);
    // Each Dolby Vision answer is its own crossing: preserving decides
    // whether the RPUs survive the filter and converting decides whether
    // they are rewritten, and a viewer can see the difference.
    let preserve = GradeIntent {
        preserve_dolby_vision: true,
        ..sdr()
    };
    let convert = GradeIntent {
        preserve_dolby_vision: true,
        convert_dolby_vision: true,
        ..sdr()
    };

    for (candidate, grade, axis) in [
        (&method, sdr(), PreparationAxis::DeliveryMethod),
        (&playing(2160), hdr10(), PreparationAxis::DynamicRange),
        (&playing(2160), preserve, PreparationAxis::DynamicRange),
        (&playing(2160), convert, PreparationAxis::DynamicRange),
        (&audio, sdr(), PreparationAxis::AudioTrackOrOffset),
        (&offset, sdr(), PreparationAxis::AudioTrackOrOffset),
        (&burn, sdr(), PreparationAxis::SubtitleBurn),
    ] {
        assert_eq!(
            decide_preparation(
                view(&playing(2160)),
                view_at(candidate, grade),
                Some(&caps),
                roomy(),
            ),
            PreparationDecision::Prepare { axis },
            "{}",
            axis.as_str(),
        );
    }
}

fn session_request(kind: SessionKind) -> crate::transcode::SessionRequest {
    crate::transcode::SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        file_id: 5615,
        playback_id: "player-a".to_owned(),
        request_id: None,
        control_sequence: None,
        automatic: false,
        previous_session_id: None,
        reopen_reason: None,
        kind,
        start_seconds: 12.5,
        audio_index: Some(0),
        audio_delivery: None,
        audio_claim: None,
        subtitle_burn: None,
        audio_offset_ms: 0,
        hdr10: false,
        presentation: crate::transcode::Presentation::Vod,
        block_budget_secs: None,
        transport: None,
    }
}

fn converting_copy() -> crate::transcode::SessionRequest {
    session_request(SessionKind::Copy {
        aac: false,
        preserve_dolby_vision: true,
        convert_dolby_vision: true,
        dual_layer_as_copied: false,
    })
}

/// The normalized selection loses nothing the wire struct distinguishes.
///
/// `observe` now answers "did the viewer ask for something else" by
/// comparing normalized selections rather than wire structs, so that claim
/// has to be proved rather than asserted in a comment: if the conversion
/// collapsed any two distinct legal asks, a real change would be reported
/// as unchanged and the successor it should have staged would never be
/// staged at all.
///
/// Proved by injectivity over every legal shape rather than by a handful
/// of examples. The cross product below is exhaustive per axis — three
/// quality policies including Original beside a manual pick, every codec,
/// every dynamic range, the default audio track beside two named ones, a
/// zero and two signed offsets, and every subtitle mode with each of two
/// track numbers — and every member is validated first, so the claim is
/// scoped to exactly the selections this server accepts. Distinct wire
/// selections must produce distinct normalized ones, and distinct digests;
/// the converse is free, because the conversion is a function.
#[test]
fn the_normalized_selection_distinguishes_every_ask_the_wire_does() {
    let qualities = [
        QualitySelection::Auto {
            height: None,
            candidate_id: None,
        },
        QualitySelection::Original,
        QualitySelection::Manual { height: 720 },
    ];
    let codecs = [
        CodecPolicy::Auto,
        CodecPolicy::H264,
        CodecPolicy::Hevc,
        CodecPolicy::Av1,
    ];
    let ranges = [
        DynamicRangePolicy::Auto,
        DynamicRangePolicy::DolbyVision,
        DynamicRangePolicy::Hdr10,
        DynamicRangePolicy::Hlg,
        DynamicRangePolicy::Sdr,
    ];
    let audio_tracks = [None, Some(0), Some(2)];
    let offsets = [0, -250, 1_000];
    let subtitles = {
        let mut shapes = vec![SubtitleSelection {
            mode: SubtitleMode::Off,
            track: None,
        }];
        for mode in [
            SubtitleMode::Native,
            SubtitleMode::Overlay,
            SubtitleMode::Burn,
        ] {
            for track in [0, 3] {
                shapes.push(SubtitleSelection {
                    mode,
                    track: Some(track),
                });
            }
        }
        shapes
    };

    let mut wire = Vec::new();
    for quality in qualities {
        for codec in codecs {
            for dynamic_range in ranges {
                for audio_track in audio_tracks {
                    for audio_offset_ms in offsets {
                        for subtitle in &subtitles {
                            wire.push(ClientSelection {
                                quality,
                                audio_track,
                                subtitle: subtitle.clone(),
                                audio_offset_ms,
                                codec,
                                dynamic_range,
                            });
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        wire.len(),
        qualities.len()
            * codecs.len()
            * ranges.len()
            * audio_tracks.len()
            * offsets.len()
            * subtitles.len()
    );

    let mut normalized = std::collections::BTreeSet::new();
    let mut digests = std::collections::BTreeSet::new();
    for selection in &wire {
        selection
            .validate()
            .expect("every shape in the cross product is one the server accepts");
        let desired = selection.desired();
        assert!(
            normalized.insert(desired.canonical_form()),
            "two distinct asks normalized to the same policy: {selection:?}"
        );
        assert!(
            digests.insert(desired.digest()),
            "two distinct policies digested the same: {selection:?}"
        );
    }
    assert_eq!(normalized.len(), wire.len());
    assert_eq!(digests.len(), wire.len());
}

fn selection_at(quality: QualitySelection) -> ClientSelection {
    ClientSelection {
        quality,
        audio_track: Some(0),
        subtitle: SubtitleSelection {
            mode: SubtitleMode::Off,
            track: None,
        },
        audio_offset_ms: 0,
        codec: CodecPolicy::Auto,
        dynamic_range: DynamicRangePolicy::Auto,
    }
}

/// The property that ruled the candidate's shape: a converting Profile 7
/// session, with the client changing nothing, must read `Unchanged`.
///
/// It fails on both obvious implementations. A candidate round-tripped
/// through a `CreateSession` loses `convert_dolby_vision` — `into_request`
/// hardcodes it false, because a client cannot ask to be handed a
/// conversion — so the candidate's `GradeIntent` differs from the
/// delivered one on that field alone, and every exchange reports a grade
/// crossing for a viewer who changed nothing. Dolby Vision titles would
/// simply never prepare, and the metric would look entirely plausible.
#[test]
fn a_converting_session_that_changed_nothing_is_unchanged() {
    let current = converting_copy();
    let candidate = candidate_request(
        &current,
        &selection_at(QualitySelection::Manual { height: 2160 }),
        2160,
        Some(2160),
    );
    assert_eq!(
        candidate.kind, current.kind,
        "a source-height ask on a copy is that copy, asked for by name",
    );
    assert_eq!(
        GradeIntent::from_request(&candidate),
        GradeIntent::from_request(&current),
        "the conversion must survive the round trip, or DV never prepares",
    );

    let delivered = EffectiveSelection::from_request(&current, 2160, Some("dolby_vision".into()));
    let proposed = EffectiveSelection::from_request(&candidate, 2160, None);
    assert_eq!(
        decide_preparation(
            RecipeView {
                selection: &delivered,
                grade: GradeIntent::from_request(&current),
            },
            RecipeView {
                selection: &proposed,
                grade: GradeIntent::from_request(&candidate),
            },
            Some(&can_prepare(true)),
            roomy(),
        ),
        PreparationDecision::Unchanged,
    );
}

#[test]
fn original_keeps_a_copy_and_revokes_automatic_authority() {
    let current = converting_copy();
    let candidate = candidate_request(
        &current,
        &selection_at(QualitySelection::Original),
        2160,
        None,
    );
    assert_eq!(candidate.kind, current.kind);
    assert!(!candidate.automatic);
}

/// A copy has no rung, so a manual rung that is not the source's own is a
/// request to stop copying — and the candidate must say so rather than
/// silently keep copying. The decision then refuses it, which is the
/// point: the candidate reports what was asked for, and the decision is
/// what says no.
///
/// It is refused as a **grade** change, not a delivery-method one, and
/// that is not an accident of this fixture: leaving a Dolby Vision copy
/// *always* crosses the grade axis, because a transcode carries no RPUs at
/// all — `GradeIntent::from_request` answers both DV questions `false` for
/// a transcode as a statement about its output. So the hardest axis names
/// it, and for a DV title that is `dynamic_range`.
#[test]
fn a_rung_a_copy_cannot_serve_becomes_a_transcode() {
    let current = converting_copy();
    let candidate = candidate_request(
        &current,
        &selection_at(QualitySelection::Manual { height: 1080 }),
        1080,
        Some(2160),
    );
    assert_eq!(candidate.kind, SessionKind::Transcode { height: 1080 });

    let delivered = EffectiveSelection::from_request(&current, 2160, None);
    let proposed = EffectiveSelection::from_request(&candidate, 1080, None);
    assert_eq!(
        decide_preparation(
            RecipeView {
                selection: &delivered,
                grade: GradeIntent::from_request(&current),
            },
            RecipeView {
                selection: &proposed,
                grade: GradeIntent::from_request(&candidate),
            },
            Some(&can_prepare(true)),
            roomy(),
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DynamicRange,
        },
        "method, height and grade all move; the hardest names it",
    );
    // A copy with no Dolby Vision to lose crosses method and height only.
    // That pair is the set the 2026-09-03 hardware run admitted, so it
    // prepares, and the delivery method is the hardest axis moving.
    let plain = session_request(SessionKind::Copy {
        aac: false,
        preserve_dolby_vision: false,
        convert_dolby_vision: false,
        dual_layer_as_copied: false,
    });
    let plain_candidate = candidate_request(
        &plain,
        &selection_at(QualitySelection::Manual { height: 1080 }),
        1080,
        Some(2160),
    );
    let plain_delivered = EffectiveSelection::from_request(&plain, 2160, None);
    let plain_proposed = EffectiveSelection::from_request(&plain_candidate, 1080, None);
    assert_eq!(
        decide_preparation(
            RecipeView {
                selection: &plain_delivered,
                grade: GradeIntent::from_request(&plain),
            },
            RecipeView {
                selection: &plain_proposed,
                grade: GradeIntent::from_request(&plain_candidate),
            },
            Some(&can_prepare(true)),
            roomy(),
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DeliveryMethod,
        },
    );
}

/// The comparison that decides it is against the **source** height, never
/// against the resolved one. A 1080 ask on a 2160 source resolves to 1080
/// — it is already a ladder rung, so the snap is the identity — and
/// comparing the ask to the result would find them equal and leave a copy
/// copying a rung it cannot serve.
#[test]
fn the_copy_escape_is_the_source_height_not_the_resolved_one() {
    let current = converting_copy();
    let resolved_equals_ask = candidate_request(
        &current,
        &selection_at(QualitySelection::Manual { height: 1080 }),
        1080,
        Some(2160),
    );
    assert_eq!(
        resolved_equals_ask.kind,
        SessionKind::Transcode { height: 1080 },
        "ask == resolved height is not the escape; ask == source height is",
    );
}

/// Auto never changes the delivery method. It asks the server to choose a
/// rung, not to stop copying — turning a direct play into a transcode
/// because the viewer selected Auto would be a downgrade nobody asked for.
#[test]
fn auto_leaves_a_copy_copying() {
    let current = converting_copy();
    let candidate = candidate_request(
        &current,
        &selection_at(QualitySelection::Auto {
            height: None,
            candidate_id: None,
        }),
        1080,
        Some(2160),
    );
    assert_eq!(candidate.kind, current.kind);
    assert!(candidate.automatic, "and it is Auto now");
}

/// D3-a. An Auto controller that moves rung has to be able to SAY so, and
/// a client that says nothing must read exactly as it always has.
///
/// The second half is the one with teeth. Every session alive when this
/// deploys is sending plain Auto against a digest taken from the old
/// canonical string; if `Auto { height: None }` rendered as anything but
/// `auto`, every one of them would record a selection change on its first
/// exchange after the upgrade and stage a successor nobody asked for.
#[test]
fn quality_selection_auto_carries_an_optional_rung() {
    use plurx_core::playback::DesiredQuality;

    let plain = selection_at(QualitySelection::Auto {
        height: None,
        candidate_id: None,
    });
    let named = selection_at(QualitySelection::Auto {
        height: Some(1080),
        candidate_id: None,
    });

    assert_eq!(
        plain.desired().quality,
        DesiredQuality::Auto {
            height: None,
            candidate_id: None
        },
    );
    assert_eq!(
        named.desired().quality,
        DesiredQuality::Auto {
            height: Some(1080),
            candidate_id: None
        },
        "the rung has to reach the desired selection, or it cannot reach the digest",
    );

    // The literal, not a round trip: a round trip would agree with itself
    // whatever this renders.
    assert!(
        plain
            .desired()
            .canonical_form()
            .starts_with("v1;quality=auto;"),
        "plain Auto must render the string every stored digest was taken from",
    );
    assert_eq!(
        plain.desired().digest(),
        selection_at(QualitySelection::Auto {
            height: None,
            candidate_id: None
        })
        .desired()
        .digest(),
    );
    assert_ne!(
        plain.desired().digest(),
        named.desired().digest(),
        "an Auto controller moving 720 -> 1080 must read as a selection change",
    );
    assert_ne!(
        named.desired().digest(),
        selection_at(QualitySelection::Auto {
            height: Some(720),
            candidate_id: None
        })
        .desired()
        .digest(),
    );
    // And a named Auto rung is still not the same ask as naming it by hand.
    assert_ne!(
        named.desired().digest(),
        selection_at(QualitySelection::Manual { height: 1080 })
            .desired()
            .digest(),
        "Auto at 1080 has not withdrawn the server's authority; Manual has",
    );
}

/// The wire shape. `{"mode":"auto"}` is what every client shipped before
/// D3-a sends and what Apple and Android still send, and it must keep
/// parsing and keep serializing back to itself.
#[test]
fn quality_selection_auto_height_round_trips_and_stays_optional() {
    let plain: QualitySelection =
        serde_json::from_str(r#"{"mode":"auto"}"#).expect("plain Auto still parses");
    assert_eq!(
        plain,
        QualitySelection::Auto {
            height: None,
            candidate_id: None
        }
    );
    assert_eq!(
        serde_json::to_string(&plain).expect("plain Auto serializes"),
        r#"{"mode":"auto"}"#,
        "an absent rung must not grow a null on the way back out",
    );

    let named: QualitySelection =
        serde_json::from_str(r#"{"mode":"auto","height":1080}"#).expect("a named rung parses");
    assert_eq!(
        named,
        QualitySelection::Auto {
            height: Some(1080),
            candidate_id: None
        }
    );
    assert_eq!(
        serde_json::to_string(&named).expect("a named rung serializes"),
        r#"{"mode":"auto","height":1080}"#,
    );

    // Bounded exactly as `manual` is, and absent is always accepted.
    assert!(selection_at(QualitySelection::Auto {
        height: None,
        candidate_id: None
    })
    .validate()
    .is_ok());
    assert!(selection_at(QualitySelection::Auto {
        height: Some(1080),
        candidate_id: None
    })
    .validate()
    .is_ok());
    for out_of_range in [143, 2161, 0, -1] {
        assert_eq!(
            selection_at(QualitySelection::Auto {
                height: Some(out_of_range),
                candidate_id: None
            })
            .validate(),
            Err("selection.quality.height"),
            "{out_of_range} is outside the ladder's bounds",
        );
    }
}

/// A named Auto rung is an explicit ask on the candidate, and it does NOT
/// switch the server's own Auto policy off for the successor.
///
/// Both halves matter. Without the first the staged successor is built at
/// the rung the client is already on and the switch delivers nothing. With
/// `automatic` cleared, the viewer would silently leave Auto by having
/// used it.
#[test]
fn quality_selection_auto_rung_is_an_explicit_ask_that_stays_automatic() {
    let current = session_request(SessionKind::Transcode { height: 720 });
    let candidate = candidate_request(
        &current,
        &selection_at(QualitySelection::Auto {
            height: Some(1080),
            candidate_id: None,
        }),
        1080,
        Some(2160),
    );
    assert_eq!(
        candidate.kind,
        SessionKind::Transcode { height: 1080 },
        "the rung the client named is the rung the successor is built at",
    );
    assert!(
        candidate.automatic,
        "Auto with a rung is still Auto; the server may move again",
    );

    // Unnamed Auto is unchanged in every respect.
    let plain = candidate_request(
        &current,
        &selection_at(QualitySelection::Auto {
            height: None,
            candidate_id: None,
        }),
        720,
        Some(2160),
    );
    assert_eq!(plain.kind, current.kind);
    assert!(plain.automatic);

    // And Auto never turns a copy into a transcode, named rung or not.
    let copying = converting_copy();
    assert_eq!(
        candidate_request(
            &copying,
            &selection_at(QualitySelection::Auto {
                height: Some(1080),
                candidate_id: None
            }),
            1080,
            Some(2160),
        )
        .kind,
        copying.kind,
        "Auto is a request to let the server choose, not to stop copying",
    );
}

/// Only a burn is a burn. Off, Native and Overlay are not video
/// replacements at all, and mapping their track into `subtitle_burn` would
/// turn every native-subtitle change into a whole new encode recipe.
#[test]
fn only_a_burn_reaches_subtitle_burn() {
    let mut current = session_request(SessionKind::Transcode { height: 1080 });
    current.subtitle_burn = Some(2);

    for mode in [
        SubtitleMode::Off,
        SubtitleMode::Native,
        SubtitleMode::Overlay,
    ] {
        let mut selection = selection_at(QualitySelection::Manual { height: 1080 });
        selection.subtitle = SubtitleSelection {
            mode,
            track: if matches!(mode, SubtitleMode::Off) {
                None
            } else {
                Some(3)
            },
        };
        let candidate = candidate_request(&current, &selection, 1080, Some(2160));
        assert_eq!(candidate.subtitle_burn, None, "{mode:?} is not a burn");
    }

    let mut burning = selection_at(QualitySelection::Manual { height: 1080 });
    burning.subtitle = SubtitleSelection {
        mode: SubtitleMode::Burn,
        track: Some(3),
    };
    assert_eq!(
        candidate_request(&current, &burning, 1080, Some(2160)).subtitle_burn,
        Some(3),
    );
}

/// The client's codec and dynamic-range answers are *policies*, and the
/// server's are answers. A selection carries the first and cannot carry the
/// second, so the session's own are carried through untouched — a change on
/// those axes means re-review, which nothing does yet.
#[test]
fn a_selection_cannot_rewrite_the_servers_plan_answers() {
    let mut current = converting_copy();
    current.hdr10 = true;
    let mut selection = selection_at(QualitySelection::Auto {
        height: None,
        candidate_id: None,
    });
    selection.codec = CodecPolicy::Av1;
    selection.dynamic_range = DynamicRangePolicy::Sdr;

    let candidate = candidate_request(&current, &selection, 1080, Some(2160));
    assert_eq!(
        GradeIntent::from_request(&candidate),
        GradeIntent::from_request(&current),
        "a policy is not an answer",
    );
    // And everything the selection does not name survives.
    assert_eq!(candidate.file_id, current.file_id);
    assert_eq!(candidate.start_seconds, current.start_seconds);
    assert_eq!(candidate.playback_id, current.playback_id);
}

/// The grade intent is read off the request, and a transcode answers both
/// Dolby Vision questions `false` as a statement rather than a default:
/// a transcode never carries RPUs through.
#[test]
fn a_grade_intent_comes_off_the_request() {
    use crate::transcode::SessionRequest;

    fn request(kind: SessionKind, hdr10: bool) -> SessionRequest {
        SessionRequest {
            dv_processing_destination: None,
            sdr_master_codecs: None,
            continuous_media: None,
            quality_catalog: None,
            candidate_context: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            file_id: 1,
            playback_id: "player-a".to_owned(),
            request_id: None,
            control_sequence: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind,
            start_seconds: 0.0,
            audio_index: None,
            audio_delivery: None,
            audio_claim: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        }
    }

    assert_eq!(
        GradeIntent::from_request(&request(SessionKind::Transcode { height: 1080 }, true)),
        GradeIntent {
            hdr10: true,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        },
    );
    assert_eq!(
        GradeIntent::from_request(&request(
            SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
                dual_layer_as_copied: false,
            },
            false,
        )),
        GradeIntent {
            hdr10: false,
            preserve_dolby_vision: true,
            convert_dolby_vision: true,
        },
        "the AAC re-encode is an audio fact and is not a grade fact",
    );
    // Preserving without converting is a third answer, not a rounding of
    // the other two: the RPUs survive but are not rewritten.
    assert_ne!(
        GradeIntent::from_request(&request(
            SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: true,
                convert_dolby_vision: false,
                dual_layer_as_copied: false,
            },
            false,
        )),
        GradeIntent::from_request(&request(
            SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
                dual_layer_as_copied: false,
            },
            false,
        )),
    );
}

/// The wire vocabulary is the operator's, so it is pinned rather than
/// merely produced. Nothing else in the tree asserts these strings, and a
/// swapped pair would make every dashboard say the opposite of the truth
/// while every other test still passed.
#[test]
fn the_decision_vocabulary_is_fixed() {
    assert_eq!(
        PreparationAxis::ResolutionOrBitrate.as_str(),
        "resolution_or_bitrate"
    );
    assert_eq!(
        PreparationAxis::AudioTrackOrOffset.as_str(),
        "audio_track_or_offset"
    );
    assert_eq!(PreparationAxis::SubtitleBurn.as_str(), "subtitle_burn");
    assert_eq!(PreparationAxis::DeliveryMethod.as_str(), "delivery_method");
    assert_eq!(PreparationAxis::DynamicRange.as_str(), "dynamic_range");
    assert_eq!(
        FallbackReason::ClientCannotPrepare.as_str(),
        "client_cannot_prepare"
    );
}

/// Compound changes prepare once and name the hardest crossed axis.
#[test]
fn compound_changes_prepare_once_and_the_hardest_names_it() {
    let caps = can_prepare(true);
    let mut audio_and_height = playing(1080);
    audio_and_height.audio_track = Some(1);
    let mut audio_and_burn = playing(2160);
    audio_and_burn.audio_track = Some(1);
    audio_and_burn.subtitle_burn = Some(3);
    let mut burn = playing(2160);
    burn.subtitle_burn = Some(3);
    let mut everything = playing(720);
    everything.codec = "source".to_owned();
    everything.audio_track = Some(2);
    everything.subtitle_burn = Some(1);

    for (candidate, grade, axis) in [
        (
            &audio_and_height,
            sdr(),
            PreparationAxis::AudioTrackOrOffset,
        ),
        (&audio_and_burn, sdr(), PreparationAxis::SubtitleBurn),
        (&burn, hdr10(), PreparationAxis::DynamicRange),
        (&everything, hdr10(), PreparationAxis::DynamicRange),
    ] {
        assert_eq!(
            decide_preparation(
                view(&playing(2160)),
                view_at(candidate, grade),
                Some(&caps),
                roomy(),
            ),
            PreparationDecision::Prepare { axis },
            "{}",
            axis.as_str(),
        );
    }
}

/// Link observations are advisory and never gate a capable client.
#[test]
fn every_link_observation_preserves_the_prepare_decision() {
    let caps = can_prepare(true);
    // Both are refusals; they are separate outcomes because "the link was
    // too tight" and "nobody measured the link" are opposite findings, and
    // a counter that reported them as one number would read as evidence
    // for the throughput rule while measuring only its own missing inputs.
    // This was once effectively all of it, for two separate reasons that
    // have both since been fixed: the native clients now send
    // `observed_download_bps` (Apple from AVFoundation's access log,
    // Android off the wire), and VOD sessions now carry a measured
    // `delivered_bps`. What remains unreported is a session too young for
    // a window to have closed.
    let observations = [
        (
            "no margin at all",
            PreparationConditions {
                observed_download_bps: Some(12_000_000),
                delivered_bps: Some(12_000_000),
            },
        ),
        (
            "a hair under twice",
            PreparationConditions {
                observed_download_bps: Some(23_999_999),
                delivered_bps: Some(12_000_000),
            },
        ),
        (
            "the client never reported one",
            PreparationConditions {
                observed_download_bps: None,
                delivered_bps: Some(12_000_000),
            },
        ),
        (
            "the server has not measured delivery yet",
            PreparationConditions {
                observed_download_bps: Some(80_000_000),
                delivered_bps: None,
            },
        ),
        (
            "nothing delivered yet, so there is no rate to double",
            PreparationConditions {
                observed_download_bps: Some(80_000_000),
                delivered_bps: Some(0),
            },
        ),
        (
            "a delivered rate that is not a measurement",
            PreparationConditions {
                observed_download_bps: Some(80_000_000),
                delivered_bps: Some(-1),
            },
        ),
    ];
    for (case, conditions) in observations {
        assert_eq!(
            decide_preparation(
                view(&playing(2160)),
                view(&playing(1080)),
                Some(&caps),
                conditions,
            ),
            PreparationDecision::Prepare {
                axis: PreparationAxis::ResolutionOrBitrate,
            },
            "{case}",
        );
    }
    // Exactly twice is the floor, and the floor passes.
    assert_eq!(
        decide_preparation(
            view(&playing(2160)),
            view(&playing(1080)),
            Some(&caps),
            PreparationConditions {
                observed_download_bps: Some(24_000_000),
                delivered_bps: Some(12_000_000),
            },
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::ResolutionOrBitrate,
        },
    );
}

/// The delivered grade never votes. It is the encoder's answer, and a
/// candidate has no encoder — so a session whose HDR10 rung the encoder
/// refused must not read as a grade crossing on every exchange for the
/// rest of its life.
#[test]
fn the_encoders_answer_is_not_the_grade_the_decision_reads() {
    let caps = can_prepare(true);
    // The session asked for HDR10 and the encoder refused it: delivered
    // says `sdr`, the request still says HDR10.
    let mut refused = playing(2160);
    refused.dynamic_range = Some("sdr".to_owned());
    let mut lower = playing(1080);
    lower.dynamic_range = Some("sdr".to_owned());
    assert_eq!(
        decide_preparation(
            view_at(&refused, hdr10()),
            view_at(&lower, hdr10()),
            Some(&caps),
            roomy(),
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::ResolutionOrBitrate,
        },
        "a plain quality change on a refused-HDR10 session is still a plain \
             quality change",
    );
    // And the reverse: two sessions the encoder happened to deliver
    // identically are still a grade change if their requests differ.
    let mut delivered_alike = playing(2160);
    delivered_alike.dynamic_range = Some("sdr".to_owned());
    assert_eq!(
        decide_preparation(
            view_at(&delivered_alike, sdr()),
            view_at(&delivered_alike, hdr10()),
            Some(&caps),
            roomy(),
        ),
        PreparationDecision::Prepare {
            axis: PreparationAxis::DynamicRange,
        },
        "the encoder agreeing does not make two different asks the same ask",
    );
}

/// The same delivered selection is not a transition, whatever else is
/// true — and the early return is before every other gate, so a throttled
/// link and an incapable client both still read `Unchanged`.
#[test]
fn an_unchanged_selection_prepares_nothing() {
    let starved = PreparationConditions {
        observed_download_bps: Some(1),
        delivered_bps: Some(12_000_000),
    };
    for caps in [Some(can_prepare(true)), Some(can_prepare(false)), None] {
        for conditions in [roomy(), starved] {
            assert_eq!(
                decide_preparation(
                    view(&playing(2160)),
                    view(&playing(2160)),
                    caps.as_ref(),
                    conditions,
                ),
                PreparationDecision::Unchanged,
            );
        }
    }
}

fn staged_preparation(
    incarnation_id: &str,
    predecessor: &str,
    now_ms: i64,
) -> plurx_core::domain::MediaSessionPreparation {
    plurx_core::domain::MediaSessionPreparation {
        quality_cancellation_key: None,
        expected_desired_revision: None,
        incarnation_id: incarnation_id.to_owned(),
        session_id: uuid::Uuid::new_v4().to_string(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        playback_id: "player-a".to_owned(),
        expected_predecessor_incarnation_id: predecessor.to_owned(),
        expected_predecessor_owner_node_id: "node-a".to_owned(),
        expected_predecessor_owner_epoch: 1,
        request_fingerprint: "b".repeat(64),
        owner_node_id: "node-a".to_owned(),
        recipe_json: "{}".to_owned(),
        response_json: r#"{"session":"staged"}"#.to_owned(),
        media_origin_ms: 0,
        now_ms,
        // Derived, not literal: `validate_preparation` requires
        // `deadline_ms > now_ms`, and the neighbouring tests in this
        // module take `now_ms` from `unix_ms()`. A constant deadline
        // silently becomes invalid the moment someone copies this helper
        // into a test with a real clock, and it surfaces as a bare
        // `.expect("stage")` panic with no hint of why.
        deadline_ms: now_ms.saturating_add(798_000),
    }
}

/// A live playback with one current generation, and the executor pointed
/// at it. Returns the predecessor's incarnation id alongside the pieces,
/// because every preparation names it and a preparation with the wrong
/// predecessor is refused before it reaches the slot.
async fn preparation_fixture(
    now_ms: i64,
) -> (
    String,
    Arc<dyn plurx_core::store::Store>,
    RollingControlHandle,
    PreparationExecutor,
) {
    use plurx_core::store::SqliteStore;

    let concrete = SqliteStore::open_in_memory().expect("store");
    let (predecessor, _) = activate_route(&concrete, now_ms, 900_000).await;
    let store: Arc<dyn plurx_core::store::Store> = Arc::new(concrete);
    let control = RollingControlHandle::spawn_for_test("session-start");
    let executor = PreparationExecutor::new(
        Arc::clone(&store),
        Arc::new(control.clone()),
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        "player-a".to_owned(),
        "node-a".to_owned(),
        1,
    );
    (predecessor, store, control, executor)
}

/// M6 acceptance 1 — commit durability, on the happy path.
///
/// The claim is not only that the pointer advances. It is that the
/// executor leaves nothing behind: no ledger row, and no slot the actor is
/// still holding for a successor it has already become.
#[tokio::test]
async fn preparation_executor_commits_a_staged_successor() {
    let now_ms = 2_000;
    let (predecessor, store, control, executor) = preparation_fixture(now_ms).await;

    let successor = uuid::Uuid::new_v4().to_string();
    assert!(
        executor
            .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
            .await
            .expect("stage"),
        "a preparation naming the current pointer is admissible"
    );
    assert_eq!(
        store
            .staged_media_session_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("ledger")
            .expect("a successor is staged")
            .staged_incarnation_id,
        successor,
    );
    assert_eq!(
        store
            .media_session_route_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("route while staged")
            .expect("the playback still has a pointer")
            .incarnation_id,
        predecessor,
        "staging must not advance the pointer",
    );

    assert!(
        control
            .reserve_preparation_commit_for_test(&successor, now_ms + 200)
            .await
    );
    assert!(
        matches!(
            executor
                .commit(&successor, now_ms + 200, 900_000, None)
                .await
                .expect("commit"),
            PreparationCommitOutcome::Committed(_)
        ),
        "the pointer still names the recorded predecessor, so the CAS wins"
    );
    assert_eq!(
        store
            .media_session_route_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("route after commit")
            .expect("the playback has a pointer")
            .incarnation_id,
        successor,
    );
    assert!(
        store
            .staged_media_session_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("ledger after commit")
            .is_none(),
        "a committed successor is no longer a preparation",
    );
    // The pointer advancing is not the same as the viewer having a
    // stream, and this is the line that keeps the two apart. A commit
    // moves the successor off its preparation deadline and nothing else:
    // the publication fence is still at the sentinel, and arming it is
    // `arm_media_session_handoff`'s job, exactly as it is for an
    // activated replacement.
    //
    // **And nothing calls it, which is a hole rather than a design.** An
    // activation has `settle_activation_predecessor` to finish the job; a
    // commit has no equivalent, so the successor this test just made
    // current is refused on both planes from its very first request —
    // `classify_durable_route` answers `OwnerTransition` for any non-zero
    // fence and `control_owner_refusal` refuses control on the same
    // predicate. The pointer moved and the viewer got nothing.
    //
    // **Do not fix that by publishing here.** Publishing is only half the
    // transaction and the wrong half first. A committed successor has no
    // local worker, because nothing primes one — `stage_prepared_successor`
    // is stage-only. Moving the row off the sentinel puts it into
    // `owned_media_sessions`, and the lease loop renews only sessions that
    // are *live*: `take_stale_settlement_candidates` selects exactly the
    // inventory rows that are not, and `end_media_session_if_owner` ends
    // them `replaced` **and deletes the playback pointer in the same
    // transaction**. Publishing a workerless successor therefore trades a
    // stalled pointer for a deleted one, seconds after the commit instead
    // of minutes. That was measured, not reasoned about: the call was
    // written, the suite passed, and an adversarial pass found the sweep.
    //
    // The publication belongs to §5.1's phase 3 alongside the priming that
    // gives the successor a worker, and it needs the predecessor's
    // terminal projection that `settle_activation_predecessor` waits for
    // before it may use `PredecessorAcknowledged` at all.
    // `docs/playback-control/M6-SERVER-PRIME-HANDOFF.md` §4 carries the
    // whole shape.
    let committed = store
        .media_session_route_by_incarnation(&successor)
        .await
        .expect("committed route")
        .expect("the successor exists");
    assert_eq!(committed.lease_expires_at_ms, 900_000);
    assert_eq!(
        committed.publication_ready_at_ms,
        plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        "commit advances the pointer; it does not publish",
    );
    assert!(
        control
            .stage_preparation(
                uuid::Uuid::new_v4().to_string(),
                successor.clone(),
                i64::MAX,
                None,
            )
            .await,
        "the slot is free again, so the next preparation can take it",
    );
}

/// M6 acceptance 1, the half that matters — a lost CAS aborts the staged
/// generation and never reaps the newer player generation that won.
///
/// The pointer is advanced out from under the preparation by an ordinary
/// activation, which is what a client restart does. The staged successor
/// must lose, and the generation that replaced its predecessor must still
/// be current and playable afterwards.
#[tokio::test]
async fn preparation_executor_abandons_a_successor_whose_pointer_moved() {
    let now_ms = 2_000;
    let (predecessor, store, control, executor) = preparation_fixture(now_ms).await;

    let successor = uuid::Uuid::new_v4().to_string();
    assert!(executor
        .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
        .await
        .expect("stage"));

    let winner = uuid::Uuid::new_v4().to_string();
    let advance = plurx_core::domain::MediaSessionActivation {
        recovery_epoch: String::new(),
        expected_desired_revision: None,
        incarnation_id: winner.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        playback_id: "player-a".to_owned(),
        expected_predecessor_incarnation_id: Some(predecessor.clone()),
        fence_predecessor: true,
        request_id: None,
        request_fingerprint: "c".repeat(64),
        owner_node_id: "node-a".to_owned(),
        recipe_json: "{}".to_owned(),
        response_json: r#"{"session":"advanced"}"#.to_owned(),
        publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: now_ms + 150,
        lease_expires_at_ms: 900_000,
    };
    store
        .activate_media_session(&advance)
        .await
        .expect("advance the pointer")
        .expect("the advance wins");
    // A replacement carries a predecessor, so its confirmation must name a
    // publication instant a full handoff safety window out. Zero is only
    // valid for a first activation, which is what `activate_route` does.
    store
        .settle_media_session_activation(
            &advance,
            plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: advance
                    .now_ms
                    .saturating_add(plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS),
            },
            advance.now_ms,
        )
        .await
        .expect("confirm the advance")
        .expect("the advance is confirmed");

    assert!(
        control
            .reserve_preparation_commit_for_test(&successor, now_ms + 175)
            .await
    );
    assert_eq!(
        executor
            .commit(&successor, now_ms + 200, 900_000, None)
            .await
            .expect("commit"),
        PreparationCommitOutcome::Refused,
        "the pointer no longer names the recorded predecessor",
    );
    assert!(
        !control.may_commit_preparation(&successor).await,
        "a definitive Store refusal cannot become an exact-retry commit"
    );
    assert_eq!(
        store
            .media_session_route_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("route after the lost CAS")
            .expect("the playback has a pointer")
            .incarnation_id,
        winner,
        "a lost commit must never reap the newer player generation",
    );
    assert_eq!(
        store
            .media_session_route_by_incarnation(&winner)
            .await
            .expect("winner route")
            .expect("the winner exists")
            .state,
        "active",
    );
    assert!(
        store
            .staged_media_session_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("ledger after the lost CAS")
            .is_none(),
        "the abandoned successor releases the one-per-playback slot",
    );
    // Delisting the ledger row is not tearing the successor down. A row
    // left `active` on its preparation lease counts against the user's
    // admission caps, and the maintenance reaper keys on the *session's*
    // state — so a successor that stays active is never swept at all.
    let abandoned = store
        .media_session_route_by_incarnation(&successor)
        .await
        .expect("abandoned route")
        .expect("the abandoned row is still readable");
    assert_eq!(abandoned.state, "ended");
    assert_eq!(abandoned.terminal_reason.as_deref(), Some("replaced"));
    assert!(
        control
            .stage_preparation(uuid::Uuid::new_v4().to_string(), winner, i64::MAX, None)
            .await,
        "and the actor's slot with it",
    );
}

/// M6 acceptance 2 — abort tears down only the successor.
#[tokio::test]
async fn preparation_executor_aborts_only_the_successor() {
    let now_ms = 2_000;
    let (predecessor, store, control, executor) = preparation_fixture(now_ms).await;

    let successor = uuid::Uuid::new_v4().to_string();
    assert!(executor
        .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
        .await
        .expect("stage"));
    executor
        .abort(&successor, now_ms + 200)
        .await
        .expect("abort");

    let current = store
        .media_session_route_for_playback(
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
            "player-a",
        )
        .await
        .expect("route after abort")
        .expect("the current stream survives an abort");
    assert_eq!(current.incarnation_id, predecessor);
    assert_eq!(
        current.state, "active",
        "the predecessor stays authoritative and playable",
    );
    assert_eq!(
        store
            .media_session_route_by_incarnation(&successor)
            .await
            .expect("successor route")
            .expect("the successor row is still readable")
            .state,
        "ended",
    );
    assert!(store
        .staged_media_session_for_playback(
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
            "player-a"
        )
        .await
        .expect("ledger after abort")
        .is_none());

    // An owner retrying an abort after a crash reads back the same
    // outcome rather than a spurious loss. Ordered before the slot is
    // reused, because a replay against a *reused* slot is a different
    // property — the one asserted below.
    executor
        .abort(&successor, now_ms + 300)
        .await
        .expect("abort replay");

    let replacement = uuid::Uuid::new_v4().to_string();
    assert!(
        control
            .stage_preparation(replacement.clone(), predecessor, i64::MAX, None)
            .await,
        "the slot is free, so the next preparation can take it",
    );
    // And a stale executor cannot settle the successor it used to hold:
    // identity is checked rather than assumed, or a crashed owner's late
    // abort would free a slot holding something else entirely.
    assert!(
        !control.settle_preparation(&successor, false).await,
        "a stale settle must not free a slot holding a replacement",
    );
    assert!(
        control
            .reserve_preparation_commit_for_test(&replacement, now_ms + 400)
            .await,
        "the replacement is still the staged successor the actor can reserve",
    );
    assert!(
        control.may_commit_preparation(&replacement).await,
        "and the reservation, not mere staging, authorizes its Store CAS",
    );
}

/// The actor's gate is the first of the three authorities, and it is the
/// only one that can refuse a commit the store would happily accept.
///
/// That is the whole point of asking. Here the pointer still names the
/// recorded predecessor, so the CAS would win — the commit is stopped
/// solely because the actor no longer holds this successor, which is what
/// a viewer disconnecting mid-preparation looks like: `terminate` moves
/// the slot to aborting and the successor stops being wanted. Without the
/// gate the pointer would advance to a generation nobody waited for.
#[tokio::test]
async fn preparation_executor_refuses_a_commit_the_actor_no_longer_wants() {
    let now_ms = 2_000;
    let (predecessor, store, control, executor) = preparation_fixture(now_ms).await;

    let successor = uuid::Uuid::new_v4().to_string();
    assert!(executor
        .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
        .await
        .expect("stage"));

    // The actor forgets the successor without the store being told.
    assert!(control.settle_preparation(&successor, false).await);
    assert!(!control.may_commit_preparation(&successor).await);

    assert_eq!(
        executor
            .commit(&successor, now_ms + 200, 900_000, None)
            .await
            .expect("commit"),
        PreparationCommitOutcome::Refused,
        "the gate refuses before the store is asked",
    );
    assert_eq!(
        store
            .media_session_route_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("route after the refused commit")
            .expect("the playback has a pointer")
            .incarnation_id,
        predecessor,
        "a refused commit must not advance the pointer",
    );
    // And the store still holds the successor, untouched: the gate stops
    // short of the durable call rather than tearing anything down. Who
    // reaps it is the disconnect path's problem, and `terminate` aborts
    // the slot it is holding for exactly this reason.
    assert_eq!(
        store
            .staged_media_session_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("ledger after the refused commit")
            .expect("the staged row survives a gate refusal")
            .staged_incarnation_id,
        successor,
    );
}

/// The durable row is created before the slot is taken, so the slot
/// refusing must undo the row. Otherwise a successor nobody owns counts
/// against the user's admission cap until the maintenance backstop runs.
#[tokio::test]
async fn preparation_executor_rolls_back_a_row_the_slot_refused() {
    use plurx_core::store::SqliteStore;

    let now_ms = 2_000;
    let concrete = SqliteStore::open_in_memory().expect("store");
    let (predecessor, _) = activate_route(&concrete, now_ms, 900_000).await;
    let store: Arc<dyn plurx_core::store::Store> = Arc::new(concrete);
    let executor = PreparationExecutor::new(
        Arc::clone(&store),
        Arc::new(RollingControlHandle::unavailable_for_test()),
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        "player-a".to_owned(),
        "node-a".to_owned(),
        1,
    );

    let successor = uuid::Uuid::new_v4().to_string();
    assert!(
        !executor
            .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
            .await
            .expect("stage"),
        "an actor that cannot be reached cannot take the slot",
    );
    assert!(
        store
            .staged_media_session_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("ledger after the refused stage")
            .is_none(),
        "the durable row is undone rather than left for the backstop",
    );
    // An empty ledger alone would also be satisfied by never having made
    // the row, which is the opposite ordering and a different design.
    // The successor's own session row is what says the row existed and
    // was aborted: `replaced` is the abort's signature.
    let rolled_back = store
        .media_session_route_by_incarnation(&successor)
        .await
        .expect("rolled-back route")
        .expect("the durable row was created before the slot was asked");
    assert_eq!(rolled_back.state, "ended");
    assert_eq!(rolled_back.terminal_reason.as_deref(), Some("replaced"));
    assert_eq!(
        store
            .media_session_route_for_playback(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "player-a"
            )
            .await
            .expect("route after the refused stage")
            .expect("the current stream is untouched")
            .incarnation_id,
        predecessor,
    );
}

fn prepublication_actor(now: Instant) -> RollingControlActor {
    let mut actor =
        RollingControlActor::new(now, "session-start", Arc::new(AtomicBool::new(false)));
    actor.prepublication = Some(PrepublicationProducerControl::new());
    actor
}

fn retry_recipe(contract: &str, fingerprint: &str) -> ValidatedRetryRecipe {
    ValidatedRetryRecipe::new(
        "cpu-safe".to_owned(),
        fingerprint.to_owned(),
        contract.to_owned(),
        ProducerStartupKind::Software,
    )
}

fn hardware_policy(contract: &str, fingerprint: &str) -> InitialProducerPolicy {
    InitialProducerPolicy::hardware_with_startup(
        contract.to_owned(),
        PRODUCER_PROGRESS_BUDGET,
        retry_recipe(contract, fingerprint),
        ProducerStartupKind::Hardware,
    )
}

fn copy_policy(contract: &str, fingerprint: &str) -> InitialProducerPolicy {
    InitialProducerPolicy::copy(
        contract.to_owned(),
        PRODUCER_PROGRESS_BUDGET,
        Some(
            ValidatedRetryRecipe::new(
                "legacy-copy".to_owned(),
                fingerprint.to_owned(),
                contract.to_owned(),
                ProducerStartupKind::Copy,
            )
            .with_exit_classifier(ProducerExitClassifier::Immediate),
        ),
    )
}

fn registered_prepublication_actor(now: Instant) -> RollingControlActor {
    let mut actor = prepublication_actor(now);
    assert_eq!(actor.register_producer_executor_at(), Ok(()));
    actor
}

#[test]
fn mkv_hls_startup_progress_is_bound_to_timeline_attempt_and_sequence() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    actor.startup.activate_at(started);

    let mut first = request();
    first.sequence = 1;
    first.position_ms = 10_000;
    first.buffered_through_ms = 25_000;
    let first_outcome = actor
        .control_at(started + Duration::from_millis(10), owned_control(&first))
        .expect("first rendering observation");
    assert_eq!(
        first_outcome.lease.startup.phase,
        RollingStartupPhase::AwaitingPresentation
    );
    assert!(first_outcome.lease.startup.presentation_progress_seen);

    let replay = actor
        .control_at(started + Duration::from_millis(20), owned_control(&first))
        .expect("exact replay");
    assert_eq!(replay.disposition, ControlDisposition::Replay);
    assert_eq!(
        replay.lease.startup.phase,
        RollingStartupPhase::AwaitingPresentation,
        "a duplicated accepted sequence is not new presentation evidence"
    );

    // A source-only retry keeps the original deadline but cannot stitch
    // its first position sample to the predecessor attempt.
    assert_eq!(
        actor.begin_producer_attempt_at(started + Duration::from_millis(250)),
        Ok(2)
    );
    let mut successor_first = first.clone();
    successor_first.sequence = 2;
    successor_first.position_ms = 10_300;
    let successor_first = actor
        .control_at(
            started + Duration::from_millis(300),
            owned_control(&successor_first),
        )
        .expect("successor's first rendering observation");
    assert_eq!(
        successor_first.lease.startup.phase,
        RollingStartupPhase::AwaitingPresentation,
        "progress cannot be assembled across producer attempts"
    );

    let mut successor_progress = first.clone();
    successor_progress.sequence = 3;
    successor_progress.position_ms = 10_600;
    let presented = actor
        .control_at(
            started + Duration::from_millis(600),
            owned_control(&successor_progress),
        )
        .expect("successor's advancing rendering observation");
    assert_eq!(
        presented.lease.startup.phase,
        RollingStartupPhase::Presented
    );
    assert_eq!(presented.lease.startup.remaining, None);
}

/// The snapshots the shipped web client emits, replayed through the real
/// actor. `tests/playback/seek-control.test.js` asserts the mapper still
/// produces exactly these bytes, so neither half of R1 can drift alone.
const WEB_SNAPSHOT_FIXTURES: &str =
    include_str!("../../../../tests/playback/seek-scratch-snapshots.json");

fn web_snapshot_request(name: &str, generation: &str, sequence: u64) -> ControlRequestV1 {
    let fixtures: serde_json::Value =
        serde_json::from_str(WEB_SNAPSHOT_FIXTURES).expect("recorded web snapshots");
    let mut value = fixtures
        .get(name)
        .unwrap_or_else(|| panic!("recorded web snapshot {name}"))
        .clone();
    let object = value.as_object_mut().expect("snapshot object");
    // Everything `makeRequest` stamps on. Only the snapshot itself is
    // recorded, because the snapshot is the part this repair changed.
    object.insert("protocol".to_owned(), PROTOCOL_V1.into());
    object.insert("generation".to_owned(), generation.into());
    object.insert("control_epoch".to_owned(), 1.into());
    object.insert(
        "client_instance_id".to_owned(),
        "22222222-2222-4222-8222-222222222222".into(),
    );
    object.insert("sequence".to_owned(), sequence.into());
    // The real client always sends this, and what the server may answer
    // with depends on it. A replayed request without it is a shape the
    // web player never produces.
    object.insert(
        "supported_actions".to_owned(),
        serde_json::json!(["retry_resource", "change_quality", "prepare", "stop"]),
    );
    serde_json::from_value(value).expect("the recorded snapshot is a valid control request")
}

#[test]
fn seek_scratch_incumbent_survives_a_pending_replacement() {
    // The incident, driven through the real actor with the real client's
    // snapshots. Before the repair each exchange carried
    // render_state=seeking and a foreign seek_target_ms for a create the
    // server had refused; `observe_control` reset the baseline every
    // time, presentation was never established, and a visibly playing
    // incumbent was reaped at first_served_at + 30 s.
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    actor.startup.activate_at(started);

    let generation = uuid::Uuid::new_v4().to_string();
    let first = web_snapshot_request("incumbent_while_replacement_pending", &generation, 1);
    assert_eq!(first.render_state, RenderState::Rendering);
    assert_eq!(first.seek_target_ms, None);
    let first_outcome = actor
        .control_at(started + Duration::from_millis(10), owned_control(&first))
        .expect("the incumbent's first rendering observation");
    assert_eq!(
        first_outcome.lease.startup.phase,
        RollingStartupPhase::AwaitingPresentation,
        "one observation is a baseline, not proof"
    );

    // 300 ms of media progress, past the 250 ms the actor requires, while
    // the replacement is still unresolved.
    let second = web_snapshot_request("incumbent_after_250ms_of_progress", &generation, 2);
    let presented = actor
        .control_at(started + Duration::from_millis(310), owned_control(&second))
        .expect("the incumbent's advancing rendering observation");
    assert_eq!(
        presented.lease.startup.phase,
        RollingStartupPhase::Presented,
        "the picture is demonstrably moving; the startup budget is satisfied"
    );

    // Well past the original 30-second startup budget, with the
    // replacement still refused and the same snapshots still arriving at
    // the ordinary reporter cadence.
    for (sequence, second) in (3..).zip((5..=60).step_by(5)) {
        let late = web_snapshot_request("incumbent_after_250ms_of_progress", &generation, sequence);
        actor
            .control_at(started + Duration::from_secs(second), owned_control(&late))
            .unwrap_or_else(|error| {
                panic!("the incumbent is still reporting at {second}s: {error:?}")
            });
        assert!(
            matches!(
                actor.claim_expiry_at(started + Duration::from_secs(second)),
                RollingExpiryClaim::Live
            ),
            "a presenting incumbent must outlive a refused destination, at {second}s"
        );
    }
    assert_eq!(
        actor
            .snapshot_at(started + Duration::from_secs(60))
            .startup
            .phase,
        RollingStartupPhase::Presented
    );
}

/// The negative control for the repair, and the reason the two `poisoned`
/// fixtures exist: replay what the client used to send, at the same
/// cadence the repaired client is replayed at, and watch the incumbent
/// die.
///
/// Without this the survival test proves only that a healthy stream
/// survives, which was never in doubt. `observe_control` resets the
/// presentation baseline on every exchange carrying a seek target, so a
/// stream that is visibly playing never establishes `Presented` and is
/// retired at `first_served_at + 30 s` — which is exactly what happened
/// on media1.
#[test]
fn seek_scratch_the_old_snapshot_shape_reaps_a_playing_incumbent() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    actor.startup.activate_at(started);

    let generation = uuid::Uuid::new_v4().to_string();
    let mut sequence = 1;
    // The same media progress the repaired shape reports, and plenty of
    // it: the picture is moving the whole time.
    for millis in [10_u64, 310, 610, 910] {
        let name = if sequence == 1 {
            "incumbent_poisoned_by_a_refused_destination"
        } else {
            "incumbent_poisoned_after_250ms_of_progress"
        };
        let poisoned = web_snapshot_request(name, &generation, sequence);
        assert_eq!(poisoned.render_state, RenderState::Seeking);
        assert_eq!(poisoned.seek_target_ms, Some(174_000));
        sequence += 1;
        actor
            .control_at(
                started + Duration::from_millis(millis),
                owned_control(&poisoned),
            )
            .expect("the incumbent is reporting");
    }
    assert_eq!(
        actor
            .snapshot_at(started + Duration::from_secs(1))
            .startup
            .phase,
        RollingStartupPhase::AwaitingPresentation,
        "a foreign destination resets the baseline on every exchange, so \
             presentation is never established however long the picture runs"
    );

    // Keep reporting right up to the deadline, exactly as a playing
    // client would, and it is still reaped.
    // Stop before the deadline: `control_at` claims expiry itself, and a
    // claim already taken would make the assertion below read `Retired`
    // instead of the transition it is about.
    for second in [5_u64, 10, 15, 20, 25] {
        let poisoned = web_snapshot_request(
            "incumbent_poisoned_after_250ms_of_progress",
            &generation,
            sequence,
        );
        sequence += 1;
        let _ = actor.control_at(
            started + Duration::from_secs(second),
            owned_control(&poisoned),
        );
    }
    let RollingExpiryClaim::Claimed(expired) =
        actor.claim_expiry_at(started + Duration::from_secs(31))
    else {
        panic!("this is the incident: the old shape must reap a playing incumbent");
    };
    assert_eq!(expired.terminal, Some(RollingTerminalCause::StartupExpired));
}

#[test]
fn seek_scratch_a_genuinely_unpresented_stream_still_expires() {
    // The other half of R1: removing foreign intent from the exchange
    // must not remove the deadline. A stream that never presents is still
    // retired on the original budget.
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    actor.startup.activate_at(started);

    let generation = uuid::Uuid::new_v4().to_string();
    for sequence in 1..=3 {
        let starting = web_snapshot_request("genuinely_unpresented_start", &generation, sequence);
        assert_eq!(starting.render_state, RenderState::Starting);
        actor
            .control_at(
                started + Duration::from_secs(sequence),
                owned_control(&starting),
            )
            .expect("a start that never presents still reports");
    }
    let RollingExpiryClaim::Claimed(expired) =
        actor.claim_expiry_at(started + Duration::from_secs(31))
    else {
        panic!("an unpresented startup must still expire");
    };
    assert_eq!(expired.terminal, Some(RollingTerminalCause::StartupExpired));
}

#[test]
fn seek_scratch_an_executing_local_seek_still_reports_its_target() {
    // And the third: a seek the attached element is actually performing
    // is still the incumbent's own seek, target and all. The repair
    // separates desired from executing; it does not hide execution.
    let generation = uuid::Uuid::new_v4().to_string();
    let seeking = web_snapshot_request("attached_local_seek_in_progress", &generation, 1);
    assert_eq!(seeking.render_state, RenderState::Seeking);
    assert_eq!(seeking.seek_target_ms, Some(130_000));

    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
    actor.startup.activate_at(started);
    actor
        .control_at(started + Duration::from_millis(10), owned_control(&seeking))
        .expect("an executing seek is accepted");
    assert_eq!(
        actor
            .snapshot_at(started + Duration::from_millis(20))
            .startup
            .phase,
        RollingStartupPhase::AwaitingPresentation,
        "a real seek in flight is not presentation evidence"
    );
}

#[test]
fn mkv_hls_startup_seek_jump_resets_evidence_without_renewing_deadline() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    actor.startup.activate_at(started);

    let mut before_seek = request();
    before_seek.sequence = 1;
    before_seek.position_ms = 5_000;
    actor
        .control_at(
            started + Duration::from_millis(10),
            owned_control(&before_seek),
        )
        .expect("pre-seek observation");

    let mut seek = before_seek.clone();
    seek.sequence = 2;
    seek.position_ms = 90_000;
    seek.seek_target_ms = Some(90_000);
    seek.render_state = RenderState::Seeking;
    let seeking = actor
        .control_at(started + Duration::from_millis(300), owned_control(&seek))
        .expect("seek observation");
    assert_eq!(
        seeking.lease.startup.phase,
        RollingStartupPhase::AwaitingPresentation
    );
    assert!(!seeking.lease.startup.presentation_progress_seen);

    let mut settled = before_seek.clone();
    settled.sequence = 3;
    settled.position_ms = 90_000;
    let settled = actor
        .control_at(
            started + Duration::from_millis(600),
            owned_control(&settled),
        )
        .expect("first settled post-seek observation");
    assert_eq!(
        settled.lease.startup.phase,
        RollingStartupPhase::AwaitingPresentation,
        "the requested seek position alone cannot spend startup"
    );
    assert_eq!(
        settled.lease.startup.remaining,
        Some(ROLLING_PRESENTATION_STARTUP_BUDGET - Duration::from_millis(600)),
        "a seek resets only the position baseline, not the deadline"
    );

    let mut progress = before_seek;
    progress.sequence = 4;
    progress.position_ms = 90_300;
    let presented = actor
        .control_at(
            started + Duration::from_millis(900),
            owned_control(&progress),
        )
        .expect("post-seek presentation progress");
    assert_eq!(
        presented.lease.startup.phase,
        RollingStartupPhase::Presented
    );
}

#[test]
fn mkv_hls_startup_deadline_starts_once_on_first_video_playlist() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::software(
                "presentation-startup".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
            ),
        ),
        Ok(1)
    );

    actor
        .authorize_response_publication_at(
            started,
            RollingResponsePublication::attempt_status(RollingResponseObject::SessionStatus, 1),
        )
        .expect("status publication");
    assert_eq!(actor.snapshot_at(started).startup.remaining, None);

    let first_served_at = started + Duration::from_secs(1);
    actor
        .authorize_response_publication_at(
            first_served_at,
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .expect("first video playlist");
    assert_eq!(
        actor.snapshot_at(first_served_at).startup.remaining,
        Some(ROLLING_PRESENTATION_STARTUP_BUDGET)
    );

    actor
        .authorize_response_publication_at(
            started + Duration::from_secs(10),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .expect("playlist reload");
    assert_eq!(
        actor
            .snapshot_at(started + Duration::from_secs(10))
            .startup
            .remaining,
        Some(Duration::from_secs(21)),
        "playlist reloads do not renew the admission budget"
    );

    let expiry = started + Duration::from_secs(31);
    let RollingExpiryClaim::Claimed(expired) = actor.claim_expiry_at(expiry) else {
        panic!("the startup deadline must retire an abandoned presentation");
    };
    assert_eq!(expired.terminal, Some(RollingTerminalCause::StartupExpired));
    assert_eq!(expired.startup.phase, RollingStartupPhase::Expired);
}

#[test]
fn mkv_hls_startup_rendering_hold_proves_progress_but_waiting_hold_does_not() {
    for rendering in [true, false] {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        actor.begin_producer_attempt_at(started).expect("producer");
        actor.startup.activate_at(started);
        let mut report = request();
        report.render_state = RenderState::Rendering;
        report.position_ms = 1000;
        actor
            .control_at(started, owned_control(&report))
            .expect("active baseline");
        report.sequence += 1;
        report.position_ms += 300;
        report.demand = PlaybackDemand::Hold;
        report.playback_rate = 0.0;
        report.render_state = if rendering {
            RenderState::Rendering
        } else {
            RenderState::Waiting
        };
        let paused = actor
            .control_at(started + Duration::from_secs(1), owned_control(&report))
            .expect("pause");
        assert_eq!(
            paused.lease.startup.phase,
            if rendering {
                RollingStartupPhase::Presented
            } else {
                RollingStartupPhase::AwaitingPresentation
            }
        );
        assert_eq!(paused.lease.pause_remaining, Some(ROLLING_PAUSE_GRACE));
        if !rendering {
            assert_eq!(
                actor
                    .snapshot_at(started + Duration::from_secs(20))
                    .startup
                    .remaining,
                Some(Duration::from_secs(29))
            );
            report.sequence += 1;
            report.demand = PlaybackDemand::Active;
            report.playback_rate = 1.0;
            report.render_state = RenderState::Waiting;
            actor
                .control_at(started + Duration::from_secs(20), owned_control(&report))
                .expect("resume");
            report.sequence += 1;
            actor
                .control_at(started + Duration::from_secs(40), owned_control(&report))
                .expect("keep control alive");
            let RollingExpiryClaim::Claimed(expired) =
                actor.claim_expiry_at(started + Duration::from_secs(49))
            else {
                panic!("remaining active budget expires")
            };
            assert_eq!(expired.terminal, Some(RollingTerminalCause::StartupExpired));
        }
    }
}

#[test]
fn mkv_hls_startup_hold_before_playlist_has_finite_nonrenewing_grace() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let mut report = request();
    report.demand = PlaybackDemand::Hold;
    report.playback_rate = 0.0;
    report.render_state = RenderState::Waiting;
    actor
        .control_at(started, owned_control(&report))
        .expect("pre-playlist pause");
    actor.startup.activate_at(started + Duration::from_secs(10));
    for elapsed in (20..=160).step_by(20) {
        report.sequence += 1;
        let held = actor
            .control_at(
                started + Duration::from_secs(elapsed),
                owned_control(&report),
            )
            .expect("held heartbeat");
        assert_eq!(
            held.lease.startup.remaining,
            Some(ROLLING_PRESENTATION_STARTUP_BUDGET)
        );
    }
    let RollingExpiryClaim::Claimed(expired) = actor.claim_expiry_at(started + ROLLING_PAUSE_GRACE)
    else {
        panic!("finite hold expires")
    };
    assert_eq!(expired.terminal, Some(RollingTerminalCause::PauseExpired));
}

#[test]
fn mkv_hls_schedule_pause_grace_is_finite_and_hold_does_not_renew_it() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    actor.startup = RollingStartupState::Presented;
    let mut hold = request();
    hold.demand = PlaybackDemand::Hold;
    hold.playback_rate = 0.0;
    hold.render_state = RenderState::Waiting;

    let first = actor
        .control_at(started, owned_control(&hold))
        .expect("first pause");
    assert_eq!(first.lease.pause_remaining, Some(ROLLING_PAUSE_GRACE));

    for elapsed in (20..=160).step_by(20) {
        hold.sequence += 1;
        let repeated = actor
            .control_at(started + Duration::from_secs(elapsed), owned_control(&hold))
            .expect("repeated pause");
        assert_eq!(
            repeated.lease.pause_remaining,
            Some(ROLLING_PAUSE_GRACE - Duration::from_secs(elapsed)),
            "repeated Hold renewed the pause grace"
        );
    }

    let RollingExpiryClaim::Claimed(expired) = actor.claim_expiry_at(started + ROLLING_PAUSE_GRACE)
    else {
        panic!("the non-renewing pause deadline must retire the presentation");
    };
    assert_eq!(expired.terminal, Some(RollingTerminalCause::PauseExpired));
    assert_eq!(expired.pause_remaining, Some(Duration::ZERO));

    let mut resume = hold;
    resume.sequence += 1;
    resume.demand = PlaybackDemand::Active;
    resume.playback_rate = 1.0;
    resume.render_state = RenderState::Rendering;
    assert!(
        matches!(
            actor.control_at(
                started + ROLLING_PAUSE_GRACE + Duration::from_millis(1),
                owned_control(&resume),
            ),
            Err(ControlStateError::PauseExpired)
        ),
        "resume cannot reopen the expired actor; the client may create one replacement"
    );
}

#[test]
fn rolling_status_authz_keeps_its_exact_attempt_and_binding() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::software("session-status".to_owned(), PRODUCER_PROGRESS_BUDGET,),
        ),
        Ok(1)
    );

    let status =
        || RollingResponsePublication::attempt_status(RollingResponseObject::SessionStatus, 1);
    let before_media = actor
        .authorize_response_publication_at(started, status())
        .expect("status before first media");
    assert!(!before_media.first_producer_media_publication);
    assert_eq!(
        actor.authorize_response_publication_at(
            started,
            RollingResponsePublication::attempt_status(RollingResponseObject::SessionStatus, 2,),
        ),
        Err(ResponsePublicationRejection::StaleAttempt)
    );
    assert_eq!(
        actor.authorize_response_publication_at(
            started,
            RollingResponsePublication::attempt_media(
                RollingResponseObject::SessionStatus,
                1,
                None,
            ),
        ),
        Err(ResponsePublicationRejection::InvalidBinding)
    );
    assert_eq!(
        actor.authorize_response_publication_at(
            started,
            RollingResponsePublication::protocol_only(RollingResponseObject::SessionStatus, 1,),
        ),
        Err(ResponsePublicationRejection::InvalidBinding)
    );
    assert_eq!(
        actor.authorize_response_publication_at(
            started,
            RollingResponsePublication::generation_metadata(
                RollingResponseObject::SessionStatus,
                "session-status".to_owned(),
            ),
        ),
        Err(ResponsePublicationRejection::InvalidBinding)
    );

    actor.apply_producer_flow_applied_at(
        started + Duration::from_millis(1),
        ProducerFlowApplied {
            revision: 1,
            producer_attempt: 1,
            state: ProducerPhysicalFlowState::Held,
            published_at: started + Duration::from_millis(1),
        },
    );
    let held_at = started + Duration::from_millis(2);
    let held_before = actor.snapshot_at(held_at);
    let while_held = actor
        .authorize_response_publication_at(held_at, status())
        .expect("status while held");
    let held_after = actor.snapshot_at(held_at);
    assert!(!while_held.first_producer_media_publication);
    assert_eq!(
        held_after, held_before,
        "status must be fully observational"
    );

    let first_media = actor
        .authorize_response_publication_at(
            held_at,
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .expect("first media while held");
    assert!(first_media.first_producer_media_publication);
    let published_at = started + Duration::from_millis(3);
    let published_before = actor.snapshot_at(published_at);
    let after_media = actor
        .authorize_response_publication_at(published_at, status())
        .expect("status after first media");
    let published_after = actor.snapshot_at(published_at);
    assert!(!after_media.first_producer_media_publication);
    assert_eq!(
        published_after, published_before,
        "status cannot mutate retry, frontier, decision, recovery, or lease state"
    );
}

#[tokio::test(start_paused = true)]
async fn prepublication_executor_registration_gates_the_initial_policy() {
    let (handle, mut registration) =
        RollingControlHandle::spawn_prepublication_transcode_for_test("session-start");
    let policy = hardware_policy("presentation-a", "recipe-a");

    assert_eq!(
        handle.begin_initial_producer_attempt(policy.clone()).await,
        Err(ProducerAttemptRejection::ExecutorNotRegistered)
    );
    assert_eq!(
        registration.next_decision().await,
        RollingProducerExecutorPoll::Unavailable
    );
    assert_eq!(registration.register().await, Ok(()));
    assert_eq!(registration.register().await, Ok(()));
    assert_eq!(handle.begin_initial_producer_attempt(policy).await, Ok(1));
    assert_eq!(
        handle
            .begin_initial_producer_attempt(InitialProducerPolicy::software(
                "presentation-a".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
            ))
            .await,
        Err(ProducerAttemptRejection::InitialAttemptAlreadyAdmitted)
    );

    tokio::time::advance(PREPUBLICATION_HARDWARE_STARTUP_BUDGET).await;
    let RollingProducerExecutorPoll::Decision(decision) = registration.next_decision().await else {
        panic!("registered executor must receive the retained producer decision");
    };
    assert!(matches!(
        decision.as_ref(),
        ProducerDecision::Retry {
            decision_sequence: 1,
            failed_attempt: 1,
            reason: ProducerDecisionReason::StartupDeadline,
            ..
        }
    ));
}

#[test]
fn prepublication_startup_and_progress_budgets_are_policy_owned() {
    let started = Instant::now();
    let mut hardware = registered_prepublication_actor(started);
    assert_eq!(
        hardware.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-hardware", "recipe-hardware"),
        ),
        Ok(1)
    );
    assert_eq!(
        hardware.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Starting,
            instant: started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET,
        })
    );

    let mut software = registered_prepublication_actor(started);
    let software_progress_budget = Duration::from_secs(17);
    assert_eq!(
        software.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::software(
                "presentation-software".to_owned(),
                software_progress_budget,
            ),
        ),
        Ok(1)
    );
    assert_eq!(
        software.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Starting,
            instant: started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
        })
    );
    assert_eq!(
        software.producer_progress_budget(),
        software_progress_budget
    );

    let mut copy = registered_prepublication_actor(started);
    assert_eq!(
        copy.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-copy", "recipe-copy"),
        ),
        Ok(1)
    );
    assert_eq!(
        copy.producer_exit_classifier,
        ProducerExitClassifier::CopyReader
    );
    assert_eq!(
        copy.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Starting,
            instant: started + PREPUBLICATION_COPY_STARTUP_BUDGET,
        })
    );

    let mut copy_without_retry = registered_prepublication_actor(started);
    assert_eq!(
        copy_without_retry.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::copy(
                "presentation-copy-without-retry".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
                None,
            ),
        ),
        Ok(1)
    );
    assert_eq!(
        copy_without_retry.producer_exit_classifier,
        ProducerExitClassifier::CopyReader
    );

    let mut immediate_copy = registered_prepublication_actor(started);
    assert_eq!(
        immediate_copy.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::copy_immediate(
                "presentation-copy-immediate".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
            ),
        ),
        Ok(1)
    );
    assert_eq!(
        immediate_copy.producer_exit_classifier,
        ProducerExitClassifier::Immediate
    );
    assert_eq!(
        immediate_copy.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::Starting,
            instant: started + PREPUBLICATION_COPY_STARTUP_BUDGET,
        })
    );
}

#[test]
fn generation_metadata_preserves_retry_and_obeys_pending_decision_kind() {
    let started = Instant::now();
    let mut retrying = registered_prepublication_actor(started);
    assert_eq!(
        retrying.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-stable", "recipe-stable"),
        ),
        Ok(1)
    );
    let metadata_at = started + Duration::from_secs(1);
    assert_eq!(
        retrying.authorize_response_publication_at(
            metadata_at,
            RollingResponsePublication::generation_metadata(
                RollingResponseObject::MasterPlaylist,
                "presentation-stable".to_owned(),
            ),
        ),
        Ok(RollingResponseAuthorization {
            first_producer_media_publication: false,
        })
    );
    assert_eq!(
        retrying.last_renewal, started,
        "authorization alone cannot renew the generation lease"
    );
    assert!(retrying.commit_generation_metadata_at(
        metadata_at,
        "presentation-stable",
        "generation-metadata-eof",
    ));
    assert_eq!(retrying.last_renewal, metadata_at);
    assert!(!retrying.commit_generation_metadata_at(
        metadata_at + Duration::from_millis(1),
        "wrong-presentation",
        "generation-metadata-eof",
    ));
    assert!(matches!(
        retrying
            .prepublication
            .as_ref()
            .expect("pre-publication state")
            .retry_state,
        PrepublicationRetryState::Available(_)
    ));
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    assert!(retrying.settle_due_deadlines_at(deadline).is_some());
    assert!(matches!(
        retrying.pending_decision.as_deref(),
        Some(ProducerDecision::Retry { .. })
    ));
    assert!(retrying
        .authorize_response_publication_at(
            deadline,
            RollingResponsePublication::generation_metadata(
                RollingResponseObject::MasterPlaylist,
                "presentation-stable".to_owned(),
            ),
        )
        .is_ok());
    assert_eq!(retrying.last_renewal, metadata_at);
    assert!(retrying.commit_generation_metadata_at(
        deadline,
        "presentation-stable",
        "generation-metadata-eof",
    ));

    let mut failing = registered_prepublication_actor(started);
    assert_eq!(
        failing.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::software(
                "presentation-fail".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
            ),
        ),
        Ok(1)
    );
    assert!(failing
        .settle_due_deadlines_at(started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET)
        .is_some());
    assert!(matches!(
        failing.pending_decision.as_deref(),
        Some(ProducerDecision::Fail { .. })
    ));
    assert_eq!(
        failing.authorize_response_publication_at(
            started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
            RollingResponsePublication::generation_metadata(
                RollingResponseObject::MasterPlaylist,
                "presentation-fail".to_owned(),
            ),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted)
    );
    assert_eq!(
        failing.authorize_response_publication_at(
            started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
            RollingResponsePublication::protocol_only(RollingResponseObject::ProtocolResponse, 1,),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted)
    );
    assert!(!failing.commit_generation_metadata_at(
        started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
        "presentation-fail",
        "generation-metadata-eof",
    ));
}

#[test]
fn compatibility_response_contract_orders_failure_with_publication() {
    let started = Instant::now();
    let failed = Arc::new(AtomicBool::new(false));
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    assert_eq!(
        actor.authorize_response_publication_at(
            started,
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                0,
                Some(0),
            ),
        ),
        Err(ResponsePublicationRejection::ProducerNotAdmitted)
    );
    assert_eq!(
        actor.bind_response_publication_contract_at(
            "copy-contract".to_owned(),
            Arc::clone(&failed),
        ),
        Ok(())
    );
    assert_eq!(
        actor.bind_response_publication_contract_at(
            "copy-contract".to_owned(),
            Arc::clone(&failed),
        ),
        Ok(()),
        "the exact immutable binding is idempotent"
    );
    assert_eq!(
        actor.bind_response_publication_contract_at(
            "other-contract".to_owned(),
            Arc::clone(&failed),
        ),
        Err(ResponsePublicationRejection::PresentationContractMismatch)
    );
    assert_eq!(
        actor.bind_response_publication_contract_at(
            "copy-contract".to_owned(),
            Arc::new(AtomicBool::new(false)),
        ),
        Err(ResponsePublicationRejection::InvalidBinding),
        "the same presentation cannot be rebound to a different failure owner"
    );
    assert!(actor
        .authorize_response_publication_at(
            started + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                0,
                Some(0),
            ),
        )
        .is_ok());

    failed.store(true, Ordering::Release);
    assert_eq!(
        actor.authorize_response_publication_at(
            started + Duration::from_millis(2),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                0,
                Some(0),
            ),
        ),
        Err(ResponsePublicationRejection::SessionEnded),
        "a compatibility failure ordered first must close actor publication"
    );
    assert!(!actor.commit_media_at(
        started + Duration::from_millis(2),
        "failed-copy-eof",
        0,
        Some(0),
        Some(4_000),
    ));
    assert!(!actor.commit_generation_metadata_at(
        started + Duration::from_millis(2),
        "copy-contract",
        "failed-copy-master-eof",
    ));
}

#[test]
fn prepublication_response_contract_orders_failure_and_policy_binding() {
    let started = Instant::now();
    let failed = Arc::new(AtomicBool::new(false));
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.bind_response_publication_contract_at(
            "copy-contract".to_owned(),
            Arc::clone(&failed),
        ),
        Ok(())
    );
    assert_eq!(
        actor.authorize_response_publication_at(
            started,
            RollingResponsePublication::generation_metadata(
                RollingResponseObject::MasterPlaylist,
                "copy-contract".to_owned(),
            ),
        ),
        Err(ResponsePublicationRejection::ProducerNotAdmitted),
        "the failure fence cannot stand in for actor-owned producer policy"
    );
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            copy_policy("copy-contract", "copy-retry"),
        ),
        Ok(1)
    );
    failed.store(true, Ordering::Release);
    assert_eq!(
        actor.authorize_response_publication_at(
            started + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                Some(0),
            ),
        ),
        Err(ResponsePublicationRejection::SessionEnded)
    );
    let control = actor
        .prepublication
        .as_ref()
        .expect("prepublication control");
    assert!(!control.producer_media_published);
    assert!(matches!(
        &control.retry_state,
        PrepublicationRetryState::Available(_)
    ));

    let mut mismatch = registered_prepublication_actor(started);
    assert_eq!(
        mismatch.bind_response_publication_contract_at(
            "copy-contract".to_owned(),
            Arc::new(AtomicBool::new(false)),
        ),
        Ok(())
    );
    assert_eq!(
        mismatch.begin_initial_producer_attempt_at(
            started,
            copy_policy("other-contract", "other-retry"),
        ),
        Err(ProducerAttemptRejection::PresentationContractMismatch)
    );
    assert_eq!(mismatch.delivery.producer_attempt, 0);

    let failed = Arc::new(AtomicBool::new(true));
    let mut failed_before_policy = registered_prepublication_actor(started);
    assert_eq!(
        failed_before_policy.bind_response_publication_contract_at(
            "copy-contract".to_owned(),
            Arc::clone(&failed),
        ),
        Ok(())
    );
    assert_eq!(
        failed_before_policy.begin_initial_producer_attempt_at(
            started,
            copy_policy("copy-contract", "copy-retry"),
        ),
        Err(ProducerAttemptRejection::SessionEnded)
    );
    assert_eq!(failed_before_policy.delivery.producer_attempt, 0);
    let control = failed_before_policy
        .prepublication
        .as_ref()
        .expect("prepublication control");
    assert!(control.initial_policy.is_none());
    assert!(matches!(
        &control.retry_state,
        PrepublicationRetryState::Unavailable
    ));
}

#[tokio::test]
async fn exact_deadline_attempt_media_wins_then_retains_one_published_failure() {
    let started = rolling_now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-media", "recipe-media"),
        ),
        Ok(1)
    );
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    assert!(actor.observe_publication_at(
        deadline - Duration::from_millis(1),
        RollingPublicationObservation {
            producer_attempt: 1,
            publication_commit: true,
            demand_sequence: None,
            produced_segment: Some(0),
            produced_end_ms: Some(6_000),
            playlist_ready: true,
            published_segment: Some(0),
            published_end_ms: Some(6_000),
            published_first_segment: Some(0),
            published_start_ms: Some(0),
            media_origin_ms: 0,
            next_media_sequence: 1,
            resolved_fetched_segment: None,
            resolved_fetched_end_ms: None,
        },
    ));
    let handoff = RollingFirstMediaPublicationHandoff::new();
    let handoff_result = handoff.waiter();
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 1,
            published_at: deadline,
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::AuthorizeResponsePublication {
                publication: RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
                handoff: Some(handoff),
                deadline: rolling_now() + Duration::from_secs(1),
                reply,
            },
        })
        .await;
    assert_eq!(
        response.await.expect("publication reply"),
        Ok(RollingResponseAuthorization {
            first_producer_media_publication: true,
        })
    );
    assert!(handoff_result.is_accepted());
    let retained = actor
        .pending_decision
        .clone()
        .expect("exact-deadline publication retains one post-publication failure");
    let ProducerDecision::Fail {
        reason,
        proposal: Some(proposal),
        cleanup,
        ..
    } = retained.as_ref()
    else {
        panic!("published deadline must fail with an immutable proposal");
    };
    assert_eq!(*reason, ProducerDecisionReason::StartupDeadline);
    assert!(uuid::Uuid::parse_str(&proposal.proposal_id).is_ok());
    assert_eq!(cleanup.cleanup_policy, CleanupPolicy::RetainPublished);
    assert!(actor.producer_progress_deadline.is_none());
    assert!(actor.producer_deadline_due.is_none());
    assert!(actor.producer_process_exit_due.is_none());
    assert!(actor
        .settle_due_deadlines_at(deadline + Duration::from_secs(1))
        .is_none());
    assert_eq!(
        actor.observe_producer_exit_at(
            deadline + Duration::from_secs(2),
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(1),
                signal: None,
                observed_at: deadline + Duration::from_secs(2),
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert_eq!(actor.pending_decision.as_ref(), Some(&retained));
    actor.mark_executor_lost(deadline + Duration::from_secs(3));
    assert_eq!(actor.pending_decision.as_ref(), Some(&retained));
    assert!(actor
        .authorize_response_publication_at(
            deadline + Duration::from_secs(3),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                Some(0),
            ),
        )
        .is_ok());
    assert!(actor
        .authorize_response_publication_at(
            deadline + Duration::from_secs(3),
            RollingResponsePublication::generation_metadata(
                RollingResponseObject::MasterPlaylist,
                "presentation-media".to_owned(),
            ),
        )
        .is_ok());
    assert!(actor.commit_generation_metadata_at(
        deadline + Duration::from_secs(3),
        "presentation-media",
        "generation-metadata-eof",
    ));
    let status = actor.producer_operational_snapshot_at(deadline);
    assert_eq!(status.phase, "producer_ended_with_proposal");
    assert_eq!(status.action_owner, "producer_ended_with_proposal");
    assert!(status.producer_media_published);
    assert!(status.producer_ended_with_proposal);
    assert_eq!(
        status
            .proposal
            .as_ref()
            .map(|proposal| proposal.proposal_id.as_str()),
        Some(proposal.proposal_id.as_str())
    );
}

#[test]
fn first_media_retains_advancing_deadline_and_published_failure_is_immutable() {
    let started = Instant::now();
    let progressed_at = started + Duration::from_secs(1);
    let published_at = started + Duration::from_secs(2);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-published", "recipe-published"),
        ),
        Ok(1)
    );
    assert!(actor.observe_producer_progress_at(
        progressed_at,
        RollingProducerProgressObservation {
            producer_attempt: 1,
            out_time_ms: Some(1_000),
            speed_milli: Some(1_000),
            recent_speed_milli: Some(1_000),
            observed_at: progressed_at,
        },
    ));
    let advancing = actor
        .producer_progress_deadline
        .expect("advancing deadline before publication");
    assert_eq!(advancing.mode, ProducerProgressDeadlineMode::Advancing);
    assert!(actor.observe_publication_at(
        published_at - Duration::from_millis(1),
        RollingPublicationObservation {
            producer_attempt: 1,
            publication_commit: true,
            demand_sequence: None,
            produced_segment: Some(1),
            produced_end_ms: Some(12_000),
            playlist_ready: true,
            published_segment: Some(1),
            published_end_ms: Some(12_000),
            published_first_segment: Some(0),
            published_start_ms: Some(0),
            media_origin_ms: 0,
            next_media_sequence: 2,
            resolved_fetched_segment: None,
            resolved_fetched_end_ms: None,
        },
    ));
    assert!(actor
        .authorize_response_publication_at(
            published_at,
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .is_ok());
    assert_eq!(
        actor.producer_progress_deadline,
        Some(advancing),
        "first media must not create an unmonitored published interval"
    );

    assert!(actor.settle_due_deadlines_at(advancing.instant).is_some());
    let retained = actor.pending_decision.clone().expect("published failure");
    let ProducerDecision::Fail {
        decision_sequence: 1,
        failed_attempt: 1,
        reason: ProducerDecisionReason::ProgressDeadline,
        proposal: Some(proposal),
        cleanup,
    } = retained.as_ref()
    else {
        panic!("published deadline emits one retain-published failure");
    };
    assert!(uuid::Uuid::parse_str(&proposal.proposal_id).is_ok());
    assert_eq!(cleanup.cleanup_policy, CleanupPolicy::RetainPublished);
    assert!(actor.settle_due_deadlines_at(advancing.instant).is_none());
    assert_eq!(actor.pending_decision.as_ref(), Some(&retained));
    assert_eq!(
        actor.authorize_response_publication_at(
            advancing.instant + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted),
        "new playlist demand must lose to the retained producer verdict"
    );
    for object in [
        RollingResponseObject::MediaSegment,
        RollingResponseObject::ByteRange,
        RollingResponseObject::NotModified,
    ] {
        assert_eq!(
            actor.authorize_response_publication_at(
                advancing.instant + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(object, 1, Some(2)),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted),
            "numeric media beyond the frozen frontier must lose atomically"
        );
    }
    assert!(actor
        .authorize_response_publication_at(
            advancing.instant + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::InitializationSegment,
                1,
                None,
            ),
        )
        .is_ok());
    assert_eq!(
        actor.authorize_response_publication_at(
            advancing.instant + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                None,
            ),
        ),
        Err(ResponsePublicationRejection::InvalidBinding)
    );
    assert_eq!(
        actor.authorize_response_publication_at(
            advancing.instant + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::SubtitleSegment,
                1,
                Some(0),
            ),
        ),
        Err(ResponsePublicationRejection::InvalidBinding)
    );
    assert!(actor
        .authorize_response_publication_at(
            advancing.instant + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                Some(0),
            ),
        )
        .is_ok());
    let status = actor.producer_operational_snapshot_at(advancing.instant);
    assert!(status.producer_ended_with_proposal);
    assert_eq!(status.phase, "producer_ended_with_proposal");
    assert_eq!(
        status
            .proposal
            .as_ref()
            .map(|value| value.proposal_id.as_str()),
        Some(proposal.proposal_id.as_str())
    );
    assert!(!actor.producer_signal_authorized);
    assert!(actor.authorized_install.is_none());
    assert_eq!(
        actor.settle_executor_expected_at(),
        Err(ProducerAttemptRejection::DecisionMismatch),
        "an unexecuted RetainPublished decision cannot suppress executor loss"
    );
    assert_eq!(actor.decision_applied_at(1, None), Ok(()));
    assert_eq!(actor.settle_executor_expected_at(), Ok(()));
    actor.mark_executor_lost(advancing.instant + Duration::from_secs(1));
    let settled = actor.producer_operational_snapshot_at(advancing.instant);
    assert_eq!(settled.executor_state, "settled");
    assert_eq!(settled.executor_last_action_failure, None);
    assert!(!actor.executor_lost);
}

#[test]
fn post_failure_observation_cannot_advance_frozen_published_frontier() {
    let started = Instant::now();
    let published_at = started + Duration::from_secs(2);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-frozen-frontier", "recipe-frozen-frontier"),
        ),
        Ok(1)
    );
    assert!(actor.observe_publication_at(
        published_at - Duration::from_millis(1),
        RollingPublicationObservation {
            producer_attempt: 1,
            publication_commit: true,
            demand_sequence: None,
            produced_segment: Some(1),
            produced_end_ms: Some(12_000),
            playlist_ready: true,
            published_segment: Some(1),
            published_end_ms: Some(12_000),
            published_first_segment: Some(0),
            published_start_ms: Some(0),
            media_origin_ms: 0,
            next_media_sequence: 2,
            resolved_fetched_segment: None,
            resolved_fetched_end_ms: None,
        },
    ));
    assert!(actor
        .authorize_response_publication_at(
            published_at,
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .is_ok());

    let deadline = actor
        .producer_progress_deadline
        .expect("published producer keeps its exact deadline");
    assert!(actor.settle_due_deadlines_at(deadline.instant).is_some());
    assert!(matches!(
        actor.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            proposal: Some(_),
            cleanup: ProducerFailureCleanup {
                cleanup_policy: CleanupPolicy::RetainPublished,
                ..
            },
            ..
        })
    ));
    let frozen = actor.delivery.clone();

    assert!(
        !actor.observe_publication_at(
            deadline.instant + Duration::from_millis(1),
            RollingPublicationObservation {
                producer_attempt: 1,
                publication_commit: true,
                demand_sequence: None,
                produced_segment: Some(2),
                produced_end_ms: Some(18_000),
                playlist_ready: true,
                published_segment: Some(2),
                published_end_ms: Some(18_000),
                published_first_segment: Some(0),
                published_start_ms: Some(0),
                media_origin_ms: 0,
                next_media_sequence: 3,
                resolved_fetched_segment: None,
                resolved_fetched_end_ms: None,
            },
        ),
        "a playlist observation sequenced after Fail must lose"
    );
    assert_eq!(actor.delivery.published_segment, frozen.published_segment);
    assert_eq!(actor.delivery.published_end_ms, frozen.published_end_ms);
    assert_eq!(
        actor.delivery.next_media_sequence,
        frozen.next_media_sequence
    );
    assert_eq!(
        actor.authorize_response_publication_at(
            deadline.instant + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                Some(2),
            ),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted)
    );
    assert!(actor
        .authorize_response_publication_at(
            deadline.instant + Duration::from_millis(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                Some(1),
            ),
        )
        .is_ok());
}

#[test]
fn copy_retry_is_unsupported_only_and_successor_classifier_is_recipe_owned() {
    let started = Instant::now();
    let classified_at = started + Duration::from_secs(1);
    let mut unsupported = registered_prepublication_actor(started);
    assert_eq!(
        unsupported.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-copy-retry", "recipe-copy-retry"),
        ),
        Ok(1)
    );
    assert_eq!(
        unsupported.classify_copy_producer_exit_at(
            classified_at,
            1,
            CopyProducerExitClassification::Unsupported,
        ),
        Ok(())
    );
    let Some(ProducerDecision::Retry {
        decision_sequence: 1,
        failed_attempt: 1,
        recipe,
        reason: ProducerDecisionReason::Unsupported,
    }) = unsupported.pending_decision.as_deref()
    else {
        panic!("unsupported copy input must select the one validated fallback");
    };
    assert_eq!(recipe.startup_kind, ProducerStartupKind::Copy);
    assert_eq!(recipe.exit_classifier, ProducerExitClassifier::Immediate);
    assert_eq!(
        unsupported.classify_copy_producer_exit_at(
            classified_at + Duration::from_nanos(1),
            1,
            CopyProducerExitClassification::Unsupported,
        ),
        Err(ProducerAttemptRejection::DuplicateClassification)
    );
    assert_eq!(
        unsupported.admit_producer_retry_at(classified_at, 1, "recipe-copy-retry"),
        Ok(2)
    );
    assert_eq!(
        unsupported.producer_exit_classifier,
        ProducerExitClassifier::Immediate,
        "the fallback attempt must use its frozen recipe classifier"
    );
    assert_eq!(unsupported.decision_applied_at(1, Some(2)), Ok(()));
    assert_eq!(
        unsupported.classify_copy_producer_exit_at(
            classified_at + Duration::from_nanos(2),
            2,
            CopyProducerExitClassification::Unsupported,
        ),
        Err(ProducerAttemptRejection::ClassifierMismatch),
        "the legacy retry cannot mint another copy-reader retry"
    );
    let retry_exit_at = classified_at + Duration::from_nanos(3);
    assert_eq!(
        unsupported.observe_producer_exit_at(
            retry_exit_at,
            RollingProducerExitObservation {
                producer_attempt: 2,
                success: false,
                code: Some(1),
                signal: None,
                observed_at: retry_exit_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    // `observe_producer_exit_at` arms the exit as due; the decision is
    // minted by the next actor transaction, which in production is the
    // next command, block report or publication authorization.
    let _ = unsupported.settle_due_deadlines_at(retry_exit_at);
    assert!(matches!(
        unsupported.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            decision_sequence: 2,
            failed_attempt: 2,
            reason: ProducerDecisionReason::ProcessExit,
            ..
        })
    ));
    assert_eq!(
        unsupported.admit_producer_retry_at(retry_exit_at, 2, "recipe-copy-retry"),
        Err(ProducerAttemptRejection::RetryUnavailable)
    );
    for (classification, reason) in [
        (
            CopyProducerExitClassification::InvalidConfiguration,
            ProducerDecisionReason::InvalidConfiguration,
        ),
        (
            CopyProducerExitClassification::ReaderFailed,
            ProducerDecisionReason::ReaderFailed,
        ),
    ] {
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                copy_policy("presentation-copy-fail", "recipe-copy-fail"),
            ),
            Ok(1)
        );
        assert_eq!(
            actor.classify_copy_producer_exit_at(classified_at, 1, classification),
            Ok(())
        );
        assert!(matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                decision_sequence: 1,
                failed_attempt: 1,
                reason: actual_reason,
                ..
            }) if *actual_reason == reason
        ));
    }

    let mut unsupported_without_fallback = registered_prepublication_actor(started);
    assert_eq!(
        unsupported_without_fallback.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::copy(
                "presentation-copy-no-fallback".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
                None,
            ),
        ),
        Ok(1)
    );
    assert_eq!(
        unsupported_without_fallback.classify_copy_producer_exit_at(
            classified_at,
            1,
            CopyProducerExitClassification::Unsupported,
        ),
        Ok(())
    );
    assert!(matches!(
        unsupported_without_fallback.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            reason: ProducerDecisionReason::Unsupported,
            ..
        })
    ));

    let mut startup_timeout = registered_prepublication_actor(started);
    assert_eq!(
        startup_timeout.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-copy-timeout", "recipe-copy-timeout"),
        ),
        Ok(1)
    );
    assert!(startup_timeout
        .settle_due_deadlines_at(started + PREPUBLICATION_COPY_STARTUP_BUDGET)
        .is_some());
    assert!(matches!(
        startup_timeout.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            reason: ProducerDecisionReason::StartupDeadline,
            ..
        })
    ));
}

#[test]
fn copy_unsupported_and_nonzero_exit_use_actor_ingress_order() {
    let started = Instant::now();
    let first_fact_at = started + Duration::from_secs(1);
    let second_fact_at = started + Duration::from_secs(2);

    let mut unsupported_first = registered_prepublication_actor(started);
    assert_eq!(
        unsupported_first.begin_initial_producer_attempt_at(
            started,
            copy_policy(
                "presentation-copy-unsupported-first",
                "recipe-copy-unsupported-first"
            ),
        ),
        Ok(1)
    );
    assert_eq!(
        unsupported_first.classify_copy_producer_exit_at(
            first_fact_at,
            1,
            CopyProducerExitClassification::Unsupported,
        ),
        Ok(())
    );
    assert_eq!(
        unsupported_first.observe_producer_exit_at(
            second_fact_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(32),
                signal: None,
                observed_at: second_fact_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert!(matches!(
        unsupported_first.pending_decision.as_deref(),
        Some(ProducerDecision::Retry {
            reason: ProducerDecisionReason::Unsupported,
            ..
        })
    ));

    let mut process_first = registered_prepublication_actor(started);
    assert_eq!(
        process_first.begin_initial_producer_attempt_at(
            started,
            copy_policy(
                "presentation-copy-process-first",
                "recipe-copy-process-first"
            ),
        ),
        Ok(1)
    );
    assert_eq!(
        process_first.observe_producer_exit_at(
            first_fact_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(32),
                signal: None,
                observed_at: first_fact_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert_eq!(
        process_first.classify_copy_producer_exit_at(
            second_fact_at,
            1,
            CopyProducerExitClassification::Unsupported,
        ),
        Err(ProducerAttemptRejection::DecisionMismatch)
    );
    assert!(matches!(
        process_first.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            reason: ProducerDecisionReason::ProcessExit,
            ..
        })
    ));
}

#[test]
fn copy_unsupported_retry_closes_only_after_actor_media_admission() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            copy_policy(
                "presentation-copy-published-unsupported",
                "recipe-copy-published-unsupported",
            ),
        ),
        Ok(1)
    );
    assert!(actor
        .authorize_response_publication_at(
            started + Duration::from_millis(500),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .is_ok());
    assert_eq!(
        actor.classify_copy_producer_exit_at(
            started + Duration::from_secs(1),
            1,
            CopyProducerExitClassification::Unsupported,
        ),
        Ok(())
    );
    assert!(matches!(
        actor.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            failed_attempt: 1,
            reason: ProducerDecisionReason::Unsupported,
            proposal: Some(_),
            cleanup: ProducerFailureCleanup {
                cleanup_policy: CleanupPolicy::RetainPublished,
                ..
            },
            ..
        })
    ));
}

#[test]
fn copy_completed_and_success_exit_rendezvous_under_the_first_facts_deadline() {
    let started = Instant::now();
    let first_fact_at = started + Duration::from_secs(1);
    let second_fact_at = started + Duration::from_secs(2);

    let mut completed_first = registered_prepublication_actor(started);
    assert_eq!(
        completed_first.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-copy-complete-a", "recipe-copy-complete-a"),
        ),
        Ok(1)
    );
    assert_eq!(
        completed_first.classify_copy_producer_exit_at(
            first_fact_at,
            1,
            CopyProducerExitClassification::Completed,
        ),
        Ok(())
    );
    let first_deadline = first_fact_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
    assert_eq!(
        completed_first.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 1,
            mode: ProducerProgressDeadlineMode::ClassifyingExit,
            instant: first_deadline,
        })
    );
    assert_eq!(
        completed_first.poll_producer_decision_at(0),
        ProducerDecisionPoll::Idle,
        "Completed alone is not a natural-exit proof"
    );
    assert!(completed_first.observe_producer_progress_at(
        first_fact_at + Duration::from_millis(100),
        producer_progress(
            1,
            1_000,
            1_000,
            1_000,
            first_fact_at + Duration::from_millis(100),
        ),
    ));
    assert_eq!(
        completed_first
            .producer_progress_deadline
            .map(|deadline| deadline.instant),
        Some(first_deadline),
        "progress cannot restart a classification rendezvous"
    );
    assert_eq!(
        completed_first.observe_producer_exit_at(
            second_fact_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: true,
                code: Some(0),
                signal: None,
                observed_at: second_fact_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    let ProducerDecisionPoll::ClassifyExit(completed_first_probe) =
        completed_first.poll_producer_decision_at(0)
    else {
        panic!("both copy exit halves must publish one completion probe");
    };
    assert_eq!(completed_first_probe.deadline, first_deadline);

    let mut process_first = registered_prepublication_actor(started);
    assert_eq!(
        process_first.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-copy-complete-b", "recipe-copy-complete-b"),
        ),
        Ok(1)
    );
    assert_eq!(
        process_first.observe_producer_exit_at(
            first_fact_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: true,
                code: Some(0),
                signal: None,
                observed_at: first_fact_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert_eq!(
        process_first.poll_producer_decision_at(0),
        ProducerDecisionPoll::Idle,
        "a successful copy process exit still needs Completed"
    );
    assert_eq!(
        process_first.classify_copy_producer_exit_at(
            second_fact_at,
            1,
            CopyProducerExitClassification::Completed,
        ),
        Ok(())
    );
    let ProducerDecisionPoll::ClassifyExit(process_first_probe) =
        process_first.poll_producer_decision_at(0)
    else {
        panic!("Completed must rendezvous with the retained successful exit");
    };
    assert_eq!(
        process_first_probe.deadline,
        first_fact_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET,
        "the second half cannot mint a fresh classifier budget"
    );
}

#[test]
fn copy_nonzero_exit_wins_completed_and_a_missing_half_times_out() {
    let started = Instant::now();
    let first_fact_at = started + Duration::from_secs(1);
    let second_fact_at = started + Duration::from_secs(2);
    let mut nonzero = registered_prepublication_actor(started);
    assert_eq!(
        nonzero.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-copy-nonzero", "recipe-copy-nonzero"),
        ),
        Ok(1)
    );
    assert_eq!(
        nonzero.classify_copy_producer_exit_at(
            first_fact_at,
            1,
            CopyProducerExitClassification::Completed,
        ),
        Ok(())
    );
    assert_eq!(
        nonzero.observe_producer_exit_at(
            second_fact_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(9),
                signal: None,
                observed_at: second_fact_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert!(nonzero.maybe_commit_producer_decision_at(second_fact_at));
    assert!(matches!(
        nonzero.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            reason: ProducerDecisionReason::ProcessExit,
            ..
        })
    ));

    let mut nonzero_first = registered_prepublication_actor(started);
    assert_eq!(
        nonzero_first.begin_initial_producer_attempt_at(
            started,
            copy_policy(
                "presentation-copy-nonzero-first",
                "recipe-copy-nonzero-first",
            ),
        ),
        Ok(1)
    );
    assert_eq!(
        nonzero_first.observe_producer_exit_at(
            first_fact_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(7),
                signal: None,
                observed_at: first_fact_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert_eq!(
        nonzero_first.classify_copy_producer_exit_at(
            second_fact_at,
            1,
            CopyProducerExitClassification::Completed,
        ),
        Err(ProducerAttemptRejection::DecisionMismatch)
    );
    assert!(matches!(
        nonzero_first.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            reason: ProducerDecisionReason::ProcessExit,
            ..
        })
    ));

    for completed_first in [false, true] {
        let mut missing = registered_prepublication_actor(started);
        assert_eq!(
            missing.begin_initial_producer_attempt_at(
                started,
                copy_policy("presentation-copy-missing", "recipe-copy-missing"),
            ),
            Ok(1)
        );
        if completed_first {
            assert_eq!(
                missing.classify_copy_producer_exit_at(
                    first_fact_at,
                    1,
                    CopyProducerExitClassification::Completed,
                ),
                Ok(())
            );
        } else {
            assert_eq!(
                missing.observe_producer_exit_at(
                    first_fact_at,
                    RollingProducerExitObservation {
                        producer_attempt: 1,
                        success: true,
                        code: Some(0),
                        signal: None,
                        observed_at: first_fact_at,
                    },
                ),
                ProducerExitAcceptance::Accepted
            );
        }
        assert!(missing
            .settle_due_deadlines_at(first_fact_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET)
            .is_some());
        assert!(matches!(
            missing.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::ExitClassificationDeadline,
                ..
            })
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn bounded_copy_classification_is_sequenced_after_preceding_process_exit() {
    let (handle, mut registration) =
        RollingControlHandle::spawn_prepublication_producer_for_test("copy-classification");
    assert_eq!(registration.register().await, Ok(()));
    assert_eq!(
        handle
            .begin_initial_producer_attempt(copy_policy(
                "presentation-copy-sequenced",
                "recipe-copy-sequenced",
            ))
            .await,
        Ok(1)
    );
    handle.observe_producer_exit(1, true, Some(0), None);
    assert_eq!(
        handle
            .classify_copy_producer_exit_before(
                1,
                CopyProducerExitClassification::Completed,
                rolling_now() + Duration::from_secs(1),
            )
            .await,
        Ok(())
    );
    let RollingProducerExecutorPoll::ClassifyExit(probe) = registration.next_decision().await
    else {
        panic!("the sealed copy result must observe the preceding successful exit");
    };
    assert_eq!(probe.producer_attempt, 1);
    assert!(probe.deadline > rolling_now());
}

#[test]
fn successful_exit_requires_attempt_fenced_completion_before_classification_deadline() {
    let started = Instant::now();
    let exit_at = started + Duration::from_secs(2);
    let mut actor = registered_prepublication_actor(started);
    let policy = InitialProducerPolicy::software(
        "presentation-complete".to_owned(),
        PRODUCER_PROGRESS_BUDGET,
    )
    .with_completion_expectation(Some(20_000), 10_000);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(started, policy),
        Ok(1)
    );
    assert!(actor.observe_producer_progress_at(
        started + Duration::from_secs(1),
        RollingProducerProgressObservation {
            producer_attempt: 1,
            out_time_ms: Some(10_000),
            speed_milli: None,
            recent_speed_milli: None,
            observed_at: started + Duration::from_secs(1),
        },
    ));
    assert!(actor
        .authorize_response_publication_at(
            started + Duration::from_millis(1500),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .is_ok());
    assert_eq!(
        actor.observe_producer_exit_at(
            exit_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: true,
                code: Some(0),
                signal: None,
                observed_at: exit_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    let ProducerDecisionPoll::ClassifyExit(probe) = actor.poll_producer_decision_at(0) else {
        panic!("successful exit must wake one exact classifier probe");
    };
    assert_eq!(probe.producer_attempt, 1);
    assert_eq!(
        probe.deadline,
        exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET
    );
    assert_eq!(
        actor.classify_producer_exit_at(
            exit_at + Duration::from_millis(1),
            RollingProducerCompletionEvidence {
                probe_sequence: probe.probe_sequence,
                producer_attempt: 1,
                end_list: true,
                final_segment: Some(1),
                final_end_ms: Some(20_000),
            },
        ),
        RollingProducerCompletionDisposition::CompleteVerifiedDuration
    );
    assert_eq!(
        actor.poll_producer_decision_at(0),
        ProducerDecisionPoll::Idle
    );
    assert!(actor.pending_decision.is_none());
    let status = actor.producer_operational_snapshot_at(exit_at + Duration::from_secs(1));
    assert_eq!(status.phase, "complete");
    assert_eq!(status.completion, "complete_verified_duration");
    assert_eq!(status.completion_attempt, Some(1));
    assert_eq!(status.completion_final_segment, Some(1));
    assert_eq!(status.completion_final_end_ms, Some(20_000));
    assert!(!status.producer_ended_with_proposal);
    assert_eq!(status.last_probe_outcome, "published");
    assert!(!actor.observe_publication_at(
        exit_at + Duration::from_millis(2),
        RollingPublicationObservation {
            producer_attempt: 1,
            publication_commit: true,
            demand_sequence: None,
            produced_segment: Some(2),
            produced_end_ms: Some(26_000),
            playlist_ready: true,
            published_segment: Some(2),
            published_end_ms: Some(26_000),
            published_first_segment: Some(0),
            published_start_ms: Some(0),
            media_origin_ms: 0,
            next_media_sequence: 3,
            resolved_fetched_segment: None,
            resolved_fetched_end_ms: None,
        },
    ));
    assert_eq!(
        actor.classify_producer_exit_at(
            exit_at + Duration::from_millis(3),
            RollingProducerCompletionEvidence {
                probe_sequence: probe.probe_sequence,
                producer_attempt: 1,
                end_list: false,
                final_segment: None,
                final_end_ms: None,
            },
        ),
        RollingProducerCompletionDisposition::Stale
    );
    assert_eq!(
        actor
            .producer_operational_snapshot_at(exit_at + Duration::from_secs(1))
            .completion,
        "complete_verified_duration"
    );
    assert_eq!(
        actor
            .producer_operational_snapshot_at(exit_at + Duration::from_secs(1))
            .last_probe_outcome,
        "published"
    );
    assert_eq!(actor.settle_executor_expected_at(), Ok(()));
    actor.mark_executor_lost(exit_at + Duration::from_secs(2));
    let settled = actor.producer_operational_snapshot_at(exit_at + Duration::from_secs(2));
    assert_eq!(settled.executor_state, "settled");
    assert_eq!(settled.executor_last_action_failure, None);
    assert!(!actor.executor_lost);
}

#[test]
fn missing_completion_evidence_yields_one_retain_published_proposal() {
    let started = Instant::now();
    let exit_at = started + Duration::from_secs(1);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::software(
                "presentation-partial".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
            ),
        ),
        Ok(1)
    );
    assert!(actor
        .authorize_response_publication_at(
            started + Duration::from_millis(500),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .is_ok());
    assert_eq!(
        actor.observe_producer_exit_at(
            exit_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: true,
                code: Some(0),
                signal: None,
                observed_at: exit_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    let deadline = exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
    assert!(actor.settle_due_deadlines_at(deadline).is_some());
    let retained = actor
        .pending_decision
        .clone()
        .expect("classification failure");
    assert!(matches!(
        retained.as_ref(),
        ProducerDecision::Fail {
            reason: ProducerDecisionReason::ExitClassificationDeadline,
            proposal: Some(_),
            cleanup: ProducerFailureCleanup {
                cleanup_policy: CleanupPolicy::RetainPublished,
                ..
            },
            ..
        }
    ));
    assert_eq!(
        actor
            .prepublication
            .as_ref()
            .expect("producer control")
            .last_probe_outcome,
        ProducerProbeOutcome::Cancelled
    );
    assert_eq!(actor.pending_decision.as_ref(), Some(&retained));
}

#[test]
fn incomplete_exit_evidence_fails_once_without_waiting_for_the_deadline() {
    let started = Instant::now();
    let exit_at = started + Duration::from_secs(1);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::software(
                "presentation-partial-now".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
            ),
        ),
        Ok(1)
    );
    assert!(actor
        .authorize_response_publication_at(
            started + Duration::from_millis(500),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::VideoMediaPlaylist,
                1,
                None,
            ),
        )
        .is_ok());
    assert_eq!(
        actor.observe_producer_exit_at(
            exit_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: true,
                code: Some(0),
                signal: None,
                observed_at: exit_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    let ProducerDecisionPoll::ClassifyExit(probe) = actor.poll_producer_decision_at(0) else {
        panic!("successful exit must expose its exact evidence request");
    };
    assert_eq!(
        actor.classify_producer_exit_at(
            exit_at + Duration::from_millis(1),
            RollingProducerCompletionEvidence {
                probe_sequence: probe.probe_sequence,
                producer_attempt: 1,
                end_list: false,
                final_segment: Some(0),
                final_end_ms: Some(6_000),
            },
        ),
        RollingProducerCompletionDisposition::FailedPartialSuccess
    );
    let retained = actor
        .pending_decision
        .clone()
        .expect("partial success verdict");
    assert!(matches!(
        retained.as_ref(),
        ProducerDecision::Fail {
            decision_sequence: 1,
            failed_attempt: 1,
            reason: ProducerDecisionReason::PartialSuccessExit,
            proposal: Some(_),
            cleanup: ProducerFailureCleanup {
                cleanup_policy: CleanupPolicy::RetainPublished,
                ..
            },
        }
    ));
    assert_eq!(
        actor.poll_producer_decision_at(0),
        ProducerDecisionPoll::Decision(retained)
    );
}

#[tokio::test]
async fn generation_and_protocol_responses_are_due_first_at_exact_deadline() {
    let started = rolling_now();
    let deadline = started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET;
    for publication in [
        RollingResponsePublication::generation_metadata(
            RollingResponseObject::MasterPlaylist,
            "presentation-due-first".to_owned(),
        ),
        RollingResponsePublication::protocol_only(RollingResponseObject::ProtocolResponse, 1),
    ] {
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::software(
                    "presentation-due-first".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                ),
            ),
            Ok(1)
        );
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: deadline,
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::AuthorizeResponsePublication {
                    publication,
                    handoff: None,
                    deadline: rolling_now() + Duration::from_secs(1),
                    reply,
                },
            })
            .await;
        assert_eq!(
            response.await.expect("response admission reply"),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );
        assert!(matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::StartupDeadline,
                ..
            })
        ));
    }
}

#[tokio::test]
async fn cancelled_attempt_media_admission_cannot_mutate_or_win_the_deadline() {
    let started = rolling_now();
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-cancelled", "recipe-cancelled"),
        ),
        Ok(1)
    );
    let handoff = RollingFirstMediaPublicationHandoff::new();
    let handoff_result = handoff.waiter();
    let (reply, response) = tokio::sync::oneshot::channel();
    drop(response);
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 1,
            published_at: deadline,
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::AuthorizeResponsePublication {
                publication: RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
                handoff: Some(handoff),
                deadline: rolling_now() + Duration::from_secs(1),
                reply,
            },
        })
        .await;
    assert!(!handoff_result.is_accepted());
    assert!(
        !actor
            .prepublication
            .as_ref()
            .expect("pre-publication state")
            .producer_media_published
    );
    assert!(matches!(
        actor.pending_decision.as_deref(),
        Some(ProducerDecision::Retry {
            reason: ProducerDecisionReason::StartupDeadline,
            ..
        })
    ));
}

#[tokio::test]
async fn cancelled_or_expired_response_commits_do_not_mutate_actor_delivery() {
    let started = rolling_now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");
    let before = actor.snapshot_at(started);

    let (reply, response) = tokio::sync::oneshot::channel();
    drop(response);
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 1,
            published_at: started + Duration::from_millis(1),
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::CommitMedia {
                kind: "cancelled-response-eof",
                producer_attempt: attempt,
                segment_index: Some(7),
                segment_end_ms: Some(28_000),
                handoff: None,
                deadline: rolling_now() + Duration::from_secs(1),
                reply,
            },
        })
        .await;

    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 2,
            published_at: started + Duration::from_millis(2),
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::CommitMedia {
                kind: "expired-response-eof",
                producer_attempt: attempt,
                segment_index: Some(8),
                segment_end_ms: Some(32_000),
                handoff: None,
                deadline: rolling_now() - Duration::from_millis(1),
                reply,
            },
        })
        .await;
    assert!(!response.await.expect("expired commit reply"));
    let after = actor.snapshot_at(started + Duration::from_millis(2));
    assert_eq!(after.last_renewal_kind, before.last_renewal_kind);
    assert_eq!(
        after.delivery.fetched_segment,
        before.delivery.fetched_segment
    );
    assert_eq!(
        after.delivery.fetched_end_ms,
        before.delivery.fetched_end_ms
    );
}

#[tokio::test]
async fn accepted_segment_eof_projects_frontier_and_flow_before_reply() {
    let started = rolling_now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");
    let compatibility_attempt = Arc::new(std::sync::Mutex::new(attempt));
    let high_segment = Arc::new(AtomicI64::new(-1));
    let fetched_end_ms = Arc::new(AtomicI64::new(0));
    let flow_sync = Arc::new(RollingFlowSync::new());
    let handoff = RollingMediaCommitHandoff {
        producer_attempt: attempt,
        segment_index: 7,
        segment_end_ms: Some(28_000),
        compatibility_attempt,
        high_segment: Arc::clone(&high_segment),
        fetched_end_ms: Arc::clone(&fetched_end_ms),
        flow_sync: Arc::clone(&flow_sync),
    };
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 1,
            published_at: started + Duration::from_millis(1),
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::CommitMedia {
                kind: "accepted-response-eof",
                producer_attempt: attempt,
                segment_index: Some(7),
                segment_end_ms: Some(28_000),
                handoff: Some(handoff),
                deadline: rolling_now() + Duration::from_secs(1),
                reply,
            },
        })
        .await;

    assert_eq!(high_segment.load(Ordering::Acquire), 7);
    assert_eq!(fetched_end_ms.load(Ordering::Acquire), 28_000);
    assert_eq!(flow_sync.requested.load(Ordering::Acquire), 1);
    drop(response);
    assert_eq!(high_segment.load(Ordering::Acquire), 7);
    assert_eq!(
        actor
            .snapshot_at(started + Duration::from_millis(1))
            .delivery
            .fetched_segment,
        Some(7)
    );
}

#[tokio::test]
async fn first_media_deadline_rejects_and_fences_a_late_handoff() {
    let (control, mut registration) =
        RollingControlHandle::spawn_prepublication_transcode_for_test("first-media-deadline");
    assert_eq!(registration.register().await, Ok(()));
    assert_eq!(
        control
            .begin_initial_producer_attempt(hardware_policy(
                "presentation-first-media-deadline",
                "recipe-first-media-deadline",
            ))
            .await,
        Ok(1)
    );
    let handoff = RollingFirstMediaPublicationHandoff::new();
    let waiter = handoff.waiter();

    assert!(!control.settle_first_media_at_deadline(&waiter));
    assert!(control.is_retired());
    handoff.settle_for_test(true);
    assert_eq!(waiter.settled_outcome(), Some(false));

    // This represents a response that already resolved the exact owner
    // and reached actor admission after the request ahead of it timed out.
    // The next actor command must import the external retirement fence,
    // not let a fresh handoff revive first-media publication.
    let follower_handoff = RollingFirstMediaPublicationHandoff::new();
    let follower_waiter = follower_handoff.waiter();
    assert_eq!(
        control
            .authorize_response_publication(
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
                Some(follower_handoff),
                rolling_now() + Duration::from_secs(1),
            )
            .await,
        Err(ResponsePublicationRejection::SessionEnded)
    );
    assert_eq!(follower_waiter.settled_outcome(), Some(false));

    let Ok(RollingExpiryClaim::Retired(snapshot)) = control.claim_expiry().await else {
        panic!("external first-media retirement must become actor terminal state");
    };
    assert_eq!(
        snapshot.terminal,
        Some(RollingTerminalCause::AuthorityFence)
    );
    assert!(!snapshot.producer_control.producer_media_published);
}

#[tokio::test]
async fn terminal_queued_before_deadline_settles_after_deadline_without_reclassification() {
    let handle = RollingControlHandle::spawn_for_test("queued-terminal-deadline");
    let pause = crate::seam_hooks::AsyncPause::new("producer attempt reply");
    handle.pause_producer_attempt_reply(Arc::clone(&pause));
    let begin = tokio::spawn({
        let handle = handle.clone();
        async move { handle.begin_producer_attempt().await }
    });
    let held = pause.reached().await;

    let deadline = rolling_now() + Duration::from_millis(100);
    let ending = tokio::spawn({
        let handle = handle.clone();
        async move { handle.end_before(deadline).await }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while handle.sender.capacity() == ROLLING_ACTOR_MAILBOX_CAPACITY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Terminal must enter the mailbox before its deadline");
    tokio::time::sleep_until(tokio::time::Instant::from_std(
        deadline + Duration::from_millis(20),
    ))
    .await;
    assert!(
        !ending.is_finished(),
        "an admitted Terminal waits for actor settlement instead of timing out"
    );

    held.release();
    assert!(begin.await.expect("begin task").is_ok());
    assert_eq!(
        ending.await.expect("End task"),
        Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
    );
}

#[tokio::test]
async fn cancelled_or_expired_http_observations_do_not_mutate_actor_delivery() {
    let started = rolling_now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("producer attempt");
    let observed_at = started + Duration::from_millis(2);
    let before = actor.snapshot_at(observed_at).delivery;

    let (reply, response) = tokio::sync::oneshot::channel();
    drop(response);
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 1,
            published_at: started + Duration::from_millis(1),
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::ObservePublication {
                observation: publication(attempt, true, 7, 28_000, None),
                deadline: Some(rolling_now() + Duration::from_secs(1)),
                reply,
            },
        })
        .await;

    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 2,
            published_at: started + Duration::from_millis(2),
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::ObservePublication {
                observation: publication(attempt, true, 8, 32_000, None),
                deadline: Some(rolling_now() - Duration::from_millis(1)),
                reply,
            },
        })
        .await;
    assert!(!response.await.expect("expired observation reply"));
    assert_eq!(actor.snapshot_at(observed_at).delivery, before);
}

#[tokio::test]
async fn cancellation_after_attempt_media_mutation_keeps_the_synchronous_handoff() {
    let started = rolling_now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-handoff", "recipe-handoff"),
        ),
        Ok(1)
    );
    let handoff = RollingFirstMediaPublicationHandoff::new();
    let handoff_result = handoff.waiter();
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 1,
            published_at: started + Duration::from_secs(1),
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::AuthorizeResponsePublication {
                publication: RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
                handoff: Some(handoff),
                deadline: rolling_now() + Duration::from_secs(1),
                reply,
            },
        })
        .await;
    drop(response);
    assert!(handoff_result.is_accepted());
    assert!(
        actor
            .prepublication
            .as_ref()
            .expect("pre-publication state")
            .producer_media_published
    );
    assert!(actor.pending_decision.is_none());
}

#[tokio::test]
async fn applied_failure_consumes_a_preceding_exit_and_replays_permanently() {
    let started = rolling_now();
    let exit_at = started + Duration::from_secs(1);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy::software(
                "presentation-applied-fail".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
            ),
        ),
        Ok(1)
    );
    let exit = RollingProducerIngressBlock::Barrier(SequencedProducerBarrier {
        preceding_progress: None,
        event: SequencedProducerEvent {
            sequence: 1,
            published_at: exit_at,
            event: RollingProducerEvent::Exit(RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(1),
                signal: None,
                observed_at: exit_at,
            }),
        },
    });
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 2,
            published_at: exit_at,
            preceding_producer: vec![exit],
            sealed_flow_barriers: 0,
            command: RollingControlCommand::DecisionApplied {
                decision_sequence: 1,
                installed_attempt: None,
                reply,
            },
        })
        .await;
    assert_eq!(response.await.expect("decision applied reply"), Ok(()));
    assert_eq!(actor.decision_applied_at(1, None), Ok(()));
    assert!(actor.pending_decision.is_none());
    assert!(actor.producer_progress_deadline.is_none());
    assert!(actor.producer_deadline_due.is_none());
    assert!(actor.producer_process_exit_due.is_none());
    assert!(
        actor
            .prepublication
            .as_ref()
            .expect("pre-publication state")
            .failure_applied
    );
    actor.arm_producer_deadline(
        1,
        ProducerProgressDeadlineMode::Advancing,
        exit_at + PRODUCER_PROGRESS_BUDGET,
    );
    assert!(actor.producer_progress_deadline.is_none());
    assert_eq!(
        actor.authorize_response_publication_at(
            exit_at,
            RollingResponsePublication::protocol_only(RollingResponseObject::ProtocolResponse, 1,),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted)
    );
    assert_eq!(
        actor.authorize_response_publication_at(
            exit_at,
            RollingResponsePublication::attempt_status(RollingResponseObject::MediaSegment, 1,),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted)
    );
}

#[test]
fn attempt_media_and_retry_are_exact_first_winner_races() {
    let started = Instant::now();
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;

    let mut decision_first = registered_prepublication_actor(started);
    assert_eq!(
        decision_first.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-race-a", "recipe-race-a"),
        ),
        Ok(1)
    );
    assert!(decision_first.settle_due_deadlines_at(deadline).is_some());
    assert_eq!(
        decision_first.authorize_response_publication_at(
            deadline,
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                Some(0),
            ),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted)
    );

    let mut publication_late = registered_prepublication_actor(started);
    assert_eq!(
        publication_late.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-race-b", "recipe-race-b"),
        ),
        Ok(1)
    );
    assert_eq!(
        publication_late.authorize_response_publication_at(
            deadline + Duration::from_nanos(1),
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                Some(0),
            ),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted)
    );
    assert!(matches!(
        publication_late.pending_decision.as_deref(),
        Some(ProducerDecision::Retry { .. })
    ));
}

#[test]
fn retry_admission_and_application_are_exact_replay_fenced() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-retry", "recipe-retry"),
        ),
        Ok(1)
    );
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    assert!(actor.settle_due_deadlines_at(deadline).is_some());
    assert_eq!(
        actor.admit_producer_retry_at(deadline, 2, "recipe-retry"),
        Err(ProducerAttemptRejection::DecisionMismatch)
    );
    assert_eq!(
        actor.admit_producer_retry_at(deadline, 1, "wrong-recipe"),
        Err(ProducerAttemptRejection::RecipeMismatch)
    );
    actor.delivery.playlist_ready = true;
    assert_eq!(
        actor.admit_producer_retry_at(deadline, 1, "recipe-retry"),
        Ok(2),
        "catalog readiness is not client response publication"
    );
    assert_eq!(
        actor.admit_producer_retry_at(deadline, 1, "recipe-retry"),
        Ok(2),
        "exact retry admission is replayable"
    );
    assert_eq!(
        actor.admit_producer_retry_at(deadline, 1, "wrong-recipe"),
        Err(ProducerAttemptRejection::DecisionMismatch)
    );
    assert_eq!(
        actor.decision_applied_at(1, Some(1)),
        Err(ProducerAttemptRejection::DecisionMismatch)
    );
    assert_eq!(actor.decision_applied_at(1, Some(2)), Ok(()));
    assert_eq!(actor.decision_applied_at(1, Some(2)), Ok(()));
    assert_eq!(
        actor.decision_applied_at(1, None),
        Err(ProducerAttemptRejection::DecisionMismatch)
    );

    let invalid_recipe = retry_recipe("different-contract", "invalid-recipe");
    let mut invalid = registered_prepublication_actor(started);
    assert_eq!(
        invalid.begin_initial_producer_attempt_at(
            started,
            InitialProducerPolicy {
                startup_kind: ProducerStartupKind::Hardware,
                progress_budget: PRODUCER_PROGRESS_BUDGET,
                retry_recipe: Some(invalid_recipe),
                decode_alternate: None,
                presentation_contract_fingerprint: "presentation-retry".to_owned(),
                exit_classifier: ProducerExitClassifier::Immediate,
                retry_eligibility: ProducerRetryEligibility::AnyPrepublicationFailure,
                expected_remaining_ms: None,
                completion_tolerance_ms: 10_000,
            },
        ),
        Err(ProducerAttemptRejection::InvalidPolicy)
    );
}

#[test]
fn successor_failure_is_final_and_cannot_mint_a_second_retry() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-successor", "recipe-successor"),
        ),
        Ok(1)
    );
    let first_deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    assert!(actor.settle_due_deadlines_at(first_deadline).is_some());
    assert_eq!(
        actor.admit_producer_retry_at(first_deadline, 1, "recipe-successor"),
        Ok(2)
    );
    let successor_deadline = first_deadline + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET;
    let expected_successor_deadline = Some(ProducerProgressDeadline {
        producer_attempt: 2,
        mode: ProducerProgressDeadlineMode::Starting,
        instant: successor_deadline,
    });
    assert_eq!(
        actor.producer_progress_deadline, expected_successor_deadline,
        "the successor owns its startup clock while Retry remains pending"
    );
    assert_eq!(actor.decision_applied_at(1, Some(2)), Ok(()));
    assert_eq!(
        actor.producer_progress_deadline, expected_successor_deadline,
        "DecisionApplied must preserve the admitted successor clock"
    );
    assert!(actor.settle_due_deadlines_at(successor_deadline).is_some());
    assert!(matches!(
        actor.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            decision_sequence: 2,
            failed_attempt: 2,
            reason: ProducerDecisionReason::StartupDeadline,
            ..
        })
    ));
    assert_eq!(
        actor.admit_producer_retry_at(successor_deadline, 2, "recipe-successor"),
        Err(ProducerAttemptRejection::RetryUnavailable)
    );
    assert_eq!(actor.decision_applied_at(2, None), Ok(()));
    assert_eq!(actor.decision_applied_at(2, None), Ok(()));
}

#[tokio::test]
async fn successor_progress_before_retry_ack_advances_and_preserves_its_exact_deadline() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy(
                "presentation-successor-progress",
                "recipe-successor-progress"
            ),
        ),
        Ok(1)
    );
    let first_deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    assert!(actor.settle_due_deadlines_at(first_deadline).is_some());
    assert_eq!(
        actor.admit_producer_retry_at(first_deadline, 1, "recipe-successor-progress"),
        Ok(2)
    );

    let progressed_at = first_deadline + Duration::from_secs(1);
    actor.producer_events.publish_at(
        RollingProducerEvent::Progress(producer_progress(2, 1_000, 1_000, 1_000, progressed_at)),
        ProducerIngressSlot::Progress,
        progressed_at,
    );
    let preceding_producer = actor.producer_events.drain_blocks();
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 2,
            published_at: progressed_at,
            preceding_producer,
            sealed_flow_barriers: 0,
            command: RollingControlCommand::DecisionApplied {
                decision_sequence: 1,
                installed_attempt: Some(2),
                reply,
            },
        })
        .await;
    assert_eq!(response.await.expect("retry acknowledgement"), Ok(()));

    let advancing = Some(ProducerProgressDeadline {
        producer_attempt: 2,
        mode: ProducerProgressDeadlineMode::Advancing,
        instant: progressed_at + PRODUCER_PROGRESS_BUDGET,
    });
    assert_eq!(
        actor.producer_progress_deadline, advancing,
        "timely successor progress must replace Starting before Retry is acknowledged"
    );
    assert_eq!(actor.delivery.producer_out_time_ms, Some(1_000));
    assert!(!actor.observe_producer_progress_at(
        progressed_at + Duration::from_nanos(1),
        producer_progress(
            1,
            99_000,
            9_000,
            9_000,
            progressed_at + Duration::from_nanos(1),
        ),
    ));
    assert_eq!(
        actor.producer_progress_deadline, advancing,
        "stale predecessor progress cannot disturb the successor clock"
    );
}

#[tokio::test]
async fn successful_successor_exit_before_retry_ack_keeps_its_exact_classifier() {
    fn pending_successor(started: Instant, suffix: &str) -> (RollingControlActor, Instant) {
        let mut actor = registered_prepublication_actor(started);
        let presentation = format!("presentation-successor-success-{suffix}");
        let recipe = format!("recipe-successor-success-{suffix}");
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy(&presentation, &recipe),
            ),
            Ok(1)
        );
        let first_deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        assert!(actor.settle_due_deadlines_at(first_deadline).is_some());
        assert_eq!(
            actor.admit_producer_retry_at(first_deadline, 1, &recipe),
            Ok(2)
        );
        (actor, first_deadline)
    }

    async fn acknowledge_after_successful_exit(actor: &mut RollingControlActor, exit_at: Instant) {
        actor.producer_events.publish_at(
            RollingProducerEvent::Exit(RollingProducerExitObservation {
                producer_attempt: 2,
                success: true,
                code: Some(0),
                signal: None,
                observed_at: exit_at,
            }),
            ProducerIngressSlot::Exit,
            exit_at,
        );
        actor.producer_events.publish_at(
            RollingProducerEvent::Exit(RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(99),
                signal: None,
                observed_at: exit_at + Duration::from_nanos(1),
            }),
            ProducerIngressSlot::Exit,
            exit_at + Duration::from_nanos(1),
        );
        let preceding_producer = actor.producer_events.drain_blocks();
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 2,
                published_at: exit_at,
                preceding_producer,
                sealed_flow_barriers: 0,
                command: RollingControlCommand::DecisionApplied {
                    decision_sequence: 1,
                    installed_attempt: Some(2),
                    reply,
                },
            })
            .await;
        assert_eq!(response.await.expect("retry acknowledgement"), Ok(()));
    }

    let started = Instant::now();
    let (mut completed, first_deadline) = pending_successor(started, "complete");
    let exit_at = first_deadline + Duration::from_secs(1);
    acknowledge_after_successful_exit(&mut completed, exit_at).await;
    let classification_deadline = exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
    assert_eq!(
        completed.producer_progress_deadline,
        Some(ProducerProgressDeadline {
            producer_attempt: 2,
            mode: ProducerProgressDeadlineMode::ClassifyingExit,
            instant: classification_deadline,
        })
    );
    let ProducerDecisionPoll::ClassifyExit(probe) = completed.poll_producer_decision_at(0) else {
        panic!("successful successor exit must retain its exact classifier");
    };
    assert_eq!(probe.producer_attempt, 2);
    assert_eq!(probe.deadline, classification_deadline);
    assert_eq!(
        completed.observe_producer_exit_at(
            exit_at + Duration::from_nanos(2),
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(88),
                signal: None,
                observed_at: exit_at + Duration::from_nanos(2),
            },
        ),
        ProducerExitAcceptance::Rejected
    );
    assert_eq!(
        completed.poll_producer_decision_at(0),
        ProducerDecisionPoll::ClassifyExit(probe),
        "stale predecessor exit cannot replace the successor classifier"
    );
    assert_eq!(
        completed.classify_producer_exit_at(
            exit_at + Duration::from_millis(1),
            RollingProducerCompletionEvidence {
                probe_sequence: probe.probe_sequence,
                producer_attempt: 2,
                end_list: true,
                final_segment: Some(0),
                final_end_ms: Some(6_000),
            },
        ),
        RollingProducerCompletionDisposition::CompleteUnverifiedDuration
    );
    assert_eq!(
        completed.poll_producer_decision_at(0),
        ProducerDecisionPoll::Idle
    );
    assert_eq!(
        completed
            .prepublication
            .as_ref()
            .expect("producer control")
            .completion,
        ProducerCompletionState::CompleteUnverifiedDuration {
            producer_attempt: 2,
            final_segment: 0,
            final_end_ms: 6_000,
        }
    );

    let late_started = started + Duration::from_secs(1);
    let (mut late, late_first_deadline) = pending_successor(late_started, "deadline");
    let late_exit_at = late_first_deadline + Duration::from_secs(1);
    acknowledge_after_successful_exit(&mut late, late_exit_at).await;
    let late_classification_deadline = late_exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
    assert!(late
        .settle_due_deadlines_at(late_classification_deadline)
        .is_some());
    assert!(matches!(
        late.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            decision_sequence: 2,
            failed_attempt: 2,
            reason: ProducerDecisionReason::ExitClassificationDeadline,
            ..
        })
    ));
    assert_eq!(
        late.admit_producer_retry_at(
            late_classification_deadline,
            2,
            "recipe-successor-success-deadline",
        ),
        Err(ProducerAttemptRejection::RetryUnavailable)
    );
}

#[tokio::test]
async fn successor_exit_before_retry_ack_is_retained_as_the_final_decision() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-successor-exit", "recipe-successor-exit"),
        ),
        Ok(1)
    );
    let first_deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    assert!(actor.settle_due_deadlines_at(first_deadline).is_some());
    assert!(
        !actor.pending_retry_precedes_successor(1),
        "a pending Retry never exempts its failed predecessor"
    );
    assert_eq!(
        actor.admit_producer_retry_at(first_deadline, 1, "recipe-successor-exit"),
        Ok(2)
    );
    assert!(actor.pending_retry_precedes_successor(2));

    let exit_at = first_deadline + Duration::from_secs(1);
    assert_eq!(
        actor.observe_producer_exit_at(
            exit_at,
            RollingProducerExitObservation {
                producer_attempt: 2,
                success: false,
                code: Some(23),
                signal: None,
                observed_at: exit_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    let successor_exit = Some(ProducerProcessExitDue {
        producer_attempt: 2,
        published_at: exit_at,
        code: Some(23),
        signal: None,
    });
    assert_eq!(actor.producer_process_exit_due, successor_exit);
    assert!(matches!(
        actor.pending_decision.as_deref(),
        Some(ProducerDecision::Retry {
            decision_sequence: 1,
            failed_attempt: 1,
            ..
        })
    ));

    assert_eq!(
        actor.observe_producer_exit_at(
            exit_at + Duration::from_nanos(1),
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(99),
                signal: None,
                observed_at: exit_at + Duration::from_nanos(1),
            },
        ),
        ProducerExitAcceptance::Rejected
    );
    assert_eq!(
        actor.producer_process_exit_due, successor_exit,
        "the stale predecessor cannot replace the retained successor exit"
    );

    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_command(RollingControlEnvelope {
            sequence: 1,
            published_at: exit_at,
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::DecisionApplied {
                decision_sequence: 1,
                installed_attempt: Some(2),
                reply,
            },
        })
        .await;
    assert_eq!(response.await.expect("retry acknowledgement"), Ok(()));
    assert!(matches!(
        actor.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            decision_sequence: 2,
            failed_attempt: 2,
            reason: ProducerDecisionReason::ProcessExit,
            ..
        })
    ));
    assert!(
        !actor.pending_retry_precedes_successor(2),
        "a pending Fail never gains the retry-overlap exemption"
    );
    assert_eq!(
        actor.admit_producer_retry_at(exit_at, 2, "recipe-successor-exit"),
        Err(ProducerAttemptRejection::RetryUnavailable)
    );
}

#[test]
fn terminal_and_executor_loss_fence_prepublication_decisions() {
    let started = Instant::now();
    let mut terminal = registered_prepublication_actor(started);
    assert_eq!(
        terminal.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-terminal", "recipe-terminal"),
        ),
        Ok(1)
    );
    assert_eq!(
        terminal.terminate(RollingTerminalCause::End),
        RollingTerminalOutcome::Won(RollingTerminalCause::End)
    );
    assert!(terminal
        .settle_due_deadlines_at(started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET)
        .is_none());
    assert_eq!(
        terminal.poll_producer_decision_at(0),
        ProducerDecisionPoll::Terminal(RollingTerminalCause::End)
    );

    let mut lost = registered_prepublication_actor(started);
    assert_eq!(
        lost.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-lost", "recipe-lost"),
        ),
        Ok(1)
    );
    lost.mark_executor_lost(started + Duration::from_secs(1));
    assert!(matches!(
        lost.pending_decision.as_deref(),
        Some(ProducerDecision::Fail {
            decision_sequence: 1,
            failed_attempt: 1,
            reason: ProducerDecisionReason::ExecutorLost,
            ..
        })
    ));
    let status = lost.producer_operational_snapshot_at(started + Duration::from_secs(1));
    assert_eq!(status.executor_state, "lost");
    assert_eq!(
        lost.begin_initial_producer_attempt_at(
            started + Duration::from_secs(1),
            InitialProducerPolicy::software(
                "presentation-lost".to_owned(),
                PRODUCER_PROGRESS_BUDGET,
            ),
        ),
        Err(ProducerAttemptRejection::ExecutorLost)
    );

    let mut lost_with_retry = registered_prepublication_actor(started);
    assert_eq!(
        lost_with_retry.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-lost-retry", "recipe-lost-retry"),
        ),
        Ok(1)
    );
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    assert!(lost_with_retry.settle_due_deadlines_at(deadline).is_some());
    let retained_retry = lost_with_retry
        .pending_decision
        .clone()
        .expect("hardware deadline retains one retry decision");
    lost_with_retry.mark_executor_lost(deadline + Duration::from_nanos(1));
    assert_eq!(
        lost_with_retry.pending_decision.as_ref(),
        Some(&retained_retry),
        "executor loss cannot replace the immutable pending retry"
    );
    assert!(lost_with_retry.producer_progress_deadline.is_none());
    assert!(lost_with_retry.producer_deadline_due.is_none());
    assert!(lost_with_retry.producer_process_exit_due.is_none());
    assert!(matches!(
        lost_with_retry
            .prepublication
            .as_ref()
            .expect("pre-publication state")
            .retry_state,
        PrepublicationRetryState::Consumed
    ));
    assert_eq!(
        lost_with_retry.admit_producer_retry_at(
            deadline + Duration::from_nanos(1),
            retained_retry.decision_sequence(),
            "recipe-lost-retry",
        ),
        Err(ProducerAttemptRejection::ExecutorLost)
    );
    assert_eq!(
        lost_with_retry.authorize_response_publication_at(
            deadline + Duration::from_nanos(1),
            RollingResponsePublication::generation_metadata(
                RollingResponseObject::MasterPlaylist,
                "presentation-lost-retry".to_owned(),
            ),
        ),
        Err(ResponsePublicationRejection::DecisionCommitted)
    );
    assert!(!lost_with_retry.commit_generation_metadata_at(
        deadline + Duration::from_nanos(1),
        "presentation-lost-retry",
        "generation-metadata-eof",
    ));
    let lost_retry_status =
        lost_with_retry.producer_operational_snapshot_at(deadline + Duration::from_nanos(1));
    assert_eq!(lost_retry_status.phase, "failed");
    assert_eq!(lost_retry_status.action_owner, "terminal_failure");
    assert_eq!(lost_retry_status.executor_state, "lost");
}

#[test]
fn producer_decision_slot_is_immutable_redeliverable_and_bounded() {
    let now = Instant::now();
    let mut actor =
        RollingControlActor::new(now, "session-start", Arc::new(AtomicBool::new(false)));
    let decision = ProducerDecision::Retry {
        decision_sequence: 7,
        failed_attempt: 1,
        recipe: ValidatedRetryRecipe::for_test("cpu-safe", "fingerprint-a"),
        reason: ProducerDecisionReason::StartupDeadline,
    };
    assert!(actor.install_producer_decision_for_test(now, decision.clone()));
    assert!(!actor.install_producer_decision_for_test(
        now,
        ProducerDecision::Fail {
            decision_sequence: 8,
            failed_attempt: 1,
            reason: ProducerDecisionReason::ProcessExit,
            proposal: None,
            cleanup: ProducerFailureCleanup {
                kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                cleanup_policy: CleanupPolicy::DiscardPrepublication,
            },
        }
    ));
    assert_eq!(
        actor.poll_producer_decision_at(0),
        ProducerDecisionPoll::Decision(Arc::new(decision.clone()))
    );
    assert_eq!(
        actor.poll_producer_decision_at(6),
        ProducerDecisionPoll::Decision(Arc::new(decision))
    );
    assert_eq!(
        actor.poll_producer_decision_at(7),
        ProducerDecisionPoll::Idle
    );
}

async fn wait_for_executor_observation(
    handle: &RollingControlHandle,
    expected_state: &str,
    expected_sequence: Option<u64>,
) {
    for _ in 0..1_024 {
        let (state, sequence) = handle.executor_observation_for_test();
        if state == expected_state && expected_sequence.is_none_or(|expected| sequence == expected)
        {
            return;
        }
        tokio::task::yield_now().await;
    }
    let (state, sequence) = handle.executor_observation_for_test();
    panic!(
        "executor never reached {expected_state:?}/{expected_sequence:?}; last={state:?}/{sequence}"
    );
}

#[test]
fn terminal_transition_settles_the_retained_producer_decision() {
    let now = Instant::now();
    let retired = Arc::new(AtomicBool::new(false));
    let mut actor = RollingControlActor::new(now, "session-start", retired);
    assert!(actor.install_producer_decision_for_test(
        now,
        ProducerDecision::Fail {
            decision_sequence: 3,
            failed_attempt: 2,
            reason: ProducerDecisionReason::ProgressDeadline,
            proposal: None,
            cleanup: ProducerFailureCleanup {
                kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                cleanup_policy: CleanupPolicy::RetainPublished,
            },
        }
    ));
    assert_eq!(
        actor.terminate(RollingTerminalCause::End),
        RollingTerminalOutcome::Won(RollingTerminalCause::End)
    );
    assert_eq!(
        actor.poll_producer_decision_at(0),
        ProducerDecisionPoll::Terminal(RollingTerminalCause::End)
    );
    assert!(actor.pending_decision.is_none());
    assert!(actor.decision_committed_at.is_none());
}

/// `after_command_settled` comes after the executor wake
/// (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8; #573 review finding 1). The
/// actor is held at that point after a decision install, a command that
/// changes what the executor sees. The executor must already have been
/// woken and begun observing, and it must stay at its poll, because the
/// held actor cannot answer, until the actor goes on. With the point
/// above the wake, the held actor has not woken the executor, which
/// stays idle.
#[tokio::test]
async fn actor_wakes_the_executor_before_its_command_settles() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let settled = crate::seam_hooks::AsyncPause::new("rolling control command settled");
    handle.pause_after_command_settled_for_test(Arc::clone(&settled));
    let decision = ProducerDecision::Fail {
        decision_sequence: 9,
        failed_attempt: 1,
        reason: ProducerDecisionReason::ReaderFailed,
        proposal: None,
        cleanup: ProducerFailureCleanup {
            kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
            cleanup_policy: CleanupPolicy::DiscardPrepublication,
        },
    };
    let install = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.install_producer_decision_for_test(decision).await })
    };
    let held = settled.reached().await;
    assert!(install.await.expect("install task"));

    wait_for_executor_observation(&handle, "executing", None).await;
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        handle.executor_observation_for_test(),
        ("executing".to_owned(), 0),
        "the executor's poll waits on the held actor"
    );

    held.release();
    wait_for_executor_observation(&handle, "idle", Some(9)).await;
}

#[tokio::test]
async fn decision_poll_replays_without_a_fake_application_acknowledgement() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let decision = ProducerDecision::Fail {
        decision_sequence: 4,
        failed_attempt: 1,
        reason: ProducerDecisionReason::ReaderFailed,
        proposal: None,
        cleanup: ProducerFailureCleanup {
            kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
            cleanup_policy: CleanupPolicy::DiscardPrepublication,
        },
    };
    assert!(
        handle
            .install_producer_decision_for_test(decision.clone())
            .await
    );
    assert_eq!(
        handle.poll_producer_decision(0).await,
        Ok(ProducerDecisionPoll::Decision(Arc::new(decision.clone())))
    );
    assert_eq!(
        handle.poll_producer_decision(0).await,
        Ok(ProducerDecisionPoll::Decision(Arc::new(decision)))
    );
    assert_eq!(
        handle.poll_producer_decision(4).await,
        Ok(ProducerDecisionPoll::Idle)
    );
    wait_for_executor_observation(&handle, "idle", Some(4)).await;
}

#[tokio::test]
async fn retained_notify_and_full_inbox_wakes_are_bounded() {
    let notify = tokio::sync::Notify::new();
    notify.notify_one();
    notify.notified().await;

    let (sender, mut inbox) = RollingSessionExecutorInbox::new();
    assert!(sender.try_send(()).is_ok());
    assert!(matches!(
        sender.try_send(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(()))
    ));
    assert_eq!(
        inbox.receiver.recv().await,
        Some(()),
        "one coalesced wake remains available"
    );
}

#[test]
fn terminal_and_lost_executor_observations_are_first_winner_absorbing() {
    let terminal_first = RollingExecutorObservation::default();
    assert!(terminal_first.begin_observing());
    terminal_first.settle_terminal();
    terminal_first.settle_lost();
    assert_eq!(terminal_first.state(), "terminal");
    assert!(!terminal_first.begin_observing());
    assert!(!terminal_first.finish_observing());

    let lost_first = RollingExecutorObservation::default();
    assert!(lost_first.begin_observing());
    lost_first.settle_lost();
    lost_first.settle_terminal();
    assert_eq!(lost_first.state(), "lost");
    assert!(!lost_first.begin_observing());
    assert!(!lost_first.finish_observing());
}

#[tokio::test]
async fn cancelled_decision_poll_does_not_consume_the_actor_slot() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let decision = ProducerDecision::Retry {
        decision_sequence: 12,
        failed_attempt: 1,
        recipe: ValidatedRetryRecipe::for_test("cpu-safe", "fingerprint-cancel"),
        reason: ProducerDecisionReason::StartupDeadline,
    };
    assert!(
        handle
            .install_producer_decision_for_test(decision.clone())
            .await
    );
    let (reply, response) = tokio::sync::oneshot::channel();
    handle
        .enqueue_command(RollingControlCommand::PollProducerDecision {
            after_sequence: 0,
            reply,
        })
        .await
        .expect("poll enqueue");
    drop(response);
    tokio::task::yield_now().await;

    assert_eq!(
        handle.poll_producer_decision(0).await,
        Ok(ProducerDecisionPoll::Decision(Arc::new(decision)))
    );
}

#[tokio::test]
async fn decision_transport_is_registered_before_begin_attempt_and_terminal_wake_is_retained() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    assert_eq!(handle.begin_producer_attempt().await, Ok(1));
    assert!(
        handle
            .install_producer_decision_for_test(ProducerDecision::Fail {
                decision_sequence: 9,
                failed_attempt: 1,
                reason: ProducerDecisionReason::ExecutorLost,
                proposal: None,
                cleanup: ProducerFailureCleanup {
                    kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                    cleanup_policy: CleanupPolicy::DiscardPrepublication,
                },
            })
            .await
    );
    assert_eq!(
        handle.end().await,
        Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
    );
    assert_eq!(
        handle.poll_producer_decision(0).await,
        Ok(ProducerDecisionPoll::Terminal(RollingTerminalCause::End))
    );
    wait_for_executor_observation(&handle, "terminal", None).await;
    handle.abort_actor_for_test();
    handle.wait_for_actor_exit_fence_for_test().await;
    handle.sender.closed().await;
    assert_eq!(
        handle.poll_producer_decision(0).await,
        Ok(ProducerDecisionPoll::Terminal(RollingTerminalCause::End)),
        "mailbox loss cannot relabel the actor's committed terminal cause"
    );
}

#[tokio::test(start_paused = true)]
async fn timer_only_lease_expiry_wakes_the_passive_executor() {
    let handle = RollingControlHandle::spawn_for_test("session-start");

    handle.wait_for_actor_start_for_test().await;
    tokio::time::advance(ROLLING_LEGACY_LEASE_TIMEOUT + Duration::from_millis(1)).await;
    wait_for_executor_observation(&handle, "terminal", None).await;

    assert_eq!(
        handle.poll_producer_decision(0).await,
        Ok(ProducerDecisionPoll::Terminal(
            RollingTerminalCause::LeaseExpired
        ))
    );
}

#[tokio::test]
async fn unexpected_executor_exit_is_visible_while_the_actor_remains_live() {
    let handle = RollingControlHandle::spawn_for_test("session-start");

    handle.abort_executor_for_test();
    wait_for_executor_observation(&handle, "lost", None).await;

    let decision = ProducerDecision::Fail {
        decision_sequence: 15,
        failed_attempt: 1,
        reason: ProducerDecisionReason::ExecutorLost,
        proposal: None,
        cleanup: ProducerFailureCleanup {
            kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
            cleanup_policy: CleanupPolicy::DiscardPrepublication,
        },
    };
    assert!(
        handle
            .install_producer_decision_for_test(decision.clone())
            .await
    );
    assert_eq!(
        handle.poll_producer_decision(0).await,
        Ok(ProducerDecisionPoll::Decision(Arc::new(decision)))
    );
    assert_eq!(handle.executor_observation_for_test().0, "lost");
    assert_eq!(
        handle.end().await,
        Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
    );
    assert_eq!(
        handle.executor_observation_for_test().0,
        "lost",
        "committed terminal state must preserve earlier executor-loss evidence"
    );
}

#[tokio::test]
async fn unexpected_actor_exit_is_not_reported_as_a_committed_terminal_cause() {
    let handle = RollingControlHandle::spawn_for_test("session-start");

    handle.wait_for_actor_start_for_test().await;
    handle.abort_actor_for_test();
    handle.wait_for_actor_exit_fence_for_test().await;
    handle.sender.closed().await;
    wait_for_executor_observation(&handle, "lost", None).await;

    assert_eq!(handle.decision_transport.committed_terminal(), None);
    assert_eq!(handle.poll_producer_decision(0).await, Err(()));
}

#[tokio::test]
async fn stale_poll_result_cannot_overwrite_terminal_executor_state() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let pause = crate::seam_hooks::AsyncPause::new("executor observation");
    handle.pause_executor_observation_for_test(Arc::clone(&pause));
    assert!(
        handle
            .install_producer_decision_for_test(ProducerDecision::Fail {
                decision_sequence: 17,
                failed_attempt: 1,
                reason: ProducerDecisionReason::ReaderFailed,
                proposal: None,
                cleanup: ProducerFailureCleanup {
                    kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                    cleanup_policy: CleanupPolicy::DiscardPrepublication,
                },
            })
            .await
    );
    let held = pause.reached().await;

    assert_eq!(
        handle.end().await,
        Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
    );
    assert_eq!(handle.executor_observation_for_test().0, "terminal");
    held.release();
    wait_for_executor_observation(&handle, "terminal", Some(17)).await;
}

#[tokio::test]
async fn actor_loss_during_executor_poll_is_reported_as_lost_not_terminal() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let pause = crate::seam_hooks::AsyncPause::new("executor poll");
    handle.pause_executor_poll_for_test(Arc::clone(&pause));
    assert!(
        handle
            .install_producer_decision_for_test(ProducerDecision::Fail {
                decision_sequence: 16,
                failed_attempt: 1,
                reason: ProducerDecisionReason::ExecutorLost,
                proposal: None,
                cleanup: ProducerFailureCleanup {
                    kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                    cleanup_policy: CleanupPolicy::DiscardPrepublication,
                },
            })
            .await
    );
    let held = pause.reached().await;

    handle.abort_actor_for_test();
    handle.wait_for_actor_exit_fence_for_test().await;
    handle.sender.closed().await;
    held.release();
    wait_for_executor_observation(&handle, "lost", None).await;

    assert_eq!(handle.decision_transport.committed_terminal(), None);
}

#[tokio::test]
async fn dropping_the_last_handle_does_not_leave_the_actor_mailbox_self_owned() {
    let handle = RollingControlHandle::spawn_for_test("session-start");
    let retired = Arc::clone(&handle.retired);
    let transport = Arc::downgrade(&handle.decision_transport);

    drop(handle);

    assert!(
        transport.upgrade().is_none(),
        "the passive executor must not keep its command transport alive"
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while !retired.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actor must exit promptly after its final external sender is dropped");
}

/// The control group's race scenarios through the production constructor
/// (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8). Every no-op hook is ready at
/// once, so the actor answers an attempt and settles each command, the
/// passive executor passes both of its poll points to observe a committed
/// decision and then the terminal, and dropping the last handle runs the
/// exit fence.
#[tokio::test]
async fn rolling_control_group_shipped_shape() {
    let handle = RollingControlHandle::spawn("session-start");
    assert!(
        RollingControlTestHooks::of(handle.decision_transport.hooks.as_ref()).is_none(),
        "the production constructor installs the no-op hooks"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), handle.begin_producer_attempt())
            .await
            .expect("the actor answers through its production hooks"),
        Ok(1)
    );
    assert!(tokio::time::timeout(
        Duration::from_secs(5),
        handle.install_producer_decision_for_test(ProducerDecision::Fail {
            decision_sequence: 17,
            failed_attempt: 1,
            reason: ProducerDecisionReason::ReaderFailed,
            proposal: None,
            cleanup: ProducerFailureCleanup {
                kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                cleanup_policy: CleanupPolicy::DiscardPrepublication,
            },
        }),
    )
    .await
    .expect("the actor settles its first command and takes the next"));
    wait_for_executor_observation(&handle, "idle", Some(17)).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), handle.end())
            .await
            .expect("the actor answers the terminal command"),
        Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
    );
    wait_for_executor_observation(&handle, "terminal", Some(17)).await;

    let retired = Arc::clone(&handle.retired);
    drop(handle);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !retired.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the exit fence publishes retirement through the production hooks");
}

/// The producer ingress's two points through the production hooks,
/// polled by hand: a waiter that finds no flow-barrier capacity parks on
/// its registered wake and the release of one reservation wakes it, and a
/// flow barrier still enters the ingress.
#[tokio::test]
async fn rolling_producer_ingress_shipped_shape() {
    use std::future::Future;
    let ingress = RollingProducerIngress::with_hooks(Arc::new(NoopRollingControlHooks));
    for _ in 0..ROLLING_PRODUCER_FLOW_BARRIER_CAPACITY {
        assert!(ingress.reserve_flow_barrier());
    }
    let retired = AtomicBool::new(false);
    let (actor, _receiver) = tokio::sync::mpsc::channel(1);
    let observation = RollingProducerFlowObservation {
        producer_attempt: 1,
        state: ProducerPhysicalFlowState::Held,
    };
    {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut waiter = std::pin::pin!(ingress.wait_for_flow_barrier_capacity(&retired, &actor));
        assert!(
            waiter.as_mut().poll(&mut context).is_pending(),
            "a full ingress parks its waiter"
        );
        ingress.finish_reserved_flow_barrier(observation, false);
        assert_eq!(
            waiter.as_mut().poll(&mut context),
            std::task::Poll::Ready(true),
            "the production hook let the waiter reach its registered wake"
        );
    }
    ingress.finish_reserved_flow_barrier(observation, true);
    assert_eq!(
        ingress.drain(),
        vec![RollingProducerEvent::FlowApplied(observation)]
    );
}

// -----------------------------------------------------------------------
// M3b2 — the health barrier
// -----------------------------------------------------------------------

fn decode_fault(producer_attempt: u64, records: u64) -> ProducerDecodeFault {
    decode_fault_qualified(producer_attempt, records, true)
}

/// The same fault with §7.3's gate answered either way.
///
/// Separate from [`decode_fault`] rather than a parameter on it because
/// every pre-M5c test wants the qualified answer and reads better without
/// a bare `true` at each call site.
fn decode_fault_qualified(
    producer_attempt: u64,
    records: u64,
    action_qualified: bool,
) -> ProducerDecodeFault {
    ProducerDecodeFault {
        action_qualified,
        producer_attempt,
        plan_digest: "plan-digest".to_owned(),
        fault: crate::decoder_health::DecodeFaultKind::VideoDecodeFailure,
        input_video_stream: 0,
        primary_error_records: records,
        diagnostic_contract: Some("contract".to_owned()),
    }
}

fn health_receipt(
    qualification: crate::decoder_health::Qualification,
) -> crate::decoder_health::ProducerHealthReceipt {
    crate::decoder_health::ProducerHealthReceipt {
        receipt_version: crate::decoder_health::PRODUCER_HEALTH_RECEIPT_VERSION,
        plan_digest: "plan-digest".to_owned(),
        diagnostic_contract: Some("contract".to_owned()),
        // Deliberately different from the fault's record count, so a
        // snapshot field sourced from the wrong one is visible.
        observation_complete: true,
        video_decode_error_records: 4,
        contract_qualified_error_records: 3,
        terminal_fault: None,
        exit_disposition: crate::decoder_health::ExitDisposition::CleanEnd,
        qualification,
    }
}

// -----------------------------------------------------------------------
// M5c — a qualified decode fault replaces the timing verdict
// -----------------------------------------------------------------------

/// The loop this whole effort exists to end.
///
/// A source the decoder cannot decode produces no segments, so a timing or
/// process verdict always arrives, and every one of those tells the client
/// to try again. It does, gets the identical failure, and tries again. The
/// decoder's own diagnostics are the only evidence that separates "this
/// attempt was unlucky" from "this source will never decode", and they
/// reach the decision as a latched fault.
#[test]
fn a_qualified_decode_fault_replaces_the_process_verdict_a_client_would_retry() {
    let started = Instant::now();
    let exited_at = started + Duration::from_secs(2);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-m5c-qualified", "recipe-m5c-qualified"),
        ),
        Ok(1)
    );
    assert!(actor.observe_producer_decode_fault(decode_fault_qualified(1, 5, true)));
    assert_eq!(
        actor.observe_producer_exit_at(
            exited_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(9),
                signal: None,
                observed_at: exited_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert!(actor.maybe_commit_producer_decision_at(exited_at));
    assert!(
        matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::SourceDecodeFailed,
                ..
            })
        ),
        "a qualified fault must rewrite the process verdict, not merely be logged beside it"
    );
}

/// The gate is the whole difference between a verdict and a guess.
///
/// An unqualified window is one this build cannot vouch for — an
/// unqualified grammar, a compressed log, a window that never met the
/// threshold. Acting on it would condemn a source on evidence the build
/// itself does not trust, and a permanent verdict is the one mistake a
/// client cannot recover from by trying again.
#[test]
fn an_unqualified_decode_fault_leaves_the_process_verdict_alone() {
    let started = Instant::now();
    let exited_at = started + Duration::from_secs(2);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-m5c-unqualified", "recipe-m5c-unqualified"),
        ),
        Ok(1)
    );
    assert!(actor.observe_producer_decode_fault(decode_fault_qualified(1, 5, false)));
    assert_eq!(
        actor.observe_producer_exit_at(
            exited_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(9),
                signal: None,
                observed_at: exited_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert!(actor.maybe_commit_producer_decision_at(exited_at));
    assert!(
        matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::ProcessExit,
                ..
            })
        ),
        "an unqualified window must not condemn a source"
    );
}

/// The `!is_permanent` clause, and it is not a nicety.
///
/// `Unsupported` is the copy pipeline saying it cannot carry this source —
/// and `UnsupportedOnly` eligibility exists precisely so that verdict
/// selects the one validated fallback. Overwriting it with
/// `SourceDecodeFailed`, which that eligibility does not allow, would
/// silently cancel a fallback the server was about to take and fail a
/// source that the successor recipe would have played.
#[test]
fn a_qualified_decode_fault_does_not_overwrite_a_verdict_that_already_knows_more() {
    let started = Instant::now();
    let classified_at = started + Duration::from_secs(1);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-m5c-permanent", "recipe-m5c-permanent"),
        ),
        Ok(1)
    );
    assert!(actor.observe_producer_decode_fault(decode_fault_qualified(1, 5, true)));
    assert_eq!(
        actor.classify_copy_producer_exit_at(
            classified_at,
            1,
            CopyProducerExitClassification::Unsupported,
        ),
        Ok(())
    );
    assert!(
        matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Retry {
                reason: ProducerDecisionReason::Unsupported,
                ..
            })
        ),
        "a permanent verdict that still selects a fallback must survive the fault"
    );
}

/// A ledger handle for the tests below, at the one identity they share.
fn recovery_ledger(
    store: &std::sync::Arc<dyn plurx_core::store::Store>,
    playback: &str,
    epoch: &str,
    incarnation: &str,
) -> ProducerRecoveryLedger {
    ProducerRecoveryLedger::new(
        std::sync::Arc::clone(store),
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        playback,
        epoch,
        incarnation,
    )
    .expect("a complete identity")
}

/// The budget is one per playback, and it is never given back.
///
/// Everything this milestone is for lives in the second half of this test.
/// The in-process one-shot M5c2 uses makes a session take one recovery; a
/// reopen, a seek or a track change mints a fresh session and therefore a
/// fresh one-shot, which is one automatic recovery per *attempt* against a
/// file that cannot be decoded. The durable row is what makes the second
/// attempt find the budget already gone.
///
/// The two refusals in here are different facts and the test asserts them
/// separately, because a `bool` cannot tell them apart and reading one as
/// the other is a live bug in either direction: a held reservation read as
/// spent abandons a `reserved` row that nothing will ever settle, and a
/// spent one read as held hands out a second recovery.
#[tokio::test]
async fn the_recovery_budget_is_taken_once_per_playback_and_never_returned() {
    use plurx_core::domain::ProducerRecoveryState;
    use plurx_core::store::SqliteStore;

    let store: std::sync::Arc<dyn plurx_core::store::Store> =
        std::sync::Arc::new(SqliteStore::open_in_memory().expect("store"));
    let failed = "a".repeat(64);
    let alternate = "b".repeat(64);

    let first = recovery_ledger(&store, "playback-1", "epoch-1", "incarnation-1");
    assert!(
        matches!(
            first
                .reserve(1, 1, &failed, &alternate, 1_000)
                .await
                .expect("reserve"),
            RecoveryReservation::Held(_)
        ),
        "the first fault of a playback takes its one budget"
    );

    // The same session asking again for the same thing is a replay, not a
    // second budget: the store compares every field and answers with the
    // row it already wrote. The row comes back because a replaying
    // executor has to be able to read what it had already decided.
    let RecoveryReservation::Held(replayed) = first
        .reserve(1, 1, &failed, &alternate, 1_100)
        .await
        .expect("replay")
    else {
        panic!("an exact replay is the same reservation");
    };
    assert_eq!(replayed.state, ProducerRecoveryState::Reserved);
    assert_eq!(replayed.failed_incarnation_id, "incarnation-1");
    assert_eq!(replayed.alternate_plan_digest, alternate);

    // A later session of the same playback — a reopen, a seek, a track
    // change — is a new incarnation and a new attempt. It inherits the
    // epoch, and the epoch is what the budget is keyed by, so it finds a
    // row. The row is still `Reserved`, so what it has found is the
    // predecessor *holding* the budget, not a budget spent: the store
    // fences on the reserving incarnation, and this is that fence.
    let successor = recovery_ledger(&store, "playback-1", "epoch-1", "incarnation-2");
    assert!(
        matches!(
            successor
                .reserve(1, 1, &failed, &alternate, 1_200)
                .await
                .expect("second reserve"),
            RecoveryReservation::Mismatched(_)
        ),
        "a continuation does not get a second recovery, and the one it \
             cannot have is held rather than gone"
    );

    // Settling does not return it, whichever way the attempt went. Now,
    // and only now, is the budget spent.
    assert_eq!(
        first
            .settle(RecoveryOutcome::Exhausted, 1_300)
            .await
            .expect("settle"),
        RecoverySettlement::Settled,
        "the reserving identity settles its own row"
    );
    assert!(
        matches!(
            successor
                .reserve(1, 1, &failed, &alternate, 1_400)
                .await
                .expect("after settle"),
            RecoveryReservation::Spent(_)
        ),
        "a settled budget is spent, not released"
    );
    assert!(
        matches!(
            first
                .reserve(1, 1, &failed, &alternate, 1_450)
                .await
                .expect("reserver after settle"),
            RecoveryReservation::Spent(_)
        ),
        "not even the identity that settled it gets it back"
    );

    // A different playback is a different budget, which is the other half
    // of "one per playback".
    let other = recovery_ledger(&store, "playback-2", "epoch-2", "incarnation-3");
    assert!(
        matches!(
            other
                .reserve(1, 1, &failed, &alternate, 1_500)
                .await
                .expect("other playback"),
            RecoveryReservation::Held(_)
        ),
        "one playback spending its budget does not spend another's"
    );
}

/// A live reservation the request no longer matches is a mismatch, and the
/// session that made it is the likeliest one to hit it.
///
/// Every field of the request is part of the row's identity, so an
/// executor that reserved against one alternate and then re-planned onto
/// another asks for something the stored row does not describe. It has not
/// spent anything; it is holding a reservation it has to settle before it
/// can take a different one. `decode_restriction` is in this list too and
/// cannot be exercised from here — nothing in this daemon builds a
/// `ContinuationDecodeRestriction` yet — which is exactly why the
/// reservation doc says the first build that writes one has to settle and
/// re-epoch rather than replay.
#[tokio::test]
async fn a_changed_request_against_its_own_live_reservation_is_a_mismatch() {
    use plurx_core::store::SqliteStore;

    let store: std::sync::Arc<dyn plurx_core::store::Store> =
        std::sync::Arc::new(SqliteStore::open_in_memory().expect("store"));
    let failed = "a".repeat(64);
    let alternate = "b".repeat(64);
    let other_digest = "c".repeat(64);

    let ledger = recovery_ledger(&store, "playback-1", "epoch-1", "incarnation-1");
    assert!(
        matches!(
            ledger
                .reserve(1, 1, &failed, &alternate, 1_000)
                .await
                .expect("reserve"),
            RecoveryReservation::Held(_)
        ),
        "the reservation under test"
    );

    for (attempt, sequence, failed_digest, alternate_digest, why) in [
        (2, 1, &failed, &alternate, "a later producer attempt"),
        (1, 2, &failed, &alternate, "a later decision"),
        (1, 1, &other_digest, &alternate, "a different failed plan"),
        (1, 1, &failed, &other_digest, "a different alternate"),
    ] {
        assert!(
            matches!(
                ledger
                    .reserve(attempt, sequence, failed_digest, alternate_digest, 1_100)
                    .await
                    .expect("changed request"),
                RecoveryReservation::Mismatched(_)
            ),
            "{why} does not replay onto a row that describes a different decision"
        );
    }
}

/// Settlement names the fence it hit, because a fence reported as "there
/// was nothing there" is a split brain nobody looks for.
#[tokio::test]
async fn a_settlement_says_which_of_the_three_refusals_it_found() {
    use plurx_core::store::SqliteStore;

    let store: std::sync::Arc<dyn plurx_core::store::Store> =
        std::sync::Arc::new(SqliteStore::open_in_memory().expect("store"));
    let failed = "a".repeat(64);
    let alternate = "b".repeat(64);

    // Nothing was ever reserved for this epoch. A caller settling on a
    // path out that never reserved is the ordinary case, not an error.
    let never = recovery_ledger(&store, "playback-absent", "epoch-absent", "incarnation-1");
    assert_eq!(
        never
            .settle(RecoveryOutcome::Exhausted, 1_000)
            .await
            .expect("absent"),
        RecoverySettlement::Absent
    );

    let owner = recovery_ledger(&store, "playback-1", "epoch-1", "incarnation-1");
    assert!(
        matches!(
            owner
                .reserve(1, 1, &failed, &alternate, 1_100)
                .await
                .expect("reserve"),
            RecoveryReservation::Held(_)
        ),
        "the reservation under test"
    );

    // A stale holder of the three key fields — a node that handled this
    // playback before a handoff — cannot exhaust a live reservation.
    let stale = recovery_ledger(&store, "playback-1", "epoch-1", "incarnation-2");
    assert_eq!(
        stale
            .settle(RecoveryOutcome::Exhausted, 1_200)
            .await
            .expect("stale"),
        RecoverySettlement::NotOurs
    );
    assert!(
        matches!(
            owner
                .reserve(1, 1, &failed, &alternate, 1_250)
                .await
                .expect("still held"),
            RecoveryReservation::Held(_)
        ),
        "the fence left the owner's reservation alone"
    );

    assert_eq!(
        owner
            .settle(RecoveryOutcome::Installed, 1_300)
            .await
            .expect("owner settles"),
        RecoverySettlement::Settled
    );
    assert_eq!(
        owner
            .settle(RecoveryOutcome::Installed, 1_400)
            .await
            .expect("owner settles again"),
        RecoverySettlement::Settled,
        "an owner that crashed after settling and settles again is told \
             what happened, not handed a silence it cannot read"
    );
    assert_eq!(
        owner
            .settle(RecoveryOutcome::Exhausted, 1_500)
            .await
            .expect("owner contradicts itself"),
        RecoverySettlement::Conflicting,
        "two different answers about one attempt is a caller bug, and it \
             is not the fencing failure `NotOurs` would send someone hunting"
    );
}

/// An incomplete identity is refused at construction, not at reservation.
///
/// The store refuses a non-positive user id, an empty playback id and an
/// empty epoch separately, and it refuses them at the moment a recovery is
/// needed — the worst moment to learn a session never had a budget. Three
/// real start paths carry exactly those gaps: a legacy process-local
/// start, a relayed worker start, and any session predating the epoch
/// column.
#[test]
fn an_incomplete_identity_has_no_ledger_handle_at_all() {
    use plurx_core::store::SqliteStore;
    let store: std::sync::Arc<dyn plurx_core::store::Store> =
        std::sync::Arc::new(SqliteStore::open_in_memory().expect("store"));
    for (user_id, playback, epoch, incarnation, why) in [
        (
            0,
            "playback",
            "epoch",
            "incarnation",
            "a legacy start has no user id",
        ),
        (7, "", "epoch", "incarnation", "no playback is no key"),
        (
            7,
            "playback",
            "",
            "incarnation",
            "a relayed start carries no epoch",
        ),
        (
            7,
            "playback",
            "epoch",
            "",
            "a reservation names the attempt that failed",
        ),
    ] {
        assert!(
            ProducerRecoveryLedger::new(
                std::sync::Arc::clone(&store),
                plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id },
                playback,
                epoch,
                incarnation,
            )
            .is_none(),
            "{why}"
        );
    }
}

/// The recovery this whole effort exists for, at the moment it fires.
///
/// A qualified decode fault reaches a decision that a timing deadline
/// triggered. Without an alternate the honest answer is permanent, and
/// M5c1 made it so. With one, the honest answer is a retry — but a retry
/// on a *different* recipe, because retrying the identical decode is the
/// loop this effort is named after, and with an impermanent reason,
/// because a permanent one on a retry reaches the client as a terminal
/// answer while this very successor is coming up.
#[test]
fn a_qualified_fault_installs_the_software_decode_alternate_rather_than_the_colour_safe_one() {
    let started = Instant::now();
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    let mut actor = registered_prepublication_actor(started);
    let alternate = ValidatedRetryRecipe::new(
        "software-decode-alternate".to_owned(),
        "alternate-fingerprint".to_owned(),
        "presentation-m5c2d".to_owned(),
        ProducerStartupKind::MixedSoftwareDecode,
    );
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-m5c2d", "recipe-m5c2d")
                .with_decode_alternate(Some(alternate.clone())),
        ),
        Ok(1)
    );
    assert!(actor.observe_producer_decode_fault(decode_fault_qualified(1, 5, true)));
    assert!(actor.settle_due_deadlines_at(deadline).is_some());

    let Some(ProducerDecision::Retry { recipe, reason, .. }) = actor.pending_decision.as_deref()
    else {
        panic!("a qualified fault with an alternate must install it, not fail");
    };
    assert_eq!(
        recipe, &alternate,
        "the colour-safe recipe answers a stopped encode; this fault is about the decode"
    );
    assert_eq!(*reason, ProducerDecisionReason::SourceDecodeRetry);
    assert!(
        !reason.is_permanent(),
        "a permanent reason here ends the session the successor was going to rescue"
    );
}

/// The alternate is offered to a decode fault and to nothing else.
///
/// An ordinary stall is not evidence about the source, and installing a
/// software decode for one would spend the session's one retry on a
/// different recipe than the failure called for — and quietly move the
/// artifact, since the decode backend feeds the plan digest.
#[test]
fn a_timing_verdict_without_a_qualified_fault_still_takes_the_colour_safe_recipe() {
    let started = Instant::now();
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    let alternate = ValidatedRetryRecipe::new(
        "software-decode-alternate".to_owned(),
        "alternate-fingerprint".to_owned(),
        "presentation-m5c2d-plain".to_owned(),
        ProducerStartupKind::MixedSoftwareDecode,
    );

    // No fault at all.
    let mut plain = registered_prepublication_actor(started);
    assert_eq!(
        plain.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-m5c2d-plain", "recipe-m5c2d-plain")
                .with_decode_alternate(Some(alternate.clone())),
        ),
        Ok(1)
    );
    assert!(plain.settle_due_deadlines_at(deadline).is_some());
    let Some(ProducerDecision::Retry { recipe, reason, .. }) = plain.pending_decision.as_deref()
    else {
        panic!("a startup deadline still retries");
    };
    assert_ne!(recipe, &alternate);
    assert_eq!(*reason, ProducerDecisionReason::StartupDeadline);

    // A fault this build cannot vouch for is not evidence either.
    let mut unqualified = registered_prepublication_actor(started);
    assert_eq!(
        unqualified.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-m5c2d-plain", "recipe-m5c2d-plain")
                .with_decode_alternate(Some(alternate.clone())),
        ),
        Ok(1)
    );
    assert!(unqualified.observe_producer_decode_fault(decode_fault_qualified(1, 5, false)));
    assert!(unqualified.settle_due_deadlines_at(deadline).is_some());
    let Some(ProducerDecision::Retry { recipe, reason, .. }) =
        unqualified.pending_decision.as_deref()
    else {
        panic!("a startup deadline still retries");
    };
    assert_ne!(
        recipe, &alternate,
        "an unqualified window cannot spend the retry on a different recipe"
    );
    assert_eq!(*reason, ProducerDecisionReason::StartupDeadline);
}

/// An alternate written for a different presentation is refused before the
/// session starts, not quietly at the decision.
///
/// This is the fence `validate` already applied to the colour-safe recipe,
/// now applied to both — because a session can install either, and an
/// invariant that held for one of them is not an invariant. Failing closed
/// at admission is the right end to fail at: a policy carrying a recipe
/// from another presentation contract is a caller bug, and discovering it
/// at decision time means discovering it during a recovery.
///
/// The decision keeps its own presentation filter as a second fence, for a
/// policy assembled by hand rather than through a constructor. Nothing in
/// production can reach it, which is the point.
#[test]
fn an_alternate_from_another_presentation_contract_is_refused() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-m5c2d-fence", "recipe-m5c2d-fence")
                .with_decode_alternate(Some(ValidatedRetryRecipe::new(
                    "software-decode-alternate".to_owned(),
                    "alternate-fingerprint".to_owned(),
                    "a-different-presentation".to_owned(),
                    ProducerStartupKind::MixedSoftwareDecode,
                ))),
        ),
        Err(ProducerAttemptRejection::InvalidPolicy),
        "an alternate is held to the presentation fence the colour-safe \
             recipe has always been held to"
    );

    // And an empty identity or fingerprint is refused by the same loop,
    // which is the half a decision-time filter could never have caught.
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-m5c2d-fence", "recipe-m5c2d-fence")
                .with_decode_alternate(Some(ValidatedRetryRecipe::new(
                    String::new(),
                    "alternate-fingerprint".to_owned(),
                    "presentation-m5c2d-fence".to_owned(),
                    ProducerStartupKind::MixedSoftwareDecode,
                ))),
        ),
        Err(ProducerAttemptRejection::InvalidPolicy),
    );
}

/// A retry keeps its own reason, because the client is watching.
///
/// The reason on a `Retry` is not private to the server. It reaches
/// `DeliveryView::producer_decision`, and `resolve_action` turns any
/// permanent reason into `ControlAction::Terminal` — so a `Retry` carrying
/// `SourceDecodeFailed` tells the viewer the file is dead while the server
/// is still bringing up its software fallback, which on a source only the
/// hardware decoder refused would very likely have played. `last_decision`
/// is never cleared, so that is not a race window; it is the rest of the
/// session.
///
/// The first draft of this milestone rewrote the reason before the retry
/// was chosen and shipped a test named for this property that asserted the
/// opposite of it. This is the assertion that draft needed.
#[test]
fn a_retry_is_never_handed_a_permanent_reason_while_the_fallback_is_still_coming() {
    let started = Instant::now();
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            hardware_policy("presentation-m5c-fallback", "recipe-m5c-fallback"),
        ),
        Ok(1)
    );
    assert!(actor.observe_producer_decode_fault(decode_fault_qualified(1, 5, true)));
    let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
    assert!(actor.settle_due_deadlines_at(deadline).is_some());
    let Some(decision) = actor.pending_decision.as_deref() else {
        panic!("the startup deadline must commit a decision");
    };
    assert!(
        matches!(decision, ProducerDecision::Retry { .. }),
        "a latched fault must not consume a fallback on another pipeline"
    );
    assert_eq!(
        decision.reason(),
        ProducerDecisionReason::StartupDeadline,
        "the retry keeps the verdict that actually ended the attempt"
    );
    assert!(
        !decision.reason().is_permanent(),
        "a permanent reason on a retry reaches the client as Terminal"
    );
}

/// A producer that published is a producer that decoded.
///
/// Five contract-qualified decode errors inside two seconds is an ordinary
/// property of a slightly damaged source, not proof that nothing decoded.
/// A long title with one corrupt region latches a fault, skips the bad
/// GOPs, publishes most of itself and exits — and calling that permanent
/// tears down a player that is still holding good buffer for a title that
/// mostly plays.
#[test]
fn a_fault_on_a_producer_that_published_is_recorded_and_not_acted_on() {
    let started = Instant::now();
    let published_at = started + Duration::from_secs(1);
    let exited_at = started + Duration::from_secs(2);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-m5c-published", "recipe-m5c-published"),
        ),
        Ok(1)
    );
    assert!(actor
        .authorize_response_publication_at(
            published_at,
            RollingResponsePublication::attempt_media(
                RollingResponseObject::MediaSegment,
                1,
                Some(1),
            ),
        )
        .is_ok());
    assert!(actor.observe_producer_decode_fault(decode_fault_qualified(1, 5, true)));
    assert_eq!(
        actor.observe_producer_exit_at(
            exited_at,
            RollingProducerExitObservation {
                producer_attempt: 1,
                success: false,
                code: Some(9),
                signal: None,
                observed_at: exited_at,
            },
        ),
        ProducerExitAcceptance::Accepted
    );
    assert!(actor.maybe_commit_producer_decision_at(exited_at));
    let Some(decision) = actor.pending_decision.as_deref() else {
        panic!("a non-zero exit must commit a decision");
    };
    assert_eq!(
        decision.reason(),
        ProducerDecisionReason::ProcessExit,
        "published media is proof the source decoded; the fault stays a record"
    );
}

/// The server's own outages are not verdicts about the file.
///
/// `ExecutorLost` says this node lost the process that was doing the work.
/// Relabelling it `SourceDecodeFailed` because a fault happened to be
/// latched makes a permanent claim about a source over an outage that will
/// pass, and the client cannot recover from a permanent claim by trying
/// again. The same argument covers the flow and install deadlines, which
/// is why the gate is a named list rather than `!is_permanent()`.
#[test]
fn a_server_side_loss_is_never_relabelled_as_a_verdict_about_the_source() {
    let started = Instant::now();
    let lost_at = started + Duration::from_secs(2);
    let mut actor = registered_prepublication_actor(started);
    assert_eq!(
        actor.begin_initial_producer_attempt_at(
            started,
            copy_policy("presentation-m5c-executor", "recipe-m5c-executor"),
        ),
        Ok(1)
    );
    assert!(actor.observe_producer_decode_fault(decode_fault_qualified(1, 5, true)));
    actor.mark_executor_lost(lost_at);
    let Some(decision) = actor.pending_decision.as_deref() else {
        panic!("executor loss must commit a decision");
    };
    assert_eq!(
        decision.reason(),
        ProducerDecisionReason::ExecutorLost,
        "a lost executor is a fact about this server, not about the file"
    );
}

/// The property this milestone exists for: a fault observed mid-attempt is
/// still the answer after the burst of healthy progress and the successful
/// exit that follow it.
///
/// Progress compaction is what makes this non-trivial. A batch collapses
/// to exactly one observation at drain, so anything riding on an
/// intermediate sample is dropped silently — which is why the fault has
/// its own sticky slot rather than a field on a progress observation.
#[test]
fn a_decode_fault_survives_the_healthy_progress_and_clean_exit_that_follow_it() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("initial producer attempt");
    let ingress = RollingProducerIngress::new();

    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            attempt,
            1_000,
            1_000,
            1_000,
            started + Duration::from_millis(100),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_millis(100),
    );
    ingress.publish_at(
        RollingProducerEvent::DecodeFault(decode_fault(attempt, 5)),
        ProducerIngressSlot::Health,
        started + Duration::from_millis(200),
    );
    // Everything a producer does after it stops decoding: more output,
    // faster, and then a clean exit. None of it is evidence that the
    // picture was ever decoded.
    for step in 1..=20_u64 {
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                attempt,
                1_000 + step as i64 * 1_000,
                2_000,
                2_000,
                started + Duration::from_millis(200 + step * 10),
            )),
            ProducerIngressSlot::Progress,
            started + Duration::from_millis(200 + step * 10),
        );
    }
    ingress.publish_at(
        RollingProducerEvent::Exit(producer_exit(
            attempt,
            true,
            Some(0),
            None,
            started + Duration::from_secs(1),
        )),
        ProducerIngressSlot::Exit,
        started + Duration::from_secs(1),
    );

    // Twenty-two progress publications, one open batch: the coalescing is
    // real, and it is what a fault riding on a progress observation would
    // have been collapsed into.
    let blocks = ingress.drain_blocks();
    let progress_blocks = blocks
        .iter()
        .filter(|block| matches!(block, RollingProducerIngressBlock::Progress(_)))
        .count();
    assert!(
        progress_blocks <= 1,
        "the progress publications coalesce: {blocks:?}"
    );
    let order: Vec<_> = blocks
        .iter()
        .filter_map(|block| match block {
            RollingProducerIngressBlock::Barrier(barrier) => match barrier.event.event {
                RollingProducerEvent::DecodeFault(_) => Some("fault"),
                RollingProducerEvent::Exit(_) => Some("exit"),
                _ => None,
            },
            RollingProducerIngressBlock::Progress(_) => None,
        })
        .collect();
    assert_eq!(
        order,
        vec!["fault", "exit"],
        "the fault is applied before the exit that would otherwise be the \
             last word about this attempt"
    );

    actor.handle_producer_blocks_at(started + Duration::from_secs(2), blocks);
    let latched = actor
        .latched_decode_fault()
        .expect("a fault observed mid-attempt is still the answer at the end of it");
    assert_eq!(latched.producer_attempt, attempt);
    assert_eq!(latched.primary_error_records, 5);
    assert_eq!(
        latched.fault,
        crate::decoder_health::DecodeFaultKind::VideoDecodeFailure
    );
    // And the exit really did reach the actor after it, so this is not a
    // fault that survived by never meeting one.
    assert!(actor.producer_exit_at.is_some(), "the exit was applied too");
}

/// A barrier absorbs the progress batch open when it arrives, which is
/// what makes it a barrier: everything published before it is applied
/// before it, and it cannot be reordered behind later progress.
#[test]
fn a_health_barrier_takes_the_open_progress_batch_with_it() {
    let started = Instant::now();
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(1, 1_000, 1_000, 1_000, started)),
        ProducerIngressSlot::Progress,
        started,
    );
    ingress.publish_at(
        RollingProducerEvent::DecodeFault(decode_fault(1, 5)),
        ProducerIngressSlot::Health,
        started + Duration::from_millis(10),
    );
    ingress.publish_at(
        RollingProducerEvent::Progress(producer_progress(
            1,
            2_000,
            1_000,
            1_000,
            started + Duration::from_millis(20),
        )),
        ProducerIngressSlot::Progress,
        started + Duration::from_millis(20),
    );

    let blocks = ingress.drain_blocks();
    assert_eq!(blocks.len(), 2, "the barrier and the progress after it");
    let RollingProducerIngressBlock::Barrier(barrier) = &blocks[0] else {
        panic!("the barrier is applied first: {blocks:?}");
    };
    assert!(
        matches!(
            barrier.event.event,
            RollingProducerEvent::DecodeFault(ref fault) if fault.primary_error_records == 5
        ),
        "{blocks:?}"
    );
    assert!(
        barrier.preceding_progress.is_some(),
        "the batch open when the fault landed travels with it, not after it"
    );
    assert!(matches!(
        blocks[1],
        RollingProducerIngressBlock::Progress(_)
    ));

    // Carried is not applied. Drive the blocks through an actor and assert
    // the absorbed batch reached the delivery snapshot: gutting the
    // `preceding_progress` arm of `handle_producer_block_at` would leave
    // the ingress assertions above green and silently drop that progress.
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("initial producer attempt");
    assert_eq!(attempt, 1);
    assert_eq!(actor.delivery.producer_out_time_ms, None);
    actor.handle_producer_blocks_at(started + Duration::from_millis(30), blocks);
    assert_eq!(
        actor.delivery.producer_out_time_ms,
        Some(2_000),
        "the batch the barrier absorbed is applied, and so is the one after it"
    );
}

/// A fault, an exit and a receipt are three facts about one attempt, and
/// each has its own slot. Sharing one would lose whichever arrived first.
#[test]
fn a_health_barrier_and_an_exit_barrier_do_not_displace_each_other() {
    let started = Instant::now();
    let ingress = RollingProducerIngress::new();
    ingress.publish_at(
        RollingProducerEvent::DecodeFault(decode_fault(1, 5)),
        ProducerIngressSlot::Health,
        started,
    );
    ingress.publish_at(
        RollingProducerEvent::Exit(producer_exit(
            1,
            true,
            Some(0),
            None,
            started + Duration::from_millis(10),
        )),
        ProducerIngressSlot::Exit,
        started + Duration::from_millis(10),
    );
    ingress.publish_at(
        RollingProducerEvent::DiagnosticsComplete(ProducerDiagnosticsComplete {
            producer_attempt: 1,
            receipt: health_receipt(crate::decoder_health::Qualification::Rejected),
        }),
        ProducerIngressSlot::Diagnostics,
        started + Duration::from_millis(20),
    );

    let blocks = ingress.drain_blocks();
    assert_eq!(blocks.len(), 3, "three facts, three slots: {blocks:?}");
    let kinds: Vec<_> = blocks
        .iter()
        .map(|block| match block {
            RollingProducerIngressBlock::Barrier(barrier) => match barrier.event.event {
                RollingProducerEvent::DecodeFault(_) => "fault",
                RollingProducerEvent::Exit(_) => "exit",
                RollingProducerEvent::DiagnosticsComplete(_) => "diagnostics",
                _ => "other",
            },
            RollingProducerIngressBlock::Progress(_) => "progress",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["fault", "exit", "diagnostics"],
        "and they are applied in the order they were published"
    );
}

/// One barrier per attempt. A second observation cannot relabel a decision
/// already made from the first, and a predecessor's fault cannot follow
/// the attempt that replaced it.
#[test]
fn a_fault_latches_once_per_attempt_and_never_crosses_into_the_next() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let first = actor
        .begin_producer_attempt_at(started)
        .expect("initial producer attempt");
    assert!(actor.observe_producer_decode_fault(decode_fault(first, 5)));
    assert!(
        !actor.observe_producer_decode_fault(decode_fault(first, 9)),
        "a second fault for one attempt is not a second barrier"
    );
    assert_eq!(
        actor
            .latched_decode_fault()
            .expect("latched")
            .primary_error_records,
        5,
        "and the first observation is the one that stands"
    );

    let second = actor
        .begin_producer_attempt_at(started + Duration::from_secs(1))
        .expect("prepublication replacement");
    assert_ne!(first, second);
    assert!(
        actor.latched_decode_fault().is_none(),
        "a successor gets fresh counters; the predecessor's fault is not \
             evidence about work it never touched"
    );
    assert!(
        !actor.observe_producer_decode_fault(decode_fault(first, 5)),
        "and a late fault for the predecessor cannot condemn the successor"
    );
    assert!(actor.latched_decode_fault().is_none());

    // The direction that matters most, and the one a bare `is_some()`
    // re-latch guard gets wrong: the successor's *own* first fault. A
    // stale predecessor fault refusing it would leave the recovery attempt
    // — the one this whole feature exists to inform — running blind.
    assert!(
        actor.observe_producer_decode_fault(decode_fault(second, 9)),
        "the successor's first fault is its first fault"
    );
    assert_eq!(
        actor
            .latched_decode_fault()
            .expect("the successor's own fault")
            .primary_error_records,
        9
    );
}

/// The settled receipt is a separate fact from the fault and from the
/// exit, and it reaches the operational snapshot so an operator can see
/// that a producer which exited cleanly was failing to decode.
#[test]
fn a_settled_receipt_reaches_the_operational_snapshot() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("initial producer attempt");
    let snapshot = actor.producer_operational_snapshot_at(started);
    assert_eq!(snapshot.decode_fault, None);
    assert_eq!(snapshot.diagnostics_qualification, None);

    assert!(actor.observe_producer_decode_fault(decode_fault(attempt, 5)));
    assert!(
        actor.observe_producer_diagnostics_complete(ProducerDiagnosticsComplete {
            producer_attempt: attempt,
            receipt: health_receipt(crate::decoder_health::Qualification::Rejected),
        })
    );
    let snapshot = actor.producer_operational_snapshot_at(started);
    assert_eq!(snapshot.decode_fault, Some("video_decode_failure"));
    assert_eq!(
        snapshot.decode_error_records,
        Some(5),
        "the fault's own count, not the receipt's"
    );
    assert_eq!(snapshot.diagnostics_qualification, Some("rejected"));

    // A later receipt for the same attempt corrects the earlier one: a
    // receipt is a summary of a finished attempt, and the only way a
    // second arrives is a caller correcting the first.
    assert!(
        actor.observe_producer_diagnostics_complete(ProducerDiagnosticsComplete {
            producer_attempt: attempt,
            receipt: health_receipt(crate::decoder_health::Qualification::Unqualified),
        })
    );
    assert_eq!(
        actor
            .producer_operational_snapshot_at(started)
            .diagnostics_qualification,
        Some("unqualified")
    );

    // A receipt for an attempt that is no longer current is not this
    // attempt's evidence.
    let _ = actor
        .begin_producer_attempt_at(started + Duration::from_secs(1))
        .expect("prepublication replacement");
    let snapshot = actor.producer_operational_snapshot_at(started + Duration::from_secs(1));
    assert_eq!(snapshot.decode_fault, None);
    assert_eq!(snapshot.diagnostics_qualification, None);
}

/// A retired session accepts neither. There is nobody left to act on them
/// and nothing left for them to be evidence about.
#[test]
fn a_retired_session_accepts_no_health_observations() {
    let started = Instant::now();
    let mut actor =
        RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
    let attempt = actor
        .begin_producer_attempt_at(started)
        .expect("initial producer attempt");
    // Both guards have two clauses, and both clauses are load-bearing.
    assert!(
        !actor.observe_producer_decode_fault(decode_fault(attempt + 1, 5)),
        "an attempt this session is not running is not this session's evidence"
    );
    assert!(
        !actor.observe_producer_diagnostics_complete(ProducerDiagnosticsComplete {
            producer_attempt: attempt + 1,
            receipt: health_receipt(crate::decoder_health::Qualification::Qualified),
        })
    );

    actor.retired = true;
    assert!(!actor.observe_producer_decode_fault(decode_fault(attempt, 5)));
    assert!(
        !actor.observe_producer_diagnostics_complete(ProducerDiagnosticsComplete {
            producer_attempt: attempt,
            receipt: health_receipt(crate::decoder_health::Qualification::Qualified),
        })
    );
    assert!(actor.latched_decode_fault().is_none());
}

/// The owner-local fence's rejection sites, one row each, in the order
/// `accept_step` reaches them (S-14 M6, plan §3.7). Each row builds the
/// cheapest state that passes every earlier fence, asserts the variant,
/// asserts that the mutable `accept_at` leaves exactly the state the pure
/// step returns, and asserts what the refused packet left behind: nothing,
/// except in the rows marked as preserved findings.
///
/// A row that breaks one fence shows the site is reachable, not that it
/// runs before its neighbour, so the order itself is pinned by a second
/// table: for each pair of adjacent sites, a packet that breaks **both**
/// and must be answered (variant and residue) by the earlier one. Swapping
/// any two adjacent checks fails a pair row, except where the swap cannot
/// be observed:
/// - an older owner epoch / an epoch advance, a first packet / another
///   client instance, a sequence below the last / the same sequence, and
///   the three prepared-successor arms are exclusive branches of one
///   comparison or one `match`, so no packet breaks both;
/// - an epoch advance without a platform / a first packet that is not
///   sequence 1: an advance that is not sequence 1 is answered by the
///   advance's own sequence check first, so no packet reaches both;
/// - another client instance / another platform: both answer
///   `StaleClient` and leave nothing behind, so their order has no
///   observable consequence.
///
/// The last pair row is not adjacent: it pins that the ask lands after the
/// rate floor, so a rate-limited packet cannot move what the viewer is
/// understood to want.
#[test]
fn accept_step_rejection_order() {
    #[derive(Clone, Copy, Debug)]
    enum LeftBehind {
        Nothing,
        /// Finding, preserved: a first packet adopts the generation before
        /// its sequence or platform is judged.
        GenerationAdopted,
        /// Finding, preserved: the ask lands before the prepared-successor
        /// observation can refuse the packet — and with it the new
        /// observational lifetime a changed ask opens.
        AskAdvanced,
        /// Finding, preserved: an owner-epoch advance resets the sequence
        /// space and registers the client before the observation refuses.
        EpochRolledOver,
    }
    struct Row {
        site: &'static str,
        state: ControlState,
        now: Instant,
        generation: String,
        owner_epoch: u64,
        client_instance_id: String,
        sequence: u64,
        acceptance: ControlAcceptance,
        expected: ControlStateError,
        left_behind: LeftBehind,
    }

    let started = Instant::now();
    let later = started + MIN_CONTROL_INTERVAL;
    let generation = uuid::Uuid::new_v4().to_string();
    let client = uuid::Uuid::new_v4().to_string();
    let web = || ControlAcceptance::new(Some(ClientPlatform::Web), None);
    let accepted = |owner_epoch: u64| {
        let mut state = ControlState::default();
        state
            .accept_at(started, &generation, owner_epoch, &client, 1, web())
            .expect("sequence 1 accepted");
        state
    };
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    let successor = |media_origin_ms: i64| PreparedSuccessorAction {
        enhancement: None,
        staged_incarnation_id: staged_incarnation_id.clone(),
        deadline_ms: i64::MAX,
        session_id: "prepared".to_owned(),
        playlist_url: "/api/v1/hls/prepared/index.m3u8".to_owned(),
        control: None,
        media_origin_ms,
        effective_selection: prepared_selection(),
    };
    let prepared = |fingerprint: Option<&str>| {
        let mut state = ControlState::default();
        assert!(state.stage_preparation(
            staged_incarnation_id.clone(),
            generation.clone(),
            i64::MAX,
            None,
        ));
        let acceptance =
            ControlAcceptance::new(Some(ClientPlatform::Web), Some(&successor(42_000)));
        let acceptance = match fingerprint {
            Some(fingerprint) => acceptance.fingerprinted(fingerprint),
            None => acceptance,
        };
        let accepted = state
            .accept_at(started, &generation, 1, &client, 1, acceptance)
            .expect("the staged successor is announced");
        assert!(matches!(accepted.2, ControlAction::Prepare { .. }));
        state
    };
    let asked_for = selection_at(QualitySelection::Manual { height: 480 });
    let row = |site,
               state,
               now,
               owner_epoch,
               client_instance_id: &str,
               sequence,
               acceptance,
               expected,
               left_behind| Row {
        site,
        state,
        now,
        generation: generation.clone(),
        owner_epoch,
        client_instance_id: client_instance_id.to_owned(),
        sequence,
        acceptance,
        expected,
        left_behind,
    };

    let rows = vec![
        row(
            "client instance id is not a UUID",
            accepted(1),
            later,
            1,
            "not-a-uuid",
            2,
            web(),
            ControlStateError::StaleClient,
            LeftBehind::Nothing,
        ),
        Row {
            generation: uuid::Uuid::new_v4().to_string(),
            ..row(
                "another generation",
                accepted(1),
                later,
                1,
                &client,
                2,
                web(),
                ControlStateError::StaleGeneration,
                LeftBehind::Nothing,
            )
        },
        row(
            "an older owner epoch",
            accepted(2),
            later,
            1,
            &client,
            2,
            web(),
            ControlStateError::OwnerChanged,
            LeftBehind::Nothing,
        ),
        row(
            "an owner-epoch advance that is not sequence 1",
            accepted(1),
            later,
            2,
            &client,
            2,
            web(),
            ControlStateError::StaleSequence,
            LeftBehind::Nothing,
        ),
        row(
            "an owner-epoch advance without a platform",
            accepted(1),
            later,
            2,
            &client,
            1,
            ControlAcceptance::new(None, None),
            ControlStateError::StaleClient,
            LeftBehind::Nothing,
        ),
        row(
            "a first packet that is not sequence 1",
            ControlState::default(),
            started,
            0,
            &client,
            2,
            web(),
            ControlStateError::StaleSequence,
            LeftBehind::GenerationAdopted,
        ),
        row(
            "a first packet without a platform",
            ControlState::default(),
            started,
            0,
            &client,
            1,
            ControlAcceptance::new(None, None),
            ControlStateError::StaleClient,
            LeftBehind::GenerationAdopted,
        ),
        row(
            "another client instance",
            accepted(1),
            later,
            1,
            &uuid::Uuid::new_v4().to_string(),
            2,
            web(),
            ControlStateError::StaleClient,
            LeftBehind::Nothing,
        ),
        row(
            "another platform",
            accepted(1),
            later,
            1,
            &client,
            2,
            ControlAcceptance::new(Some(ClientPlatform::Apple), None),
            ControlStateError::StaleClient,
            LeftBehind::Nothing,
        ),
        // Duplicate-sequence semantics, first half: N-1 after N.
        row(
            "a sequence below the last accepted one",
            {
                let mut state = accepted(1);
                state
                    .accept_at(later, &generation, 1, &client, 2, web())
                    .expect("sequence 2 accepted");
                state
            },
            later + MIN_CONTROL_INTERVAL,
            1,
            &client,
            1,
            web(),
            ControlStateError::StaleSequence,
            LeftBehind::Nothing,
        ),
        // Duplicate-sequence semantics, second half: N with another body.
        row(
            "the last sequence with another request fingerprint",
            {
                let mut state = ControlState::default();
                state
                    .accept_at(
                        started,
                        &generation,
                        1,
                        &client,
                        1,
                        web().fingerprinted(&"a".repeat(64)),
                    )
                    .expect("sequence 1 accepted");
                state
            },
            later,
            1,
            &client,
            1,
            ControlAcceptance::new(None, None).fingerprinted(&"b".repeat(64)),
            ControlStateError::StaleSequence,
            LeftBehind::Nothing,
        ),
        row(
            "a replay that drops the vocabulary its Prepare was issued under",
            prepared(None),
            later,
            1,
            &client,
            1,
            ControlAcceptance::new(None, None),
            ControlStateError::Unavailable,
            LeftBehind::Nothing,
        ),
        row(
            "the rate floor",
            accepted(1),
            started + Duration::from_millis(100),
            1,
            &client,
            2,
            web(),
            ControlStateError::RateLimited(150),
            LeftBehind::Nothing,
        ),
        row(
            "a changed payload under the bound staged identity",
            prepared(None),
            later,
            1,
            &client,
            2,
            ControlAcceptance::new(None, Some(&successor(43_000))),
            ControlStateError::Unavailable,
            LeftBehind::Nothing,
        ),
        // Production never builds this state: staging clears the binding.
        // The branch is defensive, and pinned by building it directly.
        row(
            "a binding for another staged identity",
            {
                let mut state = prepared(None);
                state
                    .prepared_action
                    .as_mut()
                    .expect("a bound Prepare")
                    .successor
                    .staged_incarnation_id = uuid::Uuid::new_v4().to_string();
                state
            },
            later,
            1,
            &client,
            2,
            ControlAcceptance::new(None, Some(&successor(42_000))),
            ControlStateError::Unavailable,
            LeftBehind::Nothing,
        ),
        row(
            "an unavailable prepared-successor observation",
            accepted(1),
            later,
            1,
            &client,
            2,
            ControlAcceptance::unavailable(None).asking(&asked_for),
            ControlStateError::Unavailable,
            LeftBehind::AskAdvanced,
        ),
        row(
            "an unavailable observation after an owner-epoch advance",
            accepted(1),
            later,
            2,
            &client,
            1,
            ControlAcceptance::unavailable(Some(ClientPlatform::Web)),
            ControlStateError::Unavailable,
            LeftBehind::EpochRolledOver,
        ),
    ];

    let sites: Vec<&str> = rows.iter().map(|row| row.site).collect();
    assert_eq!(
        sites.len(),
        17,
        "fourteen `return Err` sites, two reachable `?` sites and one preserved rollover finding"
    );

    // Each pair row breaks two fences; the earlier one must answer.
    let pairs = vec![
        Row {
            generation: uuid::Uuid::new_v4().to_string(),
            ..row(
                "order: the client-instance parse before the generation",
                accepted(1),
                later,
                1,
                "not-a-uuid",
                2,
                web(),
                ControlStateError::StaleClient,
                LeftBehind::Nothing,
            )
        },
        Row {
            generation: uuid::Uuid::new_v4().to_string(),
            ..row(
                "order: the generation before an older owner epoch",
                accepted(2),
                later,
                1,
                &client,
                2,
                web(),
                ControlStateError::StaleGeneration,
                LeftBehind::Nothing,
            )
        },
        row(
            "order: an epoch advance's sequence before its platform",
            accepted(1),
            later,
            2,
            &client,
            2,
            ControlAcceptance::new(None, None),
            ControlStateError::StaleSequence,
            LeftBehind::Nothing,
        ),
        row(
            "order: a first packet's sequence before its platform",
            ControlState::default(),
            started,
            0,
            &client,
            2,
            ControlAcceptance::new(None, None),
            ControlStateError::StaleSequence,
            LeftBehind::GenerationAdopted,
        ),
        row(
            "order: another platform before a sequence below the last",
            {
                let mut state = accepted(1);
                state
                    .accept_at(later, &generation, 1, &client, 2, web())
                    .expect("sequence 2 accepted");
                state
            },
            later + MIN_CONTROL_INTERVAL,
            1,
            &client,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Apple), None),
            ControlStateError::StaleClient,
            LeftBehind::Nothing,
        ),
        row(
            "order: another request fingerprint before the dropped Prepare vocabulary",
            prepared(Some(&"a".repeat(64))),
            later,
            1,
            &client,
            1,
            ControlAcceptance::new(None, None).fingerprinted(&"b".repeat(64)),
            ControlStateError::StaleSequence,
            LeftBehind::Nothing,
        ),
        row(
            "order: the dropped Prepare vocabulary before the rate floor",
            prepared(None),
            started + Duration::from_millis(100),
            1,
            &client,
            1,
            ControlAcceptance::new(None, None),
            ControlStateError::Unavailable,
            LeftBehind::Nothing,
        ),
        row(
            "order: the rate floor before a changed payload under the bound staged identity",
            prepared(None),
            started + Duration::from_millis(100),
            1,
            &client,
            2,
            ControlAcceptance::new(None, Some(&successor(43_000))),
            ControlStateError::RateLimited(150),
            LeftBehind::Nothing,
        ),
        row(
            "order: the rate floor before the ask lands",
            accepted(1),
            started + Duration::from_millis(100),
            1,
            &client,
            2,
            ControlAcceptance::unavailable(None).asking(&asked_for),
            ControlStateError::RateLimited(150),
            LeftBehind::Nothing,
        ),
    ];
    assert_eq!(
        pairs.len(),
        9,
        "eight observable adjacent pairs and the ask placement"
    );
    for row in rows.into_iter().chain(pairs) {
        let before = row.state.clone();
        // Snapshotted, not re-read: `before` shares its liveness tokens
        // with every clone below, and the in-place body and `accept_at`
        // (which adopts) legitimately retire them. What the refused packet
        // is measured against is the state as it was handed.
        let handed = format!("{before:?}");
        let request = || ControlRequestView {
            generation: &row.generation,
            owner_epoch: row.owner_epoch,
            client_instance_id: &row.client_instance_id,
            sequence: row.sequence,
            acceptance: row.acceptance.clone(),
        };
        let (next, result) = accept_step(&before, row.now, request());
        assert_eq!(result, Err(row.expected), "{}", row.site);
        assert_eq!(
            format!("{before:?}"),
            handed,
            "{}: the step wrote its input",
            row.site
        );

        let mut mutable = before.clone();
        assert_eq!(
            mutable.accept_at(
                row.now,
                &row.generation,
                row.owner_epoch,
                &row.client_instance_id,
                row.sequence,
                row.acceptance.clone(),
            ),
            Err(row.expected),
            "{}",
            row.site
        );
        assert_eq!(
            format!("{mutable:?}"),
            format!("{next:?}"),
            "{}: accept_at must leave exactly the state accept_step returns",
            row.site
        );
        // The pre-step body itself, run in place on the caller's state as
        // `accept_at` did before the step existed.
        let mut in_place = before.clone();
        assert_eq!(
            in_place.accept_in_place(
                row.now,
                &row.generation,
                row.owner_epoch,
                &row.client_instance_id,
                row.sequence,
                row.acceptance.clone(),
            ),
            Err(row.expected),
            "{}",
            row.site
        );
        assert_eq!(
            format!("{in_place:?}"),
            format!("{next:?}"),
            "{}: the step must leave what the in-place body left",
            row.site
        );

        let mut residue = next.clone();
        match row.left_behind {
            LeftBehind::Nothing => {}
            LeftBehind::GenerationAdopted => {
                assert_eq!(before.generation, None, "{}", row.site);
                assert_eq!(residue.generation.as_deref(), Some(generation.as_str()));
                residue.generation = None;
            }
            LeftBehind::AskAdvanced => {
                assert_eq!(before.desired_digest, None, "{}", row.site);
                assert_eq!(
                    residue.desired_digest.as_deref(),
                    Some(asked_for.desired().digest().as_str()),
                    "{}",
                    row.site
                );
                residue.desired_digest = None;
                assert_eq!(
                    residue.observational_desired_lifetime,
                    before
                        .observational_desired_lifetime
                        .and_then(|lifetime| lifetime.checked_add(1)),
                    "{}: a changed ask opens exactly one new observational lifetime",
                    row.site
                );
                residue.observational_desired_lifetime = before.observational_desired_lifetime;
            }
            LeftBehind::EpochRolledOver => {
                assert_eq!(
                    (before.owner_epoch, next.owner_epoch),
                    (1, 2),
                    "{}",
                    row.site
                );
                assert_eq!(next.last_sequence, 0, "{}", row.site);
                assert_eq!(next.last_accepted_at, None, "{}", row.site);
                assert_eq!(
                    next.client_instance_id,
                    Some(uuid::Uuid::parse_str(&client).expect("uuid")),
                    "{}",
                    row.site
                );
                continue;
            }
        }
        assert_eq!(
            format!("{residue:?}"),
            handed,
            "{}: the refused packet left state behind",
            row.site
        );
    }
}

/// The accepting half of S-14 M6: a run of exchanges that are accepted,
/// replayed, rate-free and epoch-advancing answers the same tuples and
/// leaves the same state through `accept_step` as through the in-place
/// body `accept_at` ran before the step, and the step never writes the
/// state it was handed.
#[test]
fn accept_step_matches_the_in_place_fence_on_accepted_exchanges() {
    let started = Instant::now();
    let generation = uuid::Uuid::new_v4().to_string();
    let client = uuid::Uuid::new_v4().to_string();
    let web = || ControlAcceptance::new(Some(ClientPlatform::Web), None);
    let asked_for = selection_at(QualitySelection::Manual { height: 480 });
    let exchanges: Vec<(Instant, u64, u64, ControlAcceptance)> = vec![
        (started, 1, 1, web()),
        (
            started + MIN_CONTROL_INTERVAL,
            1,
            2,
            web().asking(&asked_for),
        ),
        // The same sequence and body again: a replay, not a rejection.
        (
            started + MIN_CONTROL_INTERVAL * 2,
            1,
            2,
            web().asking(&asked_for),
        ),
        (started + MIN_CONTROL_INTERVAL * 3, 1, 3, web()),
        // An owner-epoch advance restarts the sequence space.
        (started + MIN_CONTROL_INTERVAL * 4, 2, 1, web()),
        (started + MIN_CONTROL_INTERVAL * 5, 2, 2, web()),
    ];
    let mut stepped = ControlState::default();
    let mut in_place = ControlState::default();
    let mut replays = 0;
    for (index, (now, owner_epoch, sequence, acceptance)) in exchanges.into_iter().enumerate() {
        let handed = format!("{stepped:?}");
        let (next, result) = accept_step(
            &stepped,
            now,
            ControlRequestView {
                generation: &generation,
                owner_epoch,
                client_instance_id: &client,
                sequence,
                acceptance: acceptance.clone(),
            },
        );
        assert_eq!(
            format!("{stepped:?}"),
            handed,
            "exchange {index}: the step wrote its input"
        );
        let expected =
            in_place.accept_in_place(now, &generation, owner_epoch, &client, sequence, acceptance);
        let answered = result.map(Disposition::into_tuple);
        assert!(
            answered.is_ok(),
            "exchange {index} was refused: {answered:?}"
        );
        assert_eq!(answered, expected, "exchange {index}");
        assert_eq!(
            format!("{next:?}"),
            format!("{in_place:?}"),
            "exchange {index}"
        );
        if matches!(answered, Ok((ControlDisposition::Replay, ..))) {
            replays += 1;
        }
        stepped = next;
    }
    assert_eq!(replays, 1, "the run exercises exactly one replay");
}

/// `ControlState` clones share their liveness tokens, so the step has to
/// run on private copies: a fence handed out before an exchange that opens
/// a new lifetime stays live until that exchange is **adopted**, and a
/// fence that outlives an adopted exchange which kept the lifetime is
/// still connected to the adopted state's token.
#[test]
fn accept_step_retires_outstanding_fences_only_on_adoption() {
    let started = Instant::now();
    let generation = uuid::Uuid::new_v4().to_string();
    let client = uuid::Uuid::new_v4().to_string();
    let selected = selection_at(QualitySelection::Manual { height: 480 });
    let away = selection_at(QualitySelection::Manual { height: 720 });
    let identity =
        |sequence, fingerprint: &str, selection: &ClientSelection| AcceptedControlIdentity {
            generation: generation.clone(),
            owner_epoch: 1,
            client_instance_id: client.clone(),
            sequence,
            fingerprint: fingerprint.to_owned(),
            desired_digest: selection.desired().digest(),
        };
    let mut state = ControlState::default();
    state
        .accept_at(
            started,
            &generation,
            1,
            &client,
            1,
            ControlAcceptance::new(Some(ClientPlatform::Web), None)
                .asking(&selected)
                .fingerprinted("origin"),
        )
        .expect("sequence 1 accepted");
    let origin = state
        .accepted_observation(identity(1, "origin", &selected))
        .expect("the accepted exchange mints a fence");

    let moved_at = started + MIN_CONTROL_INTERVAL;
    let moved = || {
        ControlAcceptance::new(None, None)
            .asking(&away)
            .fingerprinted("away")
    };
    let (next, result) = accept_step(
        &state,
        moved_at,
        ControlRequestView {
            generation: &generation,
            owner_epoch: 1,
            client_instance_id: &client,
            sequence: 2,
            acceptance: moved(),
        },
    );
    assert!(result.is_ok(), "the changed ask is accepted");
    assert!(
        origin.still_live(),
        "a step nobody adopted must not retire the caller's fence"
    );
    assert!(state.observation_is_current(&origin));
    assert!(
        !next.observation_is_current(&origin),
        "the stepped state has opened a new lifetime"
    );

    state
        .accept_at(moved_at, &generation, 1, &client, 2, moved())
        .expect("sequence 2 adopted");
    assert!(
        !origin.still_live(),
        "adopting the new lifetime retires the fence handed out before it"
    );

    let latest = state
        .accepted_observation(identity(2, "away", &away))
        .expect("the new lifetime mints a fence");
    state
        .accept_at(
            moved_at + MIN_CONTROL_INTERVAL,
            &generation,
            1,
            &client,
            3,
            ControlAcceptance::new(None, None)
                .asking(&away)
                .fingerprinted("poll"),
        )
        .expect("sequence 3 adopted");
    assert!(
        latest.still_live() && state.observation_is_current(&latest),
        "an exchange that keeps the lifetime keeps its fences"
    );
    state.invalidate_observational_attachment();
    assert!(
        !latest.still_live(),
        "the fence is still attached to the adopted state's token"
    );
}

/// plurx Player's first control exchange, as its own test suite pins it
/// (`clients/linux-player/tests/test_launcher.py::ControlContract`). The
/// launcher once sent no capabilities: the server answered its first exchange
/// 400 and every later one 409, so the control plane never saw it play.
#[test]
fn the_linux_player_first_exchange_is_a_valid_v1_request() {
    let request: ControlRequestV1 = serde_json::from_str(include_str!(
        "../../../../clients/linux-player/tests/fixtures/control-request-v1.json"
    ))
    .expect("the launcher's control body parses as a v1 request");
    assert_eq!(request.sequence, 1);
    assert_eq!(
        request.capabilities.as_ref().map(|caps| caps.platform),
        Some(ClientPlatform::Linux)
    );
    assert_eq!(request.validate(Some(7_200_000), 6_000), Ok(()));
}
