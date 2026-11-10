//! The processed upgrade's scheduling: it runs after a first frame for its own
//! session, never before one, and never for a session that has gone.

use super::*;

pub(in crate::http) fn ordinary_request(playback_id: &str) -> crate::transcode::SessionRequest {
    crate::transcode::SessionRequest {
        source_telemetry: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        sdr_master_codecs: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        file_id: 7,
        playback_id: playback_id.to_owned(),
        request_id: None,
        control_sequence: None,
        automatic: true,
        previous_session_id: None,
        reopen_reason: None,
        kind: crate::transcode::SessionKind::Transcode { height: 1080 },
        start_seconds: 0.0,
        audio_index: None,
        audio_delivery: None,
        audio_claim: None,
        subtitle_burn: None,
        audio_offset_ms: 0,
        hdr10: true,
        dv_processing_destination: None,
        presentation: crate::transcode::Presentation::Vod,
        block_budget_secs: None,
        transport: None,
    }
}

/// A waiting upgrade for a fresh session of a fresh playback, by `user_id`.
fn waiting(user_id: i64) -> (String, String, String) {
    let playback = uuid::Uuid::new_v4().to_string();
    let session = uuid::Uuid::new_v4().to_string();
    let incarnation = uuid::Uuid::new_v4().to_string();
    register(UpgradeIntent {
        user_id,
        playback_id: playback.clone(),
        session_id: session.clone(),
        incarnation_id: incarnation.clone(),
        request: ordinary_request(&playback),
        hdr10_intent: false,
        auto_policy: true,
        network: None,
        network_prior: None,
        incumbent_receipt: None,
    });
    (playback, session, incarnation)
}

#[test]
fn an_upgrade_is_scheduled_only_by_its_own_sessions_first_frame() {
    let (_, session, _) = waiting(11);
    // Registered at create: nothing runs before a first frame.
    assert!(is_waiting(&session));
    assert!(!is_running(&session));
    // Another viewer's frame, or a frame for another session, is not this
    // session's first frame.
    assert!(take_on_first_frame(12, &session).is_none());
    assert!(take_on_first_frame(11, &uuid::Uuid::new_v4().to_string()).is_none());
    assert!(is_waiting(&session));
    let (intent, run) = take_on_first_frame(11, &session).expect("its own first frame");
    assert_eq!(intent.session_id, session);
    assert!(is_running(&session));
    // A later frame (a seek's picture, a stall's recovery) schedules nothing.
    assert!(take_on_first_frame(11, &session).is_none());
    drop(run);
    assert!(!is_running(&session) && !is_waiting(&session));
}

#[test]
fn a_session_released_before_its_first_frame_schedules_nothing() {
    let (_, session, _) = waiting(21);
    cancel_for_session(&session);
    assert!(take_on_first_frame(21, &session).is_none());
}

#[test]
fn a_release_during_the_trial_cancels_it() {
    let (_, session, _) = waiting(31);
    let (_intent, run) = take_on_first_frame(31, &session).expect("first frame");
    cancel_for_session(&session);
    assert!(run.cancelled().is_cancelled());
    assert!(!is_running(&session));
}

#[test]
fn a_newer_session_for_the_playback_cancels_its_predecessors_upgrade() {
    let (playback, session, incarnation) = waiting(41);
    let (_intent, run) = take_on_first_frame(41, &session).expect("first frame");
    // The session's own activation (or its committed successor's, which is
    // registered under another incarnation) is not a supersession of it.
    cancel_for_playback(&playback, Some(&incarnation));
    assert!(!run.cancelled().is_cancelled());
    cancel_for_playback(&playback, Some(&uuid::Uuid::new_v4().to_string()));
    assert!(run.cancelled().is_cancelled());
    assert!(!is_running(&session));
    // A waiting one goes too.
    let (playback, session, _) = waiting(42);
    cancel_for_playback(&playback, None);
    assert!(take_on_first_frame(42, &session).is_none());
}

#[test]
fn a_viewer_change_owns_the_playback_slot_over_an_upgrade() {
    use super::super::preparation::{PendingCandidateGuard, begin_processed_upgrade};
    let playback = uuid::Uuid::new_v4().to_string();
    // A viewer's own change in flight: the upgrade does not take its slot.
    let viewer = PendingCandidateGuard::begin(&playback, "viewer-ask");
    assert!(
        begin_processed_upgrade(&playback, &tokio_util::sync::CancellationToken::new()).is_none()
    );
    assert!(
        !viewer.cancelled(),
        "an upgrade never cancels the viewer's ask"
    );
    drop(viewer);
    // An upgrade in its trial: a viewer's change takes the slot and cancels it.
    let upgrade = begin_processed_upgrade(&playback, &tokio_util::sync::CancellationToken::new())
        .expect("an idle playback");
    assert!(!upgrade.cancelled());
    let viewer = PendingCandidateGuard::begin(&playback, "viewer-ask");
    assert!(upgrade.cancelled() && upgrade.cancellation_token().is_cancelled());
    assert!(!viewer.cancelled());
}

/// Review 3 S1: cancellation reaches the staging half through the playback
/// claim, not by dropping the upgrade's future. The claim's token is the one
/// the trial races and the staged successor registers under, so a release
/// cancels it and staging exits by its own cleanup.
#[test]
fn a_release_cancels_the_upgrades_playback_claim() {
    use super::super::preparation::begin_processed_upgrade;
    let (playback, session, _) = waiting(51);
    let (_intent, run) = take_on_first_frame(51, &session).expect("first frame");
    let claim = begin_processed_upgrade(&playback, run.cancelled()).expect("an idle playback");
    assert!(!claim.cancelled());
    cancel_for_session(&session);
    assert!(claim.cancelled() && claim.cancellation_token().is_cancelled());
    // A run already cancelled claims nothing.
    let (playback, session, _) = waiting(52);
    let (_intent, run) = take_on_first_frame(52, &session).expect("first frame");
    cancel_for_session(&session);
    assert!(begin_processed_upgrade(&playback, run.cancelled()).is_none());
}

#[test]
fn a_skipped_upgrade_explains_only_its_own_generation_until_release_or_replacement() {
    let (playback, session, generation) = waiting(51);
    let (_, run) = take_on_first_frame(51, &session).expect("first frame");
    run.skipped("the title is being measured");
    drop(run);
    assert_eq!(
        processing_skip(&session, &generation)
            .expect("current explanation")
            .reason,
        "the title is being measured"
    );
    assert!(processing_skip(&session, "other-generation").is_none());
    cancel_for_playback(&playback, Some("replacement-generation"));
    assert!(processing_skip(&session, &generation).is_none());

    let (_, session, generation) = waiting(52);
    let (_, run) = take_on_first_frame(52, &session).expect("first frame");
    cancel_for_session(&session);
    run.skipped("a cancelled run must not republish a reason");
    drop(run);
    assert!(processing_skip(&session, &generation).is_none());
}
