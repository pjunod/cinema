// The processed upgrade of a session another node owns (protocol 12): the
// ingress decides and hands the verdict over in the start envelope, and the
// owner registers it from the request it restored, through the same intent
// builder a local create uses.

use crate::playback_control::ProcessedUpgradeRegistration as Outcome;

fn handoff_ask(auto_policy: bool) -> crate::media_sessions::ProcessedUpgradeAsk {
    crate::media_sessions::ProcessedUpgradeAsk {
        hdr10_intent: true,
        auto_policy,
        admitted: None,
        link_unconsulted: false,
    }
}

fn restored_request(playback: &str) -> crate::transcode::SessionRequest {
    let mut request = super::processed_upgrade::tests::ordinary_request(playback);
    // `hdr10` on the request is the delivered grade, not the client's ask:
    // the intent must take the ask's.
    request.hdr10 = false;
    request.automatic = false;
    request.candidate_context = Some(Box::new(
        crate::transcode::continuous_test_candidate_context(
            plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x41; 32]),
            [0x41; 32],
            2160,
        ),
    ));
    request
}

struct Ids {
    playback: String,
    session: String,
    incarnation: String,
}

fn ids() -> Ids {
    Ids {
        playback: uuid::Uuid::new_v4().to_string(),
        session: uuid::Uuid::new_v4().to_string(),
        incarnation: uuid::Uuid::new_v4().to_string(),
    }
}

fn owned<'a>(
    ids: &'a Ids,
    request: &'a crate::transcode::SessionRequest,
) -> super::processed_upgrade::OwnedSession<'a> {
    super::processed_upgrade::OwnedSession {
        user_id: 71,
        playback_id: &ids.playback,
        session_id: &ids.session,
        incarnation_id: &ids.incarnation,
        request,
    }
}

/// The start envelope an ingress sends this owner, as its start handler holds
/// it after restoring the candidate context.
fn handed_over_envelope(
    ids: &Ids,
    ask: Option<crate::media_sessions::ProcessedUpgradeAsk>,
) -> RemoteStartRequest {
    let mut request = restored_request(&ids.playback);
    request.request_id = Some(ids.incarnation.clone());
    RemoteStartRequest {
        retained_output_receiver: None,
        retained_output: None,
        candidate_catalog: None,
        candidate_id: None,
        presentation_target: None,
        decoder_caps: None,
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: ids.incarnation.clone(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 71 },
        source_size: 1,
        source_mtime: 1,
        typeless_playlist: true,
        library_channel: None,
        processed_upgrade: ask,
        request,
    }
}

/// Test plan 4: the owner of a handed-over start registers a waiting upgrade
/// carrying the restored candidate context, the ask's HDR10 intent and Auto
/// policy (not the request's), and no viewer evidence.
#[test]
fn a_handed_over_start_registers_from_its_restored_request_without_viewer_evidence() {
    use super::processed_upgrade::{IntentOrigin, Registration, RegistrationSource};
    let ids = ids();
    let request = restored_request(&ids.playback);
    let mut ask = handoff_ask(true);
    ask.admitted = Some(crate::media_sessions::AdmittedProcessedRecipe {
        recipe_digest: [0x42; 32],
        average_bps: Some(20_000_000),
        peak_bps: None,
    });
    let before =
        crate::playback_control::processed_upgrade_registration_count(true, Outcome::Registered);
    assert_eq!(
        super::processed_upgrade::register_owned(
            owned(&ids, &request),
            &ask,
            RegistrationSource::HandedOver
        ),
        Registration::Waiting
    );
    let intent = super::processed_upgrade::waiting_intent(&ids.session).expect("waiting intent");
    assert_eq!(intent.incarnation_id, ids.incarnation);
    assert_eq!(intent.playback_id, ids.playback);
    assert!(
        intent.hdr10_intent && intent.auto_policy,
        "the ask's facts, not the request's"
    );
    assert!(
        intent.request.candidate_context.is_some(),
        "the restored context"
    );
    assert!(intent.network.is_none() && intent.network_prior.is_none());
    assert!(intent.incumbent_receipt.is_none());
    assert_eq!(
        intent.origin,
        IntentOrigin::HandedOver {
            admitted: ask.admitted.clone(),
            link_unconsulted: false,
        }
    );
    assert!(
        crate::playback_control::processed_upgrade_registration_count(true, Outcome::Registered)
            > before
    );
    super::processed_upgrade::cancel_for_session(&ids.session);
}

