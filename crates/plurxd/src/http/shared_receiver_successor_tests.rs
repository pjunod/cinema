//! B prepared successor ownership, offer, acknowledgement and replay.
use super::super::tests::{registered, retired_within, superseding};
use super::*;
use crate::playback_control as pc;
use std::time::Duration;

fn selection() -> ClientSelection {
    ClientSelection {
        quality: QualitySelection::Auto {
            height: None,
            candidate_id: None,
        },
        audio_track: None,
        subtitle: pc::SubtitleSelection {
            mode: SubtitleMode::Off,
            track: None,
        },
        audio_offset_ms: 0,
        codec: CodecPolicy::Auto,
        dynamic_range: DynamicRangePolicy::Auto,
    }
}
fn manual(height: i64) -> ClientSelection {
    ClientSelection {
        quality: QualitySelection::Manual { height },
        ..selection()
    }
}
fn request(generation: &str, sequence: u64, selection: ClientSelection) -> ControlRequestV1 {
    ControlRequestV1 {
        intent: None,
        protocol: pc::PROTOCOL_V1.to_owned(),
        generation: generation.to_owned(),
        control_epoch: 1,
        client_instance_id: "6f1c2d1e-7f9a-4b8e-9d3c-2a1b0c9d8e7f".to_owned(),
        sequence,
        demand: pc::PlaybackDemand::Active,
        position_ms: 61_250,
        buffered_from_ms: Some(60_000),
        buffered_through_ms: 70_000,
        playback_rate: 1.0,
        render_state: pc::RenderState::Rendering,
        seek_target_ms: None,
        observed_download_bps: None,
        selection,
        capabilities: Some(pc::DynamicCapabilities {
            presentation_target: None,
            decoder_caps: None,
            platform: pc::ClientPlatform::Web,
            max_height: 1080,
            codecs: vec![CodecPolicy::H264],
            dynamic_ranges: vec![DynamicRangePolicy::Sdr],
            dual_player_preparation: true,
        }),
        observation: None,
        acknowledgement: None,
        supported_actions: Some(vec![
            "hold".to_owned(),
            "retry_resource".to_owned(),
            PREPARE_REPLACEMENT_ACTION.to_owned(),
            SHARED_PREPARE_REPLACEMENT_ACTION.to_owned(),
        ]),
    }
}
fn acknowledged(
    mut request: ControlRequestV1,
    action_id: &str,
    state: AcknowledgementState,
    origin: Option<i64>,
) -> ControlRequestV1 {
    request.acknowledgement = Some(pc::ActionAcknowledgement {
        action_id: action_id.to_owned(),
        state,
        buffered_through_ms: None,
        committed_media_origin_ms: origin,
        first_frame_unix_ms: (state == AcknowledgementState::Committed)
            .then_some(1_800_000_000_000),
    });
    request
}
fn response(request: &ControlRequestV1) -> ControlResponseV1 {
    let server_time_unix_ms = clock_ms();
    ControlResponseV1 {
        protocol: pc::PROTOCOL_V1.to_owned(),
        generation: request.generation.clone(),
        control_epoch: request.control_epoch,
        accepted_sequence: request.sequence,
        server_time_unix_ms,
        lease: pc::PlaybackLeaseView {
            state: "active".to_owned(),
            renew_after_ms: pc::NEXT_EXCHANGE_MS,
            expires_at_unix_ms: server_time_unix_ms + 30_000,
        },
        delivery: pc::DeliveryView {
            presentation: "vod".to_owned(),
            producer_state: "vod".to_owned(),
            produced_through_ms: Some(60_000),
            fetched_through_ms: 30_000,
            delivered_bps: Some(1_000_000),
            delivered_idle_ms: Some(10),
            recent_producer_speed: None,
            client_runway_ms: 20_000,
            admitted: Some(true),
            hold_reason: None,
            producer_decision: None,
            subtitle_readiness: None,
            preparation: Some("none".to_owned()),
            owner_node_hash: pc::node_hash("receiver-node"),
            owner_epoch: request.control_epoch,
        },
        effective_selection: EffectiveSelection {
            candidate_id: None,
            quality_auto: true,
            height: 720,
            audio_track: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            codec: "source".to_owned(),
            dynamic_range: None,
        },
        action: ControlAction::None,
    }
}
/// Publish an HLS Start under a fresh B session and return its id.
fn publish_hls(entry: &Arc<ReceiverStartInner>) -> Uuid {
    let session = Uuid::new_v4();
    let mut start: StartResponse = serde_json::from_value(serde_json::json!({
        "session_id": session,
        "playlist_url": format!("/api/v1/hls/{session}/index.m3u8"),
        "duration_ms": 600_000, "start_seconds": 61.25, "media_origin_ms": 0,
        "height": 480, "encoder": "vod", "vod": true, "ladder": [], "plan_notes": []
    }))
    .expect("projected B Start");
    start.control = pc::ControlBootstrap::new(
        &session.to_string(),
        &entry.intent.recipe.source_request_id.to_string(),
        1,
        pc::VOD_LEASE_TIMEOUT_MS,
    );
    entry.state.lock().expect("owner").start = Some(Ok(ReceiverPublished::Hls(Box::new(start))));
    session
}
fn generation(entry: &Arc<ReceiverStartInner>) -> String {
    entry.intent.recipe.source_request_id.to_string()
}
/// A registered, never-claimed successor whose retirement owner ends at once.
fn successor_of(
    registry: &ReceiverStartRegistry,
    predecessor: &Arc<ReceiverStartInner>,
    selection: &ClientSelection,
    deadline_ms: i64,
) -> Arc<ReceiverStartInner> {
    let json = successor_request(
        &predecessor.intent.recipe.request_json,
        selection,
        61_250,
        Uuid::new_v4(),
    )
    .expect("expressible ask");
    let (entry, _) = registry
        .register_successor(
            &ReceiverStartActor(predecessor.clone()),
            json,
            &selection.desired().digest(),
            deadline_ms,
        )
        .expect("successor registered");
    *entry.start_task.lock().expect("Start handle") = Some(tokio::spawn(async {}));
    entry
}
/// A published predecessor with a published successor installed and offered.
fn offered_pair(
    registry: &ReceiverStartRegistry,
) -> (
    Arc<ReceiverStartInner>,
    Arc<ReceiverStartInner>,
    Uuid,
    String,
) {
    let predecessor = registered(registry, "predecessor", "player", 1);
    publish_hls(&predecessor);
    let ask = manual(480);
    let deadline = clock_ms() + SUCCESSOR_DEADLINE_MS;
    let successor = successor_of(registry, &predecessor, &ask, deadline);
    let actor = ReceiverStartActor(predecessor.clone());
    assert!(actor.install_successor(
        &ReceiverStartActor(successor.clone()),
        ask.desired().digest(),
        deadline,
    ));
    let session = publish_hls(&successor);
    let offer = request(&generation(&predecessor), 5, ask);
    let mut answer = response(&offer);
    actor.compose_preparation(&mut answer, &offer, clock_ms());
    let ControlAction::Prepare { action_id, .. } = answer.action else {
        panic!("a published successor is offered");
    };
    (predecessor, successor, session, action_id)
}

