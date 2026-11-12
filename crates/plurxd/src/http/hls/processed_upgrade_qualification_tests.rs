use super::*;
use plurx_core::transcode::dv_fel_qualification as q;

pub(in crate::http::hls) fn pending_qualification_fixture(
    file_id: i64,
    size: i64,
    mtime: i64,
    settings_generation: i64,
    object: &str,
) -> PendingFelQualification {
    let envelope = plurx_core::transcode::EnhancementRateEnvelope::resolve(
        20_000,
        plurx_core::transcode::EnhancementRateProvenance::ExplicitEncode,
        1920,
        1080,
        (24, 1),
        true,
        Default::default(),
    )
    .expect("synthetic envelope");
    PendingFelQualification {
        file_id,
        settings_generation,
        source_object_version: object.into(),
        request: q::FelRecipeRequest::new(
            "a".repeat(64),
            size,
            mtime,
            envelope,
            q::FelRecipeOutput::new(0, (1920, 1080), (24, 1)),
        ),
    }
}

fn selection() -> ClientSelection {
    use crate::playback_control::*;
    ClientSelection {
        quality: QualitySelection::Auto {
            height: None,
            candidate_id: None,
        },
        audio_track: None,
        subtitle: SubtitleSelection {
            mode: SubtitleMode::Off,
            track: None,
        },
        audio_offset_ms: 0,
        codec: CodecPolicy::Auto,
        dynamic_range: DynamicRangePolicy::Auto,
    }
}

fn awaiting(user: i64) -> (String, String, String) {
    let (playback, session, generation) = super::super::tests::waiting(user);
    let (intent, run) = take_on_first_frame(user, &session).expect("first frame");
    assert!(retain(
        &intent,
        &run,
        pending_qualification_fixture(7, 1, 1, 0, "metadata:1")
    ));
    drop(run);
    (playback, session, generation)
}

fn document(wait: &PendingFelQualification, admitted: bool) -> String {
    let mut verdict = q::FelVerdict::unmeasurable(
        wait.request.identity.clone(),
        wait.request.source_size,
        wait.request.source_mtime,
        1,
    );
    // Synthetic decision data tests scheduling only, never production measurement.
    verdict.admitted = admitted;
    serde_json::json!({q::VARIANTS_KEY: {wait.request.identity.clone():
        q::FelRecipeVerdict { request: wait.request.clone(), verdict }}})
    .to_string()
}

#[test]
fn only_an_exact_pending_token_can_arm_a_continuation() {
    let wait = pending_qualification_fixture(7, 1, 1, 0, "metadata:1");
    let encoded = crate::transcode::pending_fel_qualification_error(wait.clone());
    let decoded = crate::transcode::pending_fel_qualification(&encoded).expect("typed token");
    assert!(decoded.matches(7, 0, "metadata:1", &wait.request));
    assert!(!decoded.matches(8, 0, "metadata:1", &wait.request));
    assert!(!decoded.matches(7, 1, "metadata:1", &wait.request));
    assert!(!decoded.matches(7, 0, "metadata:other", &wait.request));
    let mut foreign = wait.request.clone();
    foreign.backend_identity = "b".repeat(64);
    assert!(!decoded.matches(7, 0, "metadata:1", &foreign));
    assert!(
        crate::transcode::pending_fel_qualification(wait.reason()).is_none(),
        "human text is never a retry token"
    );
    assert!(crate::transcode::pending_fel_qualification(&"x".repeat(8193)).is_none());
}

#[test]
fn a_new_exact_receipt_can_resume_but_rejected_foreign_or_stale_receipts_cannot() {
    let wait = pending_qualification_fixture(7, 1, 1, 0, "metadata:1");
    assert!(matches!(
        exact_receipt(&wait, 1, 1, 0, None),
        Receipt::Missing
    ));
    let admitted = document(&wait, true);
    assert!(matches!(
        exact_receipt(&wait, 1, 1, 0, Some(&admitted)),
        Receipt::Admitted
    ));
    let rejected = document(&wait, false);
    assert!(matches!(
        exact_receipt(&wait, 1, 1, 0, Some(&rejected)),
        Receipt::Rejected(_)
    ));
    assert!(matches!(
        exact_receipt(&wait, 2, 1, 0, Some(&admitted)),
        Receipt::Rejected(_)
    ));
    assert!(matches!(
        exact_receipt(&wait, 1, 1, 1, Some(&admitted)),
        Receipt::Rejected(_)
    ));
    let mut foreign = wait.clone();
    foreign.request = q::FelRecipeRequest::new(
        "b".repeat(64),
        1,
        1,
        wait.request.envelope,
        wait.request.output,
    );
    assert!(matches!(
        exact_receipt(&wait, 1, 1, 0, Some(&document(&foreign, true))),
        Receipt::Missing
    ));
}