/// Test plans 4 and 5 at the owner's start handler seam: a created start that
/// carried an ask registers from the envelope the handler restored; a start
/// without an ask registers nothing; and a replayed start leaves the upgrade
/// its creation registered, here already running, alone.
#[test]
fn the_owner_start_registers_a_created_handed_over_start_and_never_a_replay() {
    use super::processed_upgrade::{register_handed_over_start, Registration};
    let ids = ids();
    assert_eq!(
        register_handed_over_start(true, &handed_over_envelope(&ids, None), 71, &ids.session),
        None
    );
    assert!(!super::processed_upgrade::is_waiting(&ids.session));
    let envelope = handed_over_envelope(&ids, Some(handoff_ask(false)));
    assert_eq!(
        register_handed_over_start(true, &envelope, 71, &ids.session),
        Some(Registration::Waiting)
    );
    let intent = super::processed_upgrade::waiting_intent(&ids.session).expect("registered");
    assert_eq!(intent.request.playback_id, ids.playback);
    assert!(intent.request.candidate_context.is_some());
    let (_intent, run) =
        super::processed_upgrade::take_on_rendering(71, &ids.session, &ids.incarnation)
            .expect("its first rendering exchange");
    assert_eq!(
        register_handed_over_start(false, &envelope, 71, &ids.session),
        None,
        "a replay registers nothing"
    );
    assert!(
        super::processed_upgrade::is_running(&ids.session),
        "the running upgrade is untouched"
    );
    drop(run);
    super::processed_upgrade::cancel_for_session(&ids.session);
}

/// Test plan 12: a local registration keeps today's intent, with this node's
/// viewer evidence and no handed-over admission.
#[test]
fn a_local_registration_keeps_the_viewers_evidence() {
    use super::processed_upgrade::{
        IntentOrigin, Registration, RegistrationSource, ViewerEvidence,
    };
    let ids = ids();
    let mut request = restored_request(&ids.playback);
    request.candidate_context = None;
    let evidence = ViewerEvidence {
        incumbent_receipt: Some("receipt".to_owned()),
        ..ViewerEvidence::default()
    };
    assert_eq!(
        super::processed_upgrade::register_owned(
            owned(&ids, &request),
            &handoff_ask(true),
            RegistrationSource::Local(Box::new(evidence))
        ),
        Registration::Waiting,
        "a local create registers even without a candidate context, as before"
    );
    let intent = super::processed_upgrade::waiting_intent(&ids.session).expect("waiting intent");
    assert_eq!(intent.origin, IntentOrigin::Local);
    assert_eq!(intent.incumbent_receipt.as_deref(), Some("receipt"));
    super::processed_upgrade::cancel_for_session(&ids.session);
}

/// The owner restored no candidate context: the trial would answer "not
/// applicable" in silence, so the registration names it instead, for this
/// generation only.
#[test]
fn a_handed_over_start_without_its_context_records_a_named_skip() {
    use super::processed_upgrade::{Registration, RegistrationSource, HANDED_OVER_WITHOUT_CONTEXT};
    let ids = ids();
    let mut request = restored_request(&ids.playback);
    request.candidate_context = None;
    let before =
        crate::playback_control::processed_upgrade_registration_count(true, Outcome::Skipped);
    assert_eq!(
        super::processed_upgrade::register_owned(
            owned(&ids, &request),
            &handoff_ask(false),
            RegistrationSource::HandedOver
        ),
        Registration::Skipped(HANDED_OVER_WITHOUT_CONTEXT)
    );
    assert!(!super::processed_upgrade::is_waiting(&ids.session));
    let skip = super::processed_upgrade::processing_skip(&ids.session, &ids.incarnation)
        .expect("the named skip for this generation");
    assert_eq!(skip.reason, HANDED_OVER_WITHOUT_CONTEXT);
    assert!(
        super::processed_upgrade::processing_skip(&ids.session, "another-generation").is_none()
    );
    assert!(
        crate::playback_control::processed_upgrade_registration_count(true, Outcome::Skipped)
            > before
    );
    super::processed_upgrade::cancel_for_session(&ids.session);
}

/// Review finding 1: Auto handed over with no link evidence at the ingress
/// (no network identity, or network priors off, the default) registers like
/// any other start, and its trial's recipe is admitted.
#[test]
fn auto_handed_over_without_link_evidence_registers_and_admits_its_trial() {
    use super::processed_upgrade::{
        handed_over_auto_admission, IntentOrigin, Registration, RegistrationSource,
    };
    let ids = ids();
    let request = restored_request(&ids.playback);
    let mut ask = handoff_ask(true);
    ask.link_unconsulted = true;
    assert_eq!(
        super::processed_upgrade::register_owned(
            owned(&ids, &request),
            &ask,
            RegistrationSource::HandedOver
        ),
        Registration::Waiting
    );
    let intent = super::processed_upgrade::waiting_intent(&ids.session).expect("registered");
    assert_eq!(
        intent.origin,
        IntentOrigin::HandedOver {
            admitted: None,
            link_unconsulted: true
        }
    );
    let frozen = crate::transcode::continuous_test_candidate_context(
        plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x49; 32]),
        [0x49; 32],
        2160,
    )
    .selected_candidate;
    assert_eq!(handed_over_auto_admission(None, true, &frozen), Ok(()));
    super::processed_upgrade::cancel_for_session(&ids.session);
}