#[test]
fn sharing_receiver_successor_request_carries_the_ask_at_the_sampled_position() {
    let original = serde_json::json!({
        "playback_id": "player", "request_id": Uuid::new_v4(), "start": 30.5,
        "copy": true, "height": 1080, "quality_auto": false, "audio": 1,
        "native_subtitles": true, "subtitle": 2, "previous_session_id": null,
        "presentation": "vod", "overrides": {"force": "original"}, "block_budget_secs": 12,
    })
    .to_string();
    let id = Uuid::new_v4();
    let mut ask = manual(480);
    ask.audio_track = Some(2);
    ask.subtitle = pc::SubtitleSelection {
        mode: SubtitleMode::Native,
        track: Some(3),
    };
    let built: Value =
        serde_json::from_str(&successor_request(&original, &ask, 61_250, id).expect("manual"))
            .expect("JSON");
    assert_eq!(built["request_id"], id.to_string());
    assert_eq!(
        built["playback_id"], "player",
        "the player's own playback id"
    );
    assert_eq!(
        built["start"], 61.25,
        "the Source plans from the sampled position"
    );
    assert_eq!(built["copy"], false);
    assert_eq!(built["height"], 480);
    assert_eq!(built["quality_auto"], false);
    assert_eq!(built["overrides"]["force"], "transcode");
    assert_eq!(built["audio"], 2);
    assert_eq!(
        (built["native_subtitles"].clone(), built["subtitle"].clone()),
        (true.into(), 3.into())
    );
    assert_eq!(
        built["block_budget_secs"], 12,
        "untouched fields are carried"
    );
    assert!(
        built.get("previous_session_id").is_none(),
        "no lineage field"
    );
    let typed: crate::http::hls::CreateSession =
        serde_json::from_value(built.clone()).expect("an ordinary create body");
    assert_eq!(typed.height, Some(480));
    assert!(crate::sharing::receiver_initial_request_supported(
        &built.to_string()
    ));

    let built: Value =
        serde_json::from_str(&successor_request(&original, &selection(), 0, id).expect("auto"))
            .expect("JSON");
    assert_eq!(built["quality_auto"], true);
    assert!(
        built.get("copy").is_none() && built.get("height").is_none(),
        "the planner decides"
    );
    assert!(
        built.get("overrides").is_none(),
        "an empty override is dropped"
    );
    assert!(built.get("subtitle").is_none() && built.get("audio").is_none());
    assert!(built.get("native_subtitles").is_none());
    let original_ask = ClientSelection {
        quality: QualitySelection::Original,
        ..selection()
    };
    let built: Value = serde_json::from_str(
        &successor_request(&original, &original_ask, 0, id).expect("original"),
    )
    .expect("JSON");
    assert_eq!(
        (built["copy"].clone(), built["overrides"]["force"].clone()),
        (true.into(), "original".into())
    );
    assert!(built.get("height").is_none());

    // Burn has its own explicit Source ask; codec and dynamic-range policy
    // changes that cannot be represented remain typed declines.
    let burn = ClientSelection {
        subtitle: pc::SubtitleSelection {
            mode: SubtitleMode::Burn,
            track: Some(1),
        },
        ..selection()
    };
    let codec = ClientSelection {
        codec: CodecPolicy::H264,
        ..selection()
    };
    let grade = ClientSelection {
        dynamic_range: DynamicRangePolicy::Hdr10,
        ..selection()
    };
    let built: Value =
        serde_json::from_str(&successor_request(&original, &burn, 0, id).expect("burn"))
            .expect("burn JSON");
    assert_eq!(built["subtitle_burn"], 1);
    assert_eq!(built["copy"], false);
    assert!(built.get("native_subtitles").is_none());
    assert!(built.get("subtitle").is_none());
    for refused in [codec, grade] {
        assert!(successor_request(&original, &refused, 0, id).is_none());
    }
    let direct =
        serde_json::json!({"playback_id":"player","request_id":"r","presentation":"direct"});
    let reopened =
        serde_json::json!({"playback_id":"player","request_id":"r","reopen_reason":"stall"});
    for refused in [direct, reopened] {
        assert!(successor_request(&refused.to_string(), &selection(), 0, id).is_none());
    }
}