#[test]
fn duplicate_control_events_claim_one_check_and_keep_the_truthful_reason() {
    let (_, session, generation) = awaiting(61);
    let selection = selection();
    let check =
        Check::take(&session, &generation, &selection, true).expect("accepted control check");
    assert!(Check::take(&session, &generation, &selection, true).is_none());
    assert!(Check::take(&session, "other-generation", &selection, true).is_none());
    assert!(processing_skip(&session, &generation)
        .expect("reason while checking")
        .reason
        .contains("quality measurement"));
    drop(check);
    assert!(
        Check::take(&session, &generation, &selection, true).is_some(),
        "a still-pending receipt may be looked up on another accepted event"
    );
    cancel_for_session(&session);
}

#[test]
fn release_changed_selection_and_expiry_cannot_revive_an_old_waiter() {
    let (_, session, generation) = awaiting(62);
    let check = Check::take(&session, &generation, &selection(), true).expect("check");
    cancel_for_session(&session);
    check.terminal("a late result");
    assert!(processing_skip(&session, &generation).is_none());
    assert!(Check::take(&session, &generation, &selection(), true).is_none());
    let (_, session, generation) = awaiting(63);
    drop(Check::take(&session, &generation, &selection(), true).expect("first accepted selection"));
    let mut changed = selection();
    changed.audio_track = Some(1);
    assert!(Check::take(&session, &generation, &changed, false).is_none());
    assert!(processing_skip(&session, &generation)
        .expect("terminal reason")
        .reason
        .contains("selection changed"));
    assert!(Check::take(&session, &generation, &selection(), true).is_none());
    cancel_for_session(&session);
    let (_, session, generation) = awaiting(64);
    if let Some(Entry::AwaitingQualification(wait)) = lock().get_mut(&session) {
        wait.recorded = std::time::Instant::now() - RETENTION;
    }
    assert!(Check::take(&session, &generation, &selection(), true).is_none());
    assert!(processing_skip(&session, &generation)
        .expect("expired reason")
        .reason
        .contains("expired"));
    cancel_for_session(&session);
}

#[test]
fn a_qualified_retry_keeps_auto_original_and_manual_as_the_user_actually_selected() {
    use crate::playback_control::QualitySelection;
    for quality in [
        QualitySelection::Original,
        QualitySelection::Manual { height: 720 },
        QualitySelection::Auto {
            height: None,
            candidate_id: None,
        },
    ] {
        let mut asked = selection();
        asked.quality = quality.clone();
        let pinned = super::super::super::preparation::qualified_upgrade_selection(&asked, 1080);
        match quality {
            QualitySelection::Auto { .. } => assert!(matches!(
                pinned.quality,
                QualitySelection::Auto {
                    height: Some(1080),
                    candidate_id: None
                }
            )),
            _ => assert_eq!(pinned.quality, quality),
        }
        assert_eq!(pinned.audio_track, asked.audio_track);
        assert_eq!(pinned.subtitle, asked.subtitle);
    }
}

#[test]
fn refreshed_auto_candidate_identity_is_not_a_new_user_quality_choice() {
    use crate::playback_control::QualitySelection;
    let before = selection();
    let mut after = before.clone();
    after.quality = QualitySelection::Auto {
        height: Some(1080),
        candidate_id: Some(
            plurx_core::playback::candidate::CandidateId::for_recipe_digest([7; 32]),
        ),
    };
    assert!(same_accepted_selection(&before, &after, 1080));
    after.quality = QualitySelection::Auto {
        height: Some(720),
        candidate_id: None,
    };
    assert!(!same_accepted_selection(&before, &after, 1080));
}

#[test]
fn a_resumed_quality_mode_uses_its_fresh_auto_policy_and_accepted_owner_epoch() {
    let (_, session, generation) = awaiting(65);
    if let Some(Entry::AwaitingQualification(wait)) = lock().get_mut(&session) {
        wait.intent.auto_policy = false;
    }
    let check =
        Check::take(&session, &generation, &selection(), true).expect("accepted Auto control");
    let mut request = super::super::tests::ordinary_request("fresh-policy");
    request.automatic = true;
    request.candidate_context = Some(Box::new(
        crate::transcode::continuous_test_candidate_context(
            plurx_core::playback::candidate::CandidateId::for_recipe_digest([8; 32]),
            [8; 32],
            1080,
        ),
    ));
    let (intent, run) = check.ready(request, 17).expect("resumed continuation");
    assert!(
        intent.auto_policy,
        "a formerly manual start must now run Auto's admission"
    );
    assert_eq!(run.expected_owner_epoch, Some(17));
    drop(run);
    drop(check);
    cancel_for_session(&session);
}