/// Decision 2 with link evidence to consult: Auto on a remote owner stages
/// only what the ingress admitted. With nothing admitted there is no trial to
/// spend: the owner records the named skip, and the relay carries it to the
/// client intact.
#[test]
fn auto_handed_over_without_an_admitted_recipe_is_skipped_by_name_and_relayed() {
    use super::processed_upgrade::{
        ProcessingSkip, Registration, RegistrationSource, HANDED_OVER_AUTO_UNADMITTED,
    };
    let ids = ids();
    let request = restored_request(&ids.playback);
    assert_eq!(
        super::processed_upgrade::register_owned(
            owned(&ids, &request),
            &handoff_ask(true),
            RegistrationSource::HandedOver
        ),
        Registration::Skipped(HANDED_OVER_AUTO_UNADMITTED)
    );
    let skip = super::processed_upgrade::processing_skip(&ids.session, &ids.incarnation)
        .expect("the named skip");
    let header = skip.owner_header().expect("the relay header");
    let relayed = ProcessingSkip::from_owner_wire(header.as_bytes(), &ids.incarnation)
        .expect("the ingress reads the owner's skip");
    assert_eq!(relayed.reason, HANDED_OVER_AUTO_UNADMITTED);
    super::processed_upgrade::cancel_for_session(&ids.session);
}

/// Test plan 7: the relayed rendering exchange of the session's own
/// generation takes a handed-over upgrade, once; a stale generation does not.
#[test]
fn a_handed_over_upgrade_is_taken_once_by_its_own_generations_rendering() {
    let ids = ids();
    let request = restored_request(&ids.playback);
    super::processed_upgrade::register_owned(
        owned(&ids, &request),
        &handoff_ask(false),
        super::processed_upgrade::RegistrationSource::HandedOver,
    );
    assert!(super::processed_upgrade::take_on_rendering(
        71,
        &ids.session,
        &uuid::Uuid::new_v4().to_string()
    )
    .is_none());
    let (intent, run) =
        super::processed_upgrade::take_on_rendering(71, &ids.session, &ids.incarnation)
            .expect("its own generation's rendering");
    assert!(matches!(
        intent.origin,
        super::processed_upgrade::IntentOrigin::HandedOver { .. }
    ));
    assert!(super::processed_upgrade::is_running(&ids.session));
    assert!(
        super::processed_upgrade::take_on_rendering(71, &ids.session, &ids.incarnation).is_none()
    );
    drop(run);
    super::processed_upgrade::cancel_for_session(&ids.session);
}