#[tokio::test]
async fn sharing_receiver_prepared_publication_never_supersedes_its_predecessor() {
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let predecessor = registered(&registry, "predecessor", "player", 1);
    publish_hls(&predecessor);
    let successor = successor_of(
        &registry,
        &predecessor,
        &manual(480),
        clock_ms() + SUCCESSOR_DEADLINE_MS,
    );
    assert!(successor.awaiting_commit());
    assert_eq!(successor.playback_id, predecessor.playback_id);
    assert_ne!(
        crate::sharing::receiver_playback_id(&successor.intent.recipe),
        crate::sharing::receiver_playback_id(&predecessor.intent.recipe),
        "its own Store and Source playback"
    );
    publish_hls(&successor);
    assert_eq!(registry.supersede_on_publication(&state, &successor), 0);
    assert!(!superseding(&predecessor), "the predecessor keeps serving");
    // A client that went around the handoff with a fresh Start retires both.
    let reopen = registered(&registry, "reopen", "player", 1);
    publish_hls(&reopen);
    assert_eq!(registry.supersede_on_publication(&state, &reopen), 2);
    assert!(superseding(&predecessor) && superseding(&successor));
    for owner in [predecessor, successor] {
        assert!(retired_within(&ReceiverStartActor(owner), Duration::from_secs(5)).await);
    }
}

#[tokio::test]
async fn sharing_receiver_commit_supersedes_exact_predecessor_and_settles_the_slot() {
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let other_player = registered(&registry, "other-player", "other", 1);
    publish_hls(&other_player);
    let (predecessor, successor, _, action_id) = offered_pair(&registry);
    let actor = ReceiverStartActor(predecessor.clone());
    let commit = acknowledged(
        request(&generation(&predecessor), 6, manual(480)),
        &action_id,
        AcknowledgementState::Committed,
        Some(0),
    );
    assert_eq!(
        actor.plan_acknowledgement(&state, &commit, clock_ms()),
        Ok(AckPlan::Commit(action_id.clone()))
    );
    let handoff = actor
        .commit_successor(&state, &registry, &action_id)
        .expect("committed");
    assert_eq!(handoff.superseding(), 1);
    assert!(
        !superseding(&predecessor),
        "the predecessor serves until the commit answer's writer is released"
    );
    drop(handoff);
    assert!(superseding(&predecessor), "the exact predecessor retires");
    assert!(!superseding(&successor) && !successor.awaiting_commit());
    assert!(
        !successor.prepared_expired(i64::MAX),
        "a committed session has no deadline"
    );
    assert!(!superseding(&other_player));
    // The settled slot answers nothing further: a second commit is stale.
    assert!(matches!(
        actor.commit_successor(&state, &registry, &action_id),
        Err(ReceiverStartError::Conflict)
    ));
    assert!(actor
        .plan_acknowledgement(&state, &commit, clock_ms())
        .is_err());
    assert!(retired_within(&actor, Duration::from_secs(5)).await);
    assert!(
        !superseding(&successor),
        "retiring the predecessor keeps a committed successor"
    );
}

