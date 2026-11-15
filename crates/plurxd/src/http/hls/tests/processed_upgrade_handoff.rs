// The processed upgrade of a session another node owns (protocol 11): the
// ingress decides and hands the verdict over in the start envelope, and the
// owner registers it from the request it restored, through the same intent
// builder a local create uses.

fn handoff_ask(auto_policy: bool) -> crate::media_sessions::ProcessedUpgradeAsk {
    crate::media_sessions::ProcessedUpgradeAsk {
        hdr10_intent: true,
        auto_policy,
        admitted: None,
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
    let before = crate::playback_control::processed_upgrade_registration_count(true, true);
    assert_eq!(
        super::processed_upgrade::register_owned(
            owned(&ids, &request),
            &ask,
            RegistrationSource::HandedOver
        ),
        Registration::Waiting
    );
    assert!(super::processed_upgrade::is_waiting(&ids.session));
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
            admitted: ask.admitted.clone()
        }
    );
    assert!(crate::playback_control::processed_upgrade_registration_count(true, true) > before);
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
    let before = crate::playback_control::processed_upgrade_registration_count(true, false);
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
    assert!(crate::playback_control::processed_upgrade_registration_count(true, false) > before);
    super::processed_upgrade::cancel_for_session(&ids.session);
}

/// Decision 2: Auto on a remote owner stages only what the ingress admitted.
/// With nothing admitted there is no trial to spend: the owner records the
/// named skip, and the relay carries it to the client intact.
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

/// Test plan 6: a start that never activates leaves no entry; the owner's
/// confirmation watcher cancels what the start registered.
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
    let source = include_str!("../../internal_media_sessions.rs");
    let watcher = source
        .split_once("let confirmation_session = response.session_id.clone();")
        .expect("the start's confirmation watcher")
        .1;
    let watcher = &watcher[..watcher
        .find("Ok::<(RemoteStartResponse, bool), String>")
        .expect("the start task's answer")];
    assert!(
        watcher.contains("processed_upgrade::cancel_for_session(&confirmation_session)"),
        "an unconfirmed start cancels its registration where its guard is dropped"
    );
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

/// Test plan 9, the decision itself: exactly the admitted recipe, or a named
/// refusal.
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
        handed_over_auto_admission(Some(&admitted), &candidate),
        Ok(())
    );
    let mut froze = candidate.clone();
    froze.recipe_digest = [0x44; 32];
    assert_eq!(
        handed_over_auto_admission(Some(&admitted), &froze),
        Err(HANDED_OVER_AUTO_RECIPE_CHANGED.to_owned())
    );
    assert_eq!(
        handed_over_auto_admission(None, &candidate),
        Err(HANDED_OVER_AUTO_UNADMITTED.to_owned())
    );
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