/// The relayed control exchange, through `control_authorized` as a peer
/// sends it: an accepted rendering exchange of the session's own generation
/// starts the waiting upgrade on this owner, and a named skip recorded for
/// that generation rides back on the relay as `x-plurx-processing-skip`.
#[tokio::test]
async fn the_relayed_control_exchange_starts_the_upgrade_and_returns_the_skip() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("handoff-relayed-control");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let user_id = route.principal.local_user_id().expect("a local viewer");
    let mut request = restored_request(&playback);
    // No context: the run this exchange starts ends at once, with no trial.
    request.candidate_context = None;
    super::processed_upgrade::register(super::processed_upgrade::UpgradeIntent {
        user_id,
        playback_id: route.playback_id.clone(),
        session_id: session_id.clone(),
        incarnation_id: route.incarnation_id.clone(),
        request,
        hdr10_intent: false,
        auto_policy: false,
        network: None,
        network_prior: None,
        incumbent_receipt: None,
        origin: super::processed_upgrade::IntentOrigin::HandedOver {
            admitted: None,
            link_unconsulted: false,
        },
    });
    let scheduled = crate::playback_control::processed_upgrade_count("scheduled");
    // One client instance across both exchanges, so the second is the next
    // in its sequence rather than a stale fence.
    let base = control_request(route.incarnation_id.clone());
    let relay = |sequence: u64| {
        let mut control = base.clone();
        control.sequence = sequence;
        control.position_ms = 1_000 + 1_000 * sequence as i64;
        crate::playback_control::ControlRelayRequest {
            session_id: session_id.clone(),
            generation: route.incarnation_id.clone(),
            expected_owner_node_id: fixture.state.node_id.clone(),
            expected_owner_epoch: 1,
            deadline_unix_ms: unix_ms().saturating_add(4_000),
            control,
        }
    };
    let response =
        crate::http::internal_media_sessions::control_authorized(fixture.state.clone(), relay(1))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !super::processed_upgrade::is_waiting(&session_id),
        "the exchange took it"
    );
    assert!(crate::playback_control::processed_upgrade_count("scheduled") > scheduled);

    // A named skip for this generation rides back on the relay, on the
    // next exchange at the real control cadence.
    super::processed_upgrade::cancel_for_session(&session_id);
    tokio::time::sleep(std::time::Duration::from_millis(260)).await;
    let mut skipped = restored_request(&playback);
    skipped.candidate_context = None;
    super::processed_upgrade::register_owned(
        super::processed_upgrade::OwnedSession {
            user_id,
            playback_id: &route.playback_id,
            session_id: &session_id,
            incarnation_id: &route.incarnation_id,
            request: &skipped,
        },
        &handoff_ask(false),
        super::processed_upgrade::RegistrationSource::HandedOver,
    );
    let response =
        crate::http::internal_media_sessions::control_authorized(fixture.state.clone(), relay(2))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let header = response
        .headers()
        .get("x-plurx-processing-skip")
        .expect("the owner's skip on the relay")
        .to_str()
        .expect("ASCII")
        .to_owned();
    let report = super::processed_upgrade::ProcessingSkip::from_owner_wire(
        header.as_bytes(),
        &route.incarnation_id,
    )
    .expect("the ingress accepts it");
    assert_eq!(
        report.reason,
        super::processed_upgrade::HANDED_OVER_WITHOUT_CONTEXT
    );
    super::processed_upgrade::cancel_for_session(&session_id);
}

/// Review finding 2: a release that reaches the owner over the peer route
/// (`settle_remote_abort`) cancels a running upgrade there, freeing the
/// owner's processed slot, as `release_session` does on the node it reached.
#[tokio::test]
async fn a_peer_abort_cancels_the_owners_running_upgrade() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("handoff-peer-abort");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let user_id = route.principal.local_user_id().expect("a local viewer");
    let request = restored_request(&playback);
    super::processed_upgrade::register_owned(
        super::processed_upgrade::OwnedSession {
            user_id,
            playback_id: &route.playback_id,
            session_id: &session_id,
            incarnation_id: &route.incarnation_id,
            request: &request,
        },
        &handoff_ask(false),
        super::processed_upgrade::RegistrationSource::HandedOver,
    );
    let (_intent, run) =
        super::processed_upgrade::take_on_rendering(user_id, &session_id, &route.incarnation_id)
            .expect("running");
    assert!(!run.cancelled().is_cancelled());
    let _ = crate::http::internal_media_sessions::settle_remote_abort(
        fixture.state.clone(),
        crate::media_sessions::RemoteAbortRequest {
            incarnation_id: route.incarnation_id.clone(),
            session_id: session_id.clone(),
            expected_owner_epoch: 1,
            reason: None,
        },
    )
    .await;
    assert!(
        run.cancelled().is_cancelled(),
        "the trial's token is cancelled"
    );
    assert!(!super::processed_upgrade::is_running(&session_id));
}

/// Test plan 11 and review finding 9: the owner of a successor another node
/// staged here registers it through the same builder, after the prime, only
/// while the successor's route is still this node's; and the successor's own
/// rendering exchange then takes it.
#[tokio::test]
async fn the_owner_registers_a_remote_successor_only_while_its_route_is_current() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("handoff-remote-successor");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let user_id = route.principal.local_user_id().expect("a local viewer");
    let mut recipe: RemoteStartRequest =
        serde_json::from_str(&route.recipe_json).expect("the successor's recipe");
    recipe.processed_upgrade = Some(crate::media_sessions::ProcessedUpgradeAsk {
        link_unconsulted: true,
        ..handoff_ask(true)
    });
    recipe.request.candidate_context = restored_request(&playback).candidate_context;
    assert_eq!(
        super::processed_upgrade::register_handed_over_successor(
            &fixture.state,
            recipe.clone(),
            &uuid::Uuid::new_v4().to_string()
        )
        .await,
        None,
        "a route that is no longer this successor's registers nothing"
    );
    let mut without = recipe.clone();
    without.processed_upgrade = None;
    assert_eq!(
        super::processed_upgrade::register_handed_over_successor(
            &fixture.state,
            without,
            &session_id
        )
        .await,
        None
    );
    assert_eq!(
        super::processed_upgrade::register_handed_over_successor(
            &fixture.state,
            recipe,
            &session_id
        )
        .await,
        Some(super::processed_upgrade::Registration::Waiting)
    );
    let (intent, run) =
        super::processed_upgrade::take_on_rendering(user_id, &session_id, &route.incarnation_id)
            .expect("the successor's own rendering exchange");
    assert!(intent.auto_policy);
    drop(run);
    super::processed_upgrade::cancel_for_session(&session_id);
}