#[tokio::test]
async fn sharing_receiver_abort_withdraws_successor_and_keeps_predecessor() {
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let (predecessor, successor, _, action_id) = offered_pair(&registry);
    let actor = ReceiverStartActor(predecessor.clone());
    for (sequence, state_) in [
        (6, AcknowledgementState::MetadataReady),
        (7, AcknowledgementState::Aborted),
    ] {
        let ack = acknowledged(
            request(&generation(&predecessor), sequence, manual(480)),
            &action_id,
            state_,
            None,
        );
        let expected = if state_ == AcknowledgementState::Aborted {
            AckPlan::Abort(action_id.clone())
        } else {
            AckPlan::Report
        };
        assert_eq!(
            actor.plan_acknowledgement(&state, &ack, clock_ms()),
            Ok(expected)
        );
    }
    assert!(actor.abort_successor(&state, &action_id));
    assert!(superseding(&successor), "the successor is withdrawn");
    assert!(!superseding(&predecessor), "the predecessor keeps serving");
    assert!(
        !actor.abort_successor(&state, &action_id),
        "nothing left to abort"
    );
    let ask = request(&generation(&predecessor), 8, manual(480));
    let mut answer = response(&ask);
    actor.compose_preparation(&mut answer, &ask, clock_ms());
    assert_eq!(answer.delivery.preparation.as_deref(), Some("none"));
    assert_eq!(answer.action, ControlAction::None);
    // Its single retirement owner frees its registry slot.
    assert!(retired_within(&ReceiverStartActor(successor), Duration::from_secs(5)).await);
}

#[tokio::test]
async fn sharing_receiver_predecessor_retirement_withdraws_uncommitted_successor() {
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let (predecessor, successor, _, _) = offered_pair(&registry);
    ReceiverStartActor(predecessor.clone())
        .begin_retirement(state, ReceiverRetirementReason::Deleted);
    assert!(superseding(&successor), "no viewer is left for it");
    for owner in [predecessor, successor] {
        assert!(retired_within(&ReceiverStartActor(owner), Duration::from_secs(5)).await);
    }
}

#[tokio::test]
async fn sharing_receiver_prepare_names_only_b_urls_and_bootstrap() {
    let registry = ReceiverStartRegistry::default();
    let (predecessor, successor, session, action_id) = offered_pair(&registry);
    let actor = ReceiverStartActor(predecessor.clone());
    let offer = request(&generation(&predecessor), 6, manual(480));
    let mut answer = response(&offer);
    actor.compose_preparation(&mut answer, &offer, clock_ms());
    assert_eq!(answer.delivery.preparation.as_deref(), Some("offered"));
    let ControlAction::Prepare {
        action_id: again,
        session_id,
        playlist_url,
        control,
        media_origin_ms,
        effective_selection,
    } = &answer.action
    else {
        panic!("offered");
    };
    assert_eq!(again, &action_id, "every offer names the same staging");
    assert_eq!(session_id, &session.to_string());
    assert_eq!(playlist_url, &format!("/api/v1/hls/{session}/index.m3u8"));
    let control = control.as_ref().expect("successor control bootstrap");
    assert_eq!(control.url, format!("/api/v1/hls/{session}/control"));
    assert_eq!(control.generation, generation(&successor));
    assert_eq!(*media_origin_ms, 0);
    assert_eq!(effective_selection.height, 480);
    assert_eq!(effective_selection.codec, "server_selected");
    // Nothing of the Source or of the predecessor's own tuple is named.
    let wire = serde_json::to_string(&answer.action).expect("action");
    assert!(!wire.contains(&generation(&predecessor)));
    assert!(!wire.contains("sharing/v1"));
    assert!(answer.is_valid_for(&pc::ControlRelayRequest {
        session_id: Uuid::new_v4().to_string(),
        generation: offer.generation.clone(),
        expected_owner_node_id: "receiver-node".to_owned(),
        expected_owner_epoch: 1,
        deadline_unix_ms: 0,
        control: offer.clone(),
    }));
    // A client that cannot hold a second player is never offered one, and
    // neither is a client that made only the Local promise: it would keep
    // beating progress on the session it left, so it reopens (P0).
    let mut passive = offer.clone();
    passive.supported_actions = Some(vec!["hold".to_owned()]);
    let mut local_only = offer.clone();
    local_only.supported_actions = Some(vec![PREPARE_REPLACEMENT_ACTION.to_owned()]);
    for declined in [passive, local_only] {
        let mut answer = response(&declined);
        actor.compose_preparation(&mut answer, &declined, clock_ms());
        assert_eq!(answer.action, ControlAction::None);
        assert_eq!(answer.delivery.preparation.as_deref(), Some("none"));
    }
}