/// Test plan 9, at the run's admission seam: a handed-over Auto upgrade whose
/// recipe is not the admitted one is refused by name before any trial, and
/// the admitted recipe reaches its trial.
#[tokio::test]
async fn a_handed_over_auto_run_is_refused_by_name_unless_its_recipe_was_admitted() {
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
        let unprobed = fixture
            .store
            .get_file(route_file_id(&route))
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
        // The owner's own description of the recipe, as its catalog holds it.
        let (_, catalog) = described_catalog(&fixture.state.node_id, [0x45; 32], 2160);
        let (mut intent, run, _) =
            upgrade_after_playing_to(&fixture, &session_id, &route, 60_000).await;
        intent.auto_policy = true;
        intent.origin = super::processed_upgrade::IntentOrigin::HandedOver {
            admitted: Some(admitted),
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

fn route_file_id(route: &MediaSessionRoute) -> i64 {
    serde_json::from_str::<RemoteStartRequest>(&route.recipe_json)
        .expect("the route's recipe")
        .request
        .file_id
}

/// Test plan 2: the ingress hands an applicable upgrade to another owner. A
/// manual-quality ask travels as it is; Auto's carries the owner's described
/// processed recipe once this viewer's link admits it, and only that owner's.
#[tokio::test]
async fn the_ingress_hands_over_its_verdict_and_admits_auto_for_the_chosen_owner() {
    let state = crate::http::tests::test_state_without_router();
    let mut source = hls_file(Vec::new());
    source.height = Some(2160);
    let evidence = super::processed_upgrade::ViewerEvidence::default();
    let mut request = restored_request("handoff-ingress");
    let (described, catalog) = described_catalog("media2", [0x48; 32], 2160);
    request.quality_catalog = Some(catalog);

    assert_eq!(
        super::handed_over_ask(&state, None, &evidence, &request, Some(&source), "media2").await,
        None,
        "no verdict, no ask"
    );
    let manual = handoff_ask(false);
    assert_eq!(
        super::handed_over_ask(
            &state,
            Some(&manual),
            &evidence,
            &request,
            Some(&source),
            "media2"
        )
        .await,
        Some(manual.clone())
    );
    let auto = handoff_ask(true);
    let sent = super::handed_over_ask(
        &state,
        Some(&auto),
        &evidence,
        &request,
        Some(&source),
        "media2",
    )
    .await
    .expect("the ask travels");
    assert_eq!(
        sent.admitted,
        Some(crate::media_sessions::AdmittedProcessedRecipe::of(
            &described
        ))
    );
    let elsewhere = super::handed_over_ask(
        &state,
        Some(&auto),
        &evidence,
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

    // What the start says about it, on the ingress.
    assert_eq!(super::handed_over_note(None, "media2", "s"), None);
    assert_eq!(
        super::handed_over_note(Some(&sent), "media2", "s").as_deref(),
        Some(super::HANDED_OVER_NOTE)
    );
    assert_eq!(
        super::handed_over_note(Some(&elsewhere), "media3", "s"),
        Some(super::processing_declined_note(
            super::processed_upgrade::HANDED_OVER_AUTO_UNADMITTED
        ))
    );
}

/// Test plans 2, 3 and 10, where they are decided: the gates run for every
/// start, a pinned reopen included, so a decline is still noted on this
/// start; the ask is cleared for every candidate and built only for another
/// owner, before that owner is asked; and the local registration goes
/// through the one builder.
#[test]
fn the_create_decides_every_start_and_hands_the_ask_only_to_another_owner() {
    let source = include_str!("../create.rs");
    let gates = source
        .split_once("let processed_upgrade_applies =")
        .expect("the create's gate")
        .1;
    let gates = &gates[..gates.find("let advertise_control").expect("after the gate")];
    assert!(
        !gates.contains("pinned_owner"),
        "a pinned reopen is not exempt: {gates}"
    );
    assert!(gates.contains("plan_notes.push(processing_declined_note(&reason))"));
    assert!(gates.contains("library_channel.is_none()"));
    let placement = source
        .split_once("for candidate in owner_candidates {")
        .expect("the placement loop")
        .1;
    let cleared = placement
        .find("remote_request.processed_upgrade = None;")
        .expect("the ask is cleared per candidate");
    let local = placement
        .find("if candidate == state.node_id {")
        .expect("the local branch");
    let built = placement.find("handed_over_ask(").expect("the remote ask");
    let asked = placement
        .find("start_remote_recovering_lost_reply")
        .expect("the remote start");
    assert!(cleared < local && local < built && built < asked);
    let published = source
        .split_once("if published_route.owner_node_id == state.node_id {")
        .expect("the local publication branch")
        .1;
    let published = &published[..published.find("if candidate_auto_policy").expect("next")];
    assert!(published.contains("processed_upgrade::register_owned("));
    assert!(published.contains("RegistrationSource::Local("));
    assert!(!source.contains("processed_upgrade::register(super::processed_upgrade::UpgradeIntent"));
}

/// Test plans 4, 5 and 11, where they are wired: the owner's start handler
/// registers only a created start that carried an ask, from the request it
/// restored; a successor another node owns carries its ask in its recipe and
/// that owner registers it through the same builder.
#[test]
fn the_owner_registers_handed_over_starts_and_successors_through_the_builder() {
    let source = include_str!("../../internal_media_sessions.rs");
    let start = source
        .split_once("pub(crate) async fn start(")
        .expect("the start handler")
        .1;
    let start = &start[..start
        .find("pub(crate) async fn activate(")
        .expect("next handler")];
    let restored = start
        .find("restore_candidate_context_with_deadline(&mut request")
        .expect("the start restores its context");
    let registered = start
        .find("if let Some(ask) = request.processed_upgrade.as_ref()")
        .expect("only an envelope with an ask registers");
    assert!(restored < registered);
    let gate = &start[..registered];
    assert!(
        gate.trim_end().ends_with("if created {"),
        "a replayed start does not re-register"
    );
    let call = &start[registered..];
    assert!(call.contains("request: &request.request"));
    assert!(call.contains("RegistrationSource::HandedOver"));

    let prepare = source
        .split_once("pub(crate) async fn prepare_authorized(")
        .expect("the prepare handler")
        .1;
    let prepare = &prepare[..prepare
        .find("pub(crate) async fn abort(")
        .expect("next handler")];
    assert!(prepare.contains("recipe.processed_upgrade.clone()"));
    assert!(prepare.contains("restore_candidate_context_with_deadline(&mut recipe"));
    assert!(prepare.contains("RegistrationSource::HandedOver"));

    let preparation = include_str!("../preparation.rs");
    let staged = preparation
        .split_once("let staged_recipe = RemoteStartRequest {")
        .expect("the staged recipe")
        .1;
    let staged = &staged[..staged.find("};").expect("its end")];
    assert!(staged.contains("processed_upgrade: successor_ask"));
    assert!(staged.contains("successor_owner != state.node_id"));
    assert!(preparation.contains("RegistrationSource::Local("));
    assert!(!preparation
        .contains("processed_upgrade::register(super::processed_upgrade::UpgradeIntent"));
}