/// Test plan 6: a start that never activates leaves no entry once the
/// owner's confirmation watcher cancels what the start registered.
#[test]
fn an_unactivated_handed_over_start_leaves_no_entry() {
    let ids = ids();
    let request = restored_request(&ids.playback);
    super::processed_upgrade::register_owned(
        owned(&ids, &request),
        &handoff_ask(false),
        super::processed_upgrade::RegistrationSource::HandedOver,
    );
    assert!(super::processed_upgrade::is_waiting(&ids.session));
    super::processed_upgrade::cancel_for_session(&ids.session);
    assert!(!super::processed_upgrade::is_waiting(&ids.session));
}

/// Test plan 8: the ingress of a remote-owned session holds no entry, so its
/// `ttff` beacon is a measurement only.
#[tokio::test]
async fn a_ttff_on_the_ingress_of_a_remote_owned_session_is_a_no_op() {
    let state = crate::http::tests::test_state_without_router();
    let session = uuid::Uuid::new_v4().to_string();
    super::processed_upgrade::note_first_frame(&state, 71, &session);
    assert!(!super::processed_upgrade::is_running(&session));
    assert!(!super::processed_upgrade::is_waiting(&session));
}

/// Test plan 9, the decision itself: no link evidence admits; otherwise
/// exactly the admitted recipe, or a named refusal.
#[test]
fn a_handed_over_auto_upgrade_admits_only_the_ingress_admitted_recipe() {
    use super::processed_upgrade::{
        handed_over_auto_admission, HANDED_OVER_AUTO_RECIPE_CHANGED, HANDED_OVER_AUTO_UNADMITTED,
    };
    let candidate = crate::transcode::continuous_test_candidate_context(
        plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x43; 32]),
        [0x43; 32],
        2160,
    )
    .selected_candidate;
    let admitted = crate::media_sessions::AdmittedProcessedRecipe::of(&candidate);
    assert_eq!(
        handed_over_auto_admission(Some(&admitted), false, &candidate),
        Ok(())
    );
    let mut froze = candidate.clone();
    froze.recipe_digest = [0x44; 32];
    assert_eq!(
        handed_over_auto_admission(Some(&admitted), false, &froze),
        Err(HANDED_OVER_AUTO_RECIPE_CHANGED.to_owned())
    );
    assert_eq!(
        handed_over_auto_admission(None, false, &candidate),
        Err(HANDED_OVER_AUTO_UNADMITTED.to_owned())
    );
    assert_eq!(handed_over_auto_admission(None, true, &froze), Ok(()));
}

/// A processed description of `height` on `node`, in a catalog.
fn described_catalog(
    node: &str,
    digest: [u8; 32],
    height: u32,
) -> (
    plurx_core::playback::candidate::QualityCandidate,
    std::sync::Arc<crate::media_pool::QualityCatalogResult>,
) {
    let mut described = crate::transcode::continuous_test_candidate_context(
        plurx_core::playback::candidate::CandidateId::for_recipe_digest(digest),
        digest,
        height,
    )
    .selected_candidate;
    described.processed_dv = true;
    described.planned_codec = Some("hevc".into());
    (
        described.clone(),
        std::sync::Arc::new(crate::media_pool::QualityCatalogResult {
            candidates: vec![crate::media_pool::WorkerQualityCandidate {
                node_id: node.to_owned(),
                candidate: described,
                binding: None,
                partial: false,
                dispatch_supported: true,
            }],
            authority_refused: false,
            complete: true,
            causes: Vec::new(),
        }),
    )
}

fn route_file_id(route: &MediaSessionRoute) -> i64 {
    serde_json::from_str::<RemoteStartRequest>(&route.recipe_json)
        .expect("the route's recipe")
        .request
        .file_id
}