#[tokio::test]
async fn sharing_receiver_prepare_offered_only_after_successor_publication() {
    let registry = ReceiverStartRegistry::default();
    let predecessor = registered(&registry, "predecessor", "player", 1);
    publish_hls(&predecessor);
    let ask = manual(480);
    let deadline = clock_ms() + SUCCESSOR_DEADLINE_MS;
    let successor = successor_of(&registry, &predecessor, &ask, deadline);
    let actor = ReceiverStartActor(predecessor.clone());
    assert!(actor.install_successor(
        &ReceiverStartActor(successor.clone()),
        ask.desired().digest(),
        deadline
    ));
    let compose = |request: &ControlRequestV1, action: ControlAction| {
        let mut answer = response(request);
        answer.action = action;
        actor.compose_preparation(&mut answer, request, clock_ms());
        (answer.delivery.preparation.clone(), answer.action)
    };
    let offer = request(&generation(&predecessor), 5, ask.clone());
    assert_eq!(
        compose(&offer, ControlAction::None),
        (Some("staging".to_owned()), ControlAction::None),
        "not yet published: staging, never an offer"
    );
    publish_hls(&successor);
    let hold = ControlAction::Hold {
        reason: pc::HoldReason::Demand,
        revisit_after_ms: 1_000,
    };
    assert_eq!(
        compose(&offer, hold.clone()),
        (Some("staging".to_owned()), hold),
        "an outranking action is offered next exchange"
    );
    let (preparation, action) = compose(&offer, ControlAction::None);
    assert_eq!(preparation.as_deref(), Some("offered"));
    assert!(matches!(action, ControlAction::Prepare { .. }));
    // Measured against this exchange's ask: a left ask is not staging.
    let left = request(&generation(&predecessor), 6, manual(720));
    assert_eq!(
        compose(&left, ControlAction::None).0.as_deref(),
        Some("none")
    );
    // A successor that failed is a decline.
    successor.state.lock().expect("owner").start = Some(Err(ReceiverStartError::Unresolved));
    assert_eq!(
        compose(&offer, ControlAction::None).0.as_deref(),
        Some("none")
    );
}

#[tokio::test]
async fn sharing_receiver_ack_replays_exactly_and_refuses_a_changed_replay() {
    let registry = ReceiverStartRegistry::default();
    let (predecessor, _, _, action_id) = offered_pair(&registry);
    let actor = ReceiverStartActor(predecessor.clone());
    let ack = acknowledged(
        request(&generation(&predecessor), 6, manual(480)),
        &action_id,
        AcknowledgementState::BufferReady,
        None,
    );
    let mut ack = ack;
    ack.acknowledgement
        .as_mut()
        .expect("ack")
        .buffered_through_ms = Some(70_000);
    assert!(!actor.has_acknowledgement_replies());
    assert!(actor.acknowledgement_replay(&ack).is_none());
    actor.retain_acknowledgement_reply(&ack, b"{\"exact\":1}");
    assert!(matches!(
        actor.acknowledgement_replay(&ack),
        Some(AckReplay::Exact(body)) if body == b"{\"exact\":1}"
    ));
    let mut changed = ack.clone();
    changed.position_ms += 1;
    assert!(matches!(
        actor.acknowledgement_replay(&changed),
        Some(AckReplay::Changed)
    ));
    let mut plain = ack.clone();
    plain.acknowledgement = None;
    assert!(
        actor.acknowledgement_replay(&plain).is_none(),
        "only acknowledgements replay here"
    );
    // Bounded: the oldest answer leaves first.
    for sequence in 7..(7 + MAX_ACK_REPLIES as u64) {
        let mut later = ack.clone();
        later.sequence = sequence;
        actor.retain_acknowledgement_reply(&later, b"{}");
    }
    assert!(actor.acknowledgement_replay(&ack).is_none());
}

#[tokio::test]
async fn sharing_receiver_stale_acknowledgements_are_refused_before_the_source() {
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let (predecessor, successor, _, action_id) = offered_pair(&registry);
    let actor = ReceiverStartActor(predecessor.clone());
    let base = || request(&generation(&predecessor), 6, manual(480));
    let refused = [
        // An action this slot never offered.
        acknowledged(
            base(),
            &Uuid::new_v4().to_string(),
            AcknowledgementState::Committed,
            Some(0),
        ),
        // A commit on another timeline origin than the offer's.
        acknowledged(base(), &action_id, AcknowledgementState::Committed, Some(1)),
        // A commit whose ask is no longer the one this successor carries.
        acknowledged(
            request(&generation(&predecessor), 6, manual(720)),
            &action_id,
            AcknowledgementState::Committed,
            Some(0),
        ),
    ];
    for ack in &refused {
        assert!(actor.plan_acknowledgement(&state, ack, clock_ms()).is_err());
    }
    assert!(!superseding(&successor), "a refused commit changes nothing");
    // Late: past the deadline the acknowledgement is refused and the
    // successor withdrawn.
    let late = acknowledged(base(), &action_id, AcknowledgementState::Committed, Some(0));
    assert!(actor
        .plan_acknowledgement(&state, &late, clock_ms() + SUCCESSOR_DEADLINE_MS + 1)
        .is_err());
    assert!(superseding(&successor));
    assert!(!superseding(&predecessor));
    assert!(actor
        .plan_acknowledgement(&state, &late, clock_ms())
        .is_err());
    assert!(retired_within(&ReceiverStartActor(successor), Duration::from_secs(5)).await);
}

#[tokio::test]
async fn sharing_receiver_observe_ask_dispatches_once_and_withdraws_a_left_ask() {
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let predecessor = registered(&registry, "predecessor", "player", 1);
    publish_hls(&predecessor);
    let actor = ReceiverStartActor(predecessor.clone());
    let at = |sequence, ask: ClientSelection| request(&generation(&predecessor), sequence, ask);
    assert_eq!(
        actor.observe_ask(&state, &at(1, selection()), true),
        None,
        "the created ask"
    );
    assert_eq!(actor.observe_ask(&state, &at(2, selection()), true), None);
    // Gates: the switch, a dual-player client and the action vocabulary.
    assert_eq!(actor.observe_ask(&state, &at(3, manual(480)), false), None);
    let mut single = at(3, manual(480));
    single
        .capabilities
        .as_mut()
        .expect("caps")
        .dual_player_preparation = false;
    assert_eq!(actor.observe_ask(&state, &single, true), None);
    let mut passive = at(3, manual(480));
    passive.capabilities = None; // the last declaration (false) stands
    assert_eq!(actor.observe_ask(&state, &passive, true), None);
    let mut no_prepare = at(3, manual(480));
    no_prepare.supported_actions = Some(vec!["hold".to_owned()]);
    assert_eq!(actor.observe_ask(&state, &no_prepare, true), None);
    let mut local_only = at(3, manual(480));
    local_only.supported_actions = Some(vec![PREPARE_REPLACEMENT_ACTION.to_owned()]);
    assert_eq!(actor.observe_ask(&state, &local_only, true), None);
    let digest = manual(480).desired().digest();
    assert_eq!(
        actor.observe_ask(&state, &at(4, manual(480)), true),
        Some(digest.clone())
    );
    assert_eq!(
        actor.observe_ask(&state, &at(5, manual(480)), true),
        None,
        "dispatched once"
    );
    let deadline = clock_ms() + SUCCESSOR_DEADLINE_MS;
    let successor = successor_of(&registry, &predecessor, &manual(480), deadline);
    assert!(actor.install_successor(&ReceiverStartActor(successor.clone()), digest, deadline));
    // The viewer moved on: the staged successor is withdrawn and the new ask
    // waits until it has retired and freed its slots.
    assert_eq!(actor.observe_ask(&state, &at(6, manual(720)), true), None);
    assert!(superseding(&successor));
    assert!(retired_within(&ReceiverStartActor(successor), Duration::from_secs(5)).await);
    assert_eq!(
        actor.observe_ask(&state, &at(7, manual(720)), true),
        Some(manual(720).desired().digest())
    );
}

#[tokio::test]
async fn sharing_receiver_successor_deadline_and_progress_follow_the_commit() {
    let registry = ReceiverStartRegistry::default();
    let predecessor = registered(&registry, "predecessor", "player", 1);
    let deadline = clock_ms() + 60_000;
    let successor = successor_of(&registry, &predecessor, &manual(480), deadline);
    assert!(
        successor.awaiting_commit(),
        "progress beats are refused until commit"
    );
    let state = Arc::new(crate::http::source_actor_test_state());
    let first = request(&generation(&successor), 1, manual(480));
    let changed = request(&generation(&successor), 2, manual(720));
    let uncommitted = ReceiverStartActor(successor.clone());
    for exchange in [&first, &changed] {
        assert_eq!(
            uncommitted.observe_ask(&state, exchange, true),
            None,
            "an uncommitted successor stages nothing of its own"
        );
    }
    assert!(!successor.prepared_expired(deadline - 1));
    assert!(
        successor.prepared_expired(deadline),
        "the owner tick withdraws it here"
    );
    assert!(!predecessor.awaiting_commit() && !predecessor.prepared_expired(i64::MAX));
    successor
        .state
        .lock()
        .expect("owner")
        .prepared
        .as_mut()
        .expect("role")
        .committed = true;
    assert!(!successor.awaiting_commit() && !successor.prepared_expired(i64::MAX));
}