/// A probed 4K source for the fixture's route, so the trial height is known.
async fn probe_4k(
    fixture: &HlsDeliveryFixture,
    route: &MediaSessionRoute,
) -> plurx_core::domain::MediaFile {
    let unprobed = fixture
        .store
        .get_file(route_file_id(route))
        .await
        .expect("file read")
        .expect("source");
    fixture
        .store
        .upsert_file(
            unprobed.item_id,
            unprobed.path.to_str().expect("fixture source path"),
            unprobed.size,
            unprobed.mtime,
            &plurx_core::domain::ProbeResult {
                duration_ms: unprobed.duration_ms,
                width: Some(3840),
                height: Some(2160),
                ..plurx_core::domain::ProbeResult::default()
            },
        )
        .await
        .expect("probe the fixture's source");
    fixture
        .store
        .get_file(route_file_id(route))
        .await
        .expect("file read")
        .expect("source")
}

/// Test plan 9 after the trial, where production decides it on a remote
/// owner: the admission of the recipe the trial froze. The admitted recipe
/// is admitted, another one is refused by name, and with no link evidence at
/// the ingress any frozen recipe is admitted.
#[tokio::test]
async fn a_handed_over_auto_admission_after_the_trial_admits_only_the_admitted_recipe() {
    use super::processed_upgrade::{admit_processed_candidate, AdmissionStage, IntentOrigin};
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("handoff-auto-after-trial");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let source = probe_4k(&fixture, &route).await;
    let (frozen, _) = described_catalog(&fixture.state.node_id, [0x4a; 32], 2160);
    let admitted = crate::media_sessions::AdmittedProcessedRecipe::of(&frozen);
    let mut other = frozen.clone();
    other.recipe_digest = [0x4b; 32];
    let intent = |link_unconsulted: bool| super::processed_upgrade::UpgradeIntent {
        user_id: route.principal.local_user_id().expect("viewer"),
        playback_id: route.playback_id.clone(),
        session_id: session_id.clone(),
        incarnation_id: route.incarnation_id.clone(),
        request: restored_request(&playback),
        hdr10_intent: true,
        auto_policy: true,
        network: None,
        network_prior: None,
        incumbent_receipt: None,
        origin: IntentOrigin::HandedOver {
            admitted: (!link_unconsulted).then(|| admitted.clone()),
            link_unconsulted,
        },
    };
    let consulted = intent(false);
    assert!(
        admit_processed_candidate(
            &fixture.state,
            &consulted,
            &source,
            &route,
            &frozen,
            AdmissionStage::AfterTrial
        )
        .await
        .is_ok(),
        "the admitted recipe stages"
    );
    assert_eq!(
        admit_processed_candidate(
            &fixture.state,
            &consulted,
            &source,
            &route,
            &other,
            AdmissionStage::AfterTrial
        )
        .await,
        Err(super::processed_upgrade::HANDED_OVER_AUTO_RECIPE_CHANGED.to_owned())
    );
    assert!(
        admit_processed_candidate(
            &fixture.state,
            &intent(true),
            &source,
            &route,
            &other,
            AdmissionStage::AfterTrial
        )
        .await
        .is_ok(),
        "no link evidence admits the trial's recipe"
    );
}

/// Test plan 9 before the trial, where the owner's catalog describes the
/// recipe (finding 10): a handed-over Auto upgrade whose described recipe is
/// not the admitted one is refused by name before any GPU is spent, and the
/// admitted recipe reaches its trial.
#[tokio::test]
async fn a_handed_over_auto_run_is_refused_before_its_trial_unless_its_recipe_was_admitted() {
    let (described, _) = described_catalog("unused", [0x45; 32], 2160);
    for (admitted, expected_reason) in [
        (
            crate::media_sessions::AdmittedProcessedRecipe {
                recipe_digest: [0x46; 32],
                average_bps: described.average_bps,
                peak_bps: described.peak_bps,
            },
            Some(super::processed_upgrade::HANDED_OVER_AUTO_RECIPE_CHANGED),
        ),
        (
            crate::media_sessions::AdmittedProcessedRecipe::of(&described),
            None,
        ),
    ] {
        let dir = crate::test_tempdir().expect("state dir");
        let playback = unique_playback_id("handoff-auto-admission");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback).await;
        probe_4k(&fixture, &route).await;
        // The owner's own description of the recipe, as the catalog it reads
        // for a handed-over intent holds it.
        let (_, catalog) = described_catalog(&fixture.state.node_id, [0x45; 32], 2160);
        let (mut intent, run, _) =
            upgrade_after_playing_to(&fixture, &session_id, &route, 60_000).await;
        intent.auto_policy = true;
        intent.origin = super::processed_upgrade::IntentOrigin::HandedOver {
            admitted: Some(admitted),
            link_unconsulted: false,
        };
        intent.request.candidate_context = Some(Box::new(
            crate::transcode::continuous_test_candidate_context(
                plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x47; 32]),
                [0x47; 32],
                2160,
            ),
        ));
        intent.request.quality_catalog = Some(catalog);
        let reached = std::sync::atomic::AtomicBool::new(false);
        let outcome = super::processed_upgrade::run_upgrade_with(
            &fixture.state,
            &intent,
            &run,
            |_request, _recovery| {
                reached.store(true, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(Err("test trial".to_owned()))
            },
        )
        .await;
        match expected_reason {
            Some(reason) => {
                assert_eq!(outcome, "admission_refused");
                assert!(
                    !reached.load(std::sync::atomic::Ordering::SeqCst),
                    "no GPU spent"
                );
                let skip =
                    super::processed_upgrade::processing_skip(&session_id, &route.incarnation_id)
                        .expect("the named skip the relayed exchange returns");
                assert_eq!(skip.reason, reason);
            }
            None => {
                assert_eq!(outcome, "declined", "the trial decides the admitted recipe");
                assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
            }
        }
        drop(run);
        super::processed_upgrade::cancel_for_session(&session_id);
    }
}