#[tokio::test]
async fn sharing_receiver_viewer_cap_declines_typed() {
    let registry = ReceiverStartRegistry::default();
    let predecessor = registered(&registry, "predecessor", "player", 1);
    publish_hls(&predecessor);
    for n in 0..7 {
        registered(&registry, &format!("other-{n}"), "other", 2);
    }
    let ask = manual(480);
    let json = successor_request(
        &predecessor.intent.recipe.request_json,
        &ask,
        0,
        Uuid::new_v4(),
    )
    .expect("ask");
    assert!(matches!(
        registry.register_successor(
            &ReceiverStartActor(predecessor.clone()),
            json,
            &ask.desired().digest(),
            clock_ms() + SUCCESSOR_DEADLINE_MS,
        ),
        Err(ReceiverStartError::Capacity)
    ));
    // No slot was taken, so the exchange answers a typed decline.
    let offer = request(&generation(&predecessor), 5, ask);
    let mut answer = response(&offer);
    ReceiverStartActor(predecessor).compose_preparation(&mut answer, &offer, clock_ms());
    assert_eq!(answer.delivery.preparation.as_deref(), Some("none"));
}

/// Record the B session id a claim would have recorded for this attempt.
fn own(entry: &Arc<ReceiverStartInner>, session: Uuid) {
    entry.state.lock().expect("owner").owner = Some(ReceiverSourceOwner {
        incarnation_id: entry.intent.recipe.source_request_id,
        session_id: session,
        owner_node_id: "receiver-node".to_owned(),
        owner_epoch: 1,
        request_id: entry.request_id.clone(),
        now_ms: clock_ms(),
        lease_expires_at_ms: clock_ms() + 30_000,
    });
}

#[tokio::test]
async fn sharing_receiver_successor_media_relays_before_commit() {
    let registry = ReceiverStartRegistry::default();
    let (predecessor, successor, session, _) = offered_pair(&registry);
    own(&successor, session);
    let predecessor_session = Uuid::new_v4();
    own(&predecessor, predecessor_session);
    // The playlist, segments, status and control of the offered successor
    // dispatch to its own owner exactly as any published B session's do,
    // while the client is still watching (and beating on) the predecessor.
    let served = registry.by_session(session).expect("successor relayed");
    assert!(Arc::ptr_eq(&served.0, &successor));
    assert!(
        served.0.awaiting_commit(),
        "uncommitted: its progress beats are refused"
    );
    let watched = registry
        .by_session(predecessor_session)
        .expect("predecessor still relayed");
    assert!(Arc::ptr_eq(&watched.0, &predecessor));
    assert!(!watched.0.awaiting_commit());
}

#[tokio::test]
async fn sharing_receiver_lost_commit_reconciles_after_predecessor_retired() {
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let (predecessor, successor, _, action_id) = offered_pair(&registry);
    let predecessor_session = Uuid::new_v4();
    own(&predecessor, predecessor_session);
    let actor = ReceiverStartActor(predecessor.clone());
    let commit = acknowledged(
        request(&generation(&predecessor), 6, manual(480)),
        &action_id,
        AcknowledgementState::Committed,
        Some(0),
    );
    assert_eq!(
        actor.plan_acknowledgement(&state, &commit, clock_ms()),
        Ok(AckPlan::Commit(action_id.clone()))
    );
    drop(
        actor
            .commit_successor(&state, &registry, &action_id)
            .expect("committed"),
    );
    actor.retain_acknowledgement_reply(&commit, b"{\"committed\":1}");
    assert!(retired_within(&actor, Duration::from_secs(5)).await);
    // Still registered: the actor itself replays.
    assert!(matches!(
        actor.acknowledgement_replay(&commit),
        Some(AckReplay::Exact(body)) if body == b"{\"committed\":1}"
    ));
    // The next registration prunes the retired predecessor into a tombstone.
    registered(&registry, "later", "other", 2);
    assert!(registry.by_session(predecessor_session).is_none());
    assert!(registry.settled_acknowledgement_replies(predecessor_session));
    assert!(matches!(
        registry.settled_acknowledgement_replay(predecessor_session, &commit),
        Some(AckReplay::Exact(body)) if body == b"{\"committed\":1}"
    ));
    let mut changed = commit.clone();
    changed.position_ms += 1;
    assert!(matches!(
        registry.settled_acknowledgement_replay(predecessor_session, &changed),
        Some(AckReplay::Changed)
    ));
    let mut next = commit.clone();
    next.sequence += 1;
    assert!(
        registry
            .settled_acknowledgement_replay(predecessor_session, &next)
            .is_none(),
        "only an answered exchange replays; nothing is written"
    );
    assert!(registry
        .settled_acknowledgement_replay(Uuid::new_v4(), &commit)
        .is_none());
    assert!(
        !superseding(&successor),
        "the committed successor is the session now"
    );
}

#[tokio::test]
async fn sharing_receiver_commit_answer_writer_is_released_before_the_predecessor_retires() {
    // Records, at the moment the commit answer's connection guard drops,
    // whether the predecessor had already begun retiring. Before the fix the
    // commit superseded at once, so the predecessor's connection monitor
    // could cut the transport still carrying the answer.
    struct Writer(Arc<ReceiverStartInner>, Arc<std::sync::Mutex<Option<bool>>>);
    impl Drop for Writer {
        fn drop(&mut self) {
            *self.1.lock().expect("probe") = Some(superseding(&self.0));
        }
    }
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let (predecessor, successor, _, action_id) = offered_pair(&registry);
    let actor = ReceiverStartActor(predecessor.clone());
    let handoff = actor
        .commit_successor(&state, &registry, &action_id)
        .expect("committed");
    assert!(!successor.awaiting_commit(), "committed at once");
    assert!(!superseding(&predecessor));
    let seen = Arc::new(std::sync::Mutex::new(None));
    let custody = CommitAnswerWriter::custody(
        Arc::new(Writer(predecessor.clone(), seen.clone())),
        Some(handoff),
    );
    drop(custody);
    assert_eq!(
        *seen.lock().expect("probe"),
        Some(false),
        "the answer's writer is released before the predecessor retires"
    );
    assert!(
        superseding(&predecessor),
        "and then the predecessor retires"
    );
    assert!(!superseding(&successor));
    assert!(retired_within(&actor, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn sharing_receiver_cancelled_change_never_restages_the_sessions_own_ask() {
    let registry = ReceiverStartRegistry::default();
    let state = Arc::new(crate::http::source_actor_test_state());
    let predecessor = registered(&registry, "predecessor", "player", 1);
    publish_hls(&predecessor);
    let actor = ReceiverStartActor(predecessor.clone());
    let at = |sequence, ask: ClientSelection| request(&generation(&predecessor), sequence, ask);
    assert_eq!(actor.observe_ask(&state, &at(1, selection()), true), None);
    let changed = manual(480).desired().digest();
    assert_eq!(
        actor.observe_ask(&state, &at(2, manual(480)), true),
        Some(changed.clone())
    );
    let deadline = clock_ms() + SUCCESSOR_DEADLINE_MS;
    let successor = successor_of(&registry, &predecessor, &manual(480), deadline);
    assert!(actor.install_successor(
        &ReceiverStartActor(successor.clone()),
        changed.clone(),
        deadline
    ));
    // Cancelled: the client's ask returns to what this session delivers. The
    // staged successor is withdrawn, and nothing is staged for the session's
    // own rendition, before or after the withdrawn one has retired.
    assert_eq!(actor.observe_ask(&state, &at(3, selection()), true), None);
    assert!(superseding(&successor));
    assert!(retired_within(&ReceiverStartActor(successor), Duration::from_secs(5)).await);
    for sequence in 4..7 {
        assert_eq!(
            actor.observe_ask(&state, &at(sequence, selection()), true),
            None,
            "never a successor for the session's own ask"
        );
    }
    // A renewed change stages again.
    assert_eq!(
        actor.observe_ask(&state, &at(7, manual(480)), true),
        Some(changed.clone())
    );
    // The same after an aborted offer.
    let registry = ReceiverStartRegistry::default();
    let (predecessor, successor, _, action_id) = offered_pair(&registry);
    let actor = ReceiverStartActor(predecessor.clone());
    let at = |sequence, ask: ClientSelection| request(&generation(&predecessor), sequence, ask);
    // The session delivers the created ask; its offered successor carries 480.
    predecessor.state.lock().expect("owner").handoff.own_digest =
        Some(selection().desired().digest());
    assert!(actor.abort_successor(&state, &action_id));
    assert!(retired_within(&ReceiverStartActor(successor), Duration::from_secs(5)).await);
    for sequence in 1..4 {
        assert_eq!(
            actor.observe_ask(&state, &at(sequence, selection()), true),
            None,
            "an aborted change never restages the session's own ask"
        );
    }
}