/// Review finding 3: a handed-over intent arrives with no quality catalog (it
/// is never serialized). Its run reads the catalog for the restored query on
/// the owner, so the trial plans, and the staged successor's response lists
/// rows, with it.
#[tokio::test]
async fn a_handed_over_run_reads_the_owners_catalog_before_its_trial() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("handoff-owner-catalog");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let source = probe_4k(&fixture, &route).await;
    let (mut intent, run, _) =
        upgrade_after_playing_to(&fixture, &session_id, &route, 60_000).await;
    intent.origin = super::processed_upgrade::IntentOrigin::HandedOver {
        admitted: None,
        link_unconsulted: true,
    };
    intent.request.quality_catalog = None;
    let mut context = crate::transcode::continuous_test_candidate_context(
        plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x4c; 32]),
        [0x4c; 32],
        2160,
    );
    context.catalog_query = Some(crate::media_pool::QualityCatalogRequest {
        audio_claim: None,
        audio_delivery: None,
        copy_contract: None,
        file_id: source.id,
        source_size: source.size,
        source_mtime: source.mtime,
        caps: plurx_core::playback::DeviceCaps {
            v: 2,
            ..Default::default()
        },
        audio_index: None,
        audio_offset_ms: 0,
        subtitle_burn: None,
        presentation: crate::transcode::Presentation::Vod,
    });
    intent.request.candidate_context = Some(Box::new(context));
    assert!(
        super::processed_upgrade::handed_over_catalog(&fixture.state, &intent)
            .await
            .is_some_and(|with| with.request.quality_catalog.is_some()),
        "the owner reads the catalog for the restored query"
    );
    let planned_with_catalog = std::sync::atomic::AtomicBool::new(false);
    let outcome = super::processed_upgrade::run_upgrade_with(
        &fixture.state,
        &intent,
        &run,
        |request, _recovery| {
            planned_with_catalog.store(
                request.quality_catalog.is_some(),
                std::sync::atomic::Ordering::SeqCst,
            );
            std::future::ready(Err("test trial".to_owned()))
        },
    )
    .await;
    assert_eq!(outcome, "declined");
    assert!(planned_with_catalog.load(std::sync::atomic::Ordering::SeqCst));
    // A local intent keeps the catalog its create held (or none).
    let mut local = intent.clone();
    local.origin = super::processed_upgrade::IntentOrigin::Local;
    assert!(
        super::processed_upgrade::handed_over_catalog(&fixture.state, &local)
            .await
            .is_none()
    );
    drop(run);
    super::processed_upgrade::cancel_for_session(&session_id);
}

/// Test plan 2: the ingress hands an applicable upgrade to another owner,
/// and never to itself. A manual-quality ask travels as it is. Auto with no
/// link evidence to consult (no network identity, or priors off) says so and
/// reads nothing; Auto with evidence carries the owner's described processed
/// row once this viewer's link admits it, and only that owner's.
#[tokio::test]
async fn the_ingress_hands_over_its_verdict_and_admits_auto_for_the_chosen_owner() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("handoff-ingress");
    let (fixture, _session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let state = fixture.state.clone();
    let source = probe_4k(&fixture, &route).await;
    let mut request = restored_request(&playback);
    let (described, catalog) = described_catalog("media2", [0x48; 32], 2160);
    request.quality_catalog = Some(catalog);
    let no_identity = super::processed_upgrade::ViewerEvidence::default();
    let manual = handoff_ask(false);
    let auto = handoff_ask(true);

    assert_eq!(
        super::handed_over_ask(
            &state,
            None,
            true,
            &no_identity,
            &request,
            Some(&source),
            "media2"
        )
        .await,
        None,
        "no verdict, no ask"
    );
    assert_eq!(
        super::handed_over_ask(
            &state,
            Some(&manual),
            true,
            &no_identity,
            &request,
            Some(&source),
            &state.node_id
        )
        .await,
        None,
        "this node registers its own"
    );
    assert_eq!(
        super::handed_over_ask(
            &state,
            Some(&manual),
            true,
            &no_identity,
            &request,
            Some(&source),
            "media2"
        )
        .await,
        Some(manual.clone())
    );
    assert_eq!(
        super::handed_over_ask(
            &state,
            Some(&manual),
            false,
            &no_identity,
            &request,
            Some(&source),
            "media2"
        )
        .await,
        None,
        "with the control protocol off the owner has no proof of the first frame: nothing travels"
    );
    let unconsulted = super::handed_over_ask(
        &state,
        Some(&auto),
        true,
        &no_identity,
        &request,
        Some(&source),
        "media3",
    )
    .await
    .expect("the ask travels");
    assert!(unconsulted.link_unconsulted && unconsulted.admitted.is_none());

    // A viewer with a network identity, and network priors on: the link
    // memory is consulted, for the chosen owner's described row only.
    fixture
        .store
        .put_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS, "1")
        .await
        .expect("priors on");
    let identified = super::processed_upgrade::ViewerEvidence {
        network: Some(crate::telemetry::NetworkIdentity {
            client_class: "web".to_owned(),
            network_fingerprint: "handoff".to_owned(),
            credential_generation: Some(plurx_core::domain::CredentialGeneration::from(
                "handoff-gen".to_owned(),
            )),
            user_id: Some(i64::MAX),
        }),
        ..Default::default()
    };
    let sent = super::handed_over_ask(
        &state,
        Some(&auto),
        true,
        &identified,
        &request,
        Some(&source),
        "media2",
    )
    .await
    .expect("the ask travels");
    assert!(!sent.link_unconsulted);
    assert_eq!(
        sent.admitted,
        Some(crate::media_sessions::AdmittedProcessedRecipe::of(
            &described
        ))
    );
    let elsewhere = super::handed_over_ask(
        &state,
        Some(&auto),
        true,
        &identified,
        &request,
        Some(&source),
        "media3",
    )
    .await
    .expect("the ask still travels, so the owner names its skip");
    assert_eq!(
        elsewhere.admitted, None,
        "another node's row admits nothing for this owner"
    );
    assert!(!elsewhere.link_unconsulted);

    // What the start says about it, on the ingress.
    assert_eq!(super::handed_over_note(None, "media2", "s"), None);
    assert_eq!(
        super::handed_over_note(Some(&sent), "media2", "s").as_deref(),
        Some(super::HANDED_OVER_NOTE)
    );
    assert_eq!(
        super::handed_over_note(Some(&unconsulted), "media3", "s").as_deref(),
        Some(super::HANDED_OVER_NOTE)
    );
    assert_eq!(
        super::handed_over_note(Some(&elsewhere), "media3", "s"),
        Some(super::processing_declined_note(
            super::processed_upgrade::HANDED_OVER_AUTO_UNADMITTED
        ))
    );
}

/// Review finding 8: a full registry is said and counted, and a skip that
/// could not be recorded is not counted as one.
#[test]
fn a_full_registry_is_counted_not_silent() {
    use super::processed_upgrade::Registration;
    let before = crate::playback_control::processed_upgrade_registration_count(true, Outcome::Full);
    let mut filled = Vec::new();
    let full = loop {
        let ids = ids();
        let request = restored_request(&ids.playback);
        let outcome = super::processed_upgrade::register_owned(
            owned(&ids, &request),
            &handoff_ask(false),
            super::processed_upgrade::RegistrationSource::HandedOver,
        );
        filled.push(ids.session.clone());
        if outcome == Registration::Full || filled.len() > 2_000 {
            break outcome;
        }
    };
    assert_eq!(full, Registration::Full);
    let ids = ids();
    let mut request = restored_request(&ids.playback);
    request.candidate_context = None;
    assert_eq!(
        super::processed_upgrade::register_owned(
            owned(&ids, &request),
            &handoff_ask(false),
            super::processed_upgrade::RegistrationSource::HandedOver,
        ),
        Registration::Full,
        "a skip the full registry could not record is reported as full"
    );
    assert!(
        crate::playback_control::processed_upgrade_registration_count(true, Outcome::Full)
            >= before + 2
    );
    for session in filled {
        super::processed_upgrade::cancel_for_session(&session);
    }
}
