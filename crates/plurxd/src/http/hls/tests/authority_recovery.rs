// M2 of docs/cluster/PLAYBACK-QUORUM-GAPS-RCA.md: the manager owns the
// outage outcome, and HTTP only observes it.

/// A bare manager bound to a fence it does not own, with the outage owner
/// loop running, for timing tests on a paused clock.
async fn fenced_manager() -> (
    crate::serving_fence::ServingFence,
    std::sync::Arc<crate::transcode::TranscodeManager>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
) {
    let base = crate::test_tempdir().expect("manager dir");
    let fence = crate::serving_fence::ServingFence::local();
    let manager = std::sync::Arc::new(
        crate::transcode::TranscodeManager::new(
            std::sync::Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store")),
            base.path().join("manager"),
            plurx_core::transcode::EncoderCaps::default(),
            plurx_core::transcode::Pipeline::Cpu,
        )
        .with_serving_authority(fence.authority()),
    );
    let owner = tokio::spawn(std::sync::Arc::clone(&manager).serving_fence_loop(fence.subscribe()));
    manager.outage_owner_started().await;
    (fence, manager, owner, base)
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

fn far_deadline() -> Instant {
    tokio::time::Instant::now().into_std() + Duration::from_secs(30)
}

/// A request that arrives four seconds into an outage waits for what is
/// left of the manager's grace, about one second, not a fresh five.
#[tokio::test(start_paused = true)]
async fn an_arrival_four_seconds_into_an_outage_waits_about_one_second() {
    let (fence, manager, _owner, _base) = fenced_manager().await;
    fence.validation_set_ready(false).await;
    settle().await;
    tokio::time::advance(Duration::from_secs(4)).await;
    let baseline = fence
        .authority()
        .outage_baseline()
        .expect("still fenced four seconds in");
    let arrived = tokio::time::Instant::now();
    let outcome = manager
        .wait_for_outage_resolution(baseline, far_deadline())
        .await;
    assert_ne!(outcome, crate::serving_fence::OutageWait::Recovered);
    assert_eq!(
        arrived.elapsed(),
        Duration::from_secs(1),
        "the wait ends where the manager's own grace ends"
    );
}

/// Once an outage has expired its sessions are gone, and a read arriving
/// later in the same outage gets its 503 at once: the manager does not
/// re-arm a fresh `Pending` grace for an outage it has already torn down.
#[tokio::test(start_paused = true)]
async fn an_arrival_six_seconds_into_an_outage_fails_fast() {
    let (fence, manager, _owner, _base) = fenced_manager().await;
    fence.validation_set_ready(false).await;
    settle().await;
    tokio::time::advance(Duration::from_secs(6)).await;
    settle().await;
    let baseline = fence
        .authority()
        .outage_baseline()
        .expect("still fenced six seconds in");
    assert_eq!(
        manager.outage_resolution_now().state,
        crate::serving_fence::OutageState::Expired,
        "an expired outage stays expired until authority returns"
    );
    let arrived = tokio::time::Instant::now();
    let outcome = manager
        .wait_for_outage_resolution(baseline, far_deadline())
        .await;
    assert_eq!(outcome, crate::serving_fence::OutageWait::Expired);
    assert_eq!(
        arrived.elapsed(),
        Duration::ZERO,
        "nothing is left to wait for"
    );

    // Still expired a full grace later, and a recovery then reopens normally:
    // the next loss gets a grace of its own.
    tokio::time::advance(Duration::from_secs(6)).await;
    settle().await;
    assert_eq!(
        manager.outage_resolution_now().state,
        crate::serving_fence::OutageState::Expired
    );
    fence.validation_set_ready(true).await;
    settle().await;
    fence.validation_set_ready(false).await;
    settle().await;
    assert!(
        matches!(
            manager.outage_resolution_now().state,
            crate::serving_fence::OutageState::Pending { .. }
        ),
        "a loss after authority returned is a new outage with its own grace"
    );
}

/// A new loss that lands after the owner saw authority return, but before it
/// published the recovery, is not that recovery: `Recovered` carries the
/// generation authority came back under, so a request that was admitted
/// under it keeps waiting on the new loss instead of spending its retry.
#[tokio::test(start_paused = true)]
async fn a_loss_landing_before_the_recovery_is_published_is_not_reported_recovered() {
    let (fence, manager, _owner, _base) = fenced_manager().await;
    fence.validation_set_ready(false).await;
    settle().await;
    let first_loss = fence.authority().state().loss_generation;

    let pause = manager
        .test_hooks()
        .outage_loss_resolved
        .arm("outage loss resolved");
    let sessions = manager.hold_sessions_for_test().await;
    fence.validation_set_ready(true).await;
    // The owner has seen the recovery and is stopped before publishing it.
    let hold = pause.reached().await;
    fence.validation_set_ready(false).await;
    let second_loss = fence.authority().state().loss_generation;
    assert_eq!(second_loss, first_loss + 1);
    let baseline = fence.authority().outage_baseline().expect("fenced again");
    assert_eq!(baseline, first_loss, "the generation last held");
    let waiter = tokio::spawn({
        let manager = std::sync::Arc::clone(&manager);
        async move {
            manager
                .wait_for_outage_resolution(baseline, far_deadline())
                .await
        }
    });
    settle().await;

    drop(hold);
    settle().await;
    // Published, and stopped again (on the session lock) before it re-arms.
    assert_eq!(
        manager.outage_resolution_now(),
        crate::serving_fence::OutageResolution {
            loss_generation: first_loss,
            state: crate::serving_fence::OutageState::Recovered,
        },
        "the recovery is reported under the generation it happened in"
    );
    settle().await;
    assert!(
        !waiter.is_finished(),
        "a waiter after the first loss's generation is still waiting on the second"
    );

    drop(sessions);
    settle().await;
    assert_eq!(
        manager.outage_resolution_now().loss_generation,
        second_loss,
        "the second loss gets its own resolution"
    );
    assert!(!waiter.is_finished());
    fence.validation_set_ready(true).await;
    settle().await;
    assert_eq!(
        waiter.await.expect("waiter"),
        crate::serving_fence::OutageWait::Recovered
    );
}

/// Losses that follow each other inside a grace spend one budget, and
/// requests held across them, including ones that are cancelled, neither
/// spend nor reset it.
#[tokio::test(start_paused = true)]
async fn flapping_across_several_requests_spends_one_budget_and_cancelling_does_not_reset_it() {
    let (fence, manager, _owner, _base) = fenced_manager().await;

    fence.validation_set_ready(false).await;
    settle().await;
    let first = fence.authority().outage_baseline().expect("fenced");
    let held = tokio::spawn({
        let manager = std::sync::Arc::clone(&manager);
        async move {
            manager
                .wait_for_outage_resolution(first, far_deadline())
                .await
        }
    });
    tokio::time::advance(Duration::from_secs(1)).await;
    fence.validation_set_ready(true).await;
    settle().await;
    assert_eq!(
        held.await.expect("held request"),
        crate::serving_fence::OutageWait::Recovered
    );

    tokio::time::advance(Duration::from_millis(500)).await;
    fence.validation_set_ready(false).await;
    settle().await;
    let second = fence.authority().outage_baseline().expect("fenced again");
    // Several requests park on the second loss and are dropped: a client
    // giving up must not hand the session a fresh budget.
    for _ in 0..4 {
        let cancelled = tokio::spawn({
            let manager = std::sync::Arc::clone(&manager);
            async move {
                manager
                    .wait_for_outage_resolution(second, far_deadline())
                    .await
            }
        });
        settle().await;
        cancelled.abort();
        let _ = cancelled.await;
    }
    let arrived = tokio::time::Instant::now();
    let outcome = manager
        .wait_for_outage_resolution(second, far_deadline())
        .await;
    assert_ne!(outcome, crate::serving_fence::OutageWait::Recovered);
    assert_eq!(
        arrived.elapsed(),
        Duration::from_secs(4),
        "the second loss gets what the first left of one budget"
    );
}

/// With no owner loop publishing resolutions there is nothing to wait for,
/// so a held request fails fast exactly as it did before the hold existed.
#[tokio::test(start_paused = true)]
async fn without_an_outage_owner_a_wait_fails_fast() {
    let (fence, manager, owner, _base) = fenced_manager().await;
    owner.abort();
    let _ = owner.await;
    fence.validation_set_ready(false).await;
    let arrived = tokio::time::Instant::now();
    assert_eq!(
        manager.wait_for_outage_resolution(0, far_deadline()).await,
        crate::serving_fence::OutageWait::Expired
    );
    assert_eq!(arrived.elapsed(), Duration::ZERO);
}

/// Publication-boundary fixtures: a real VOD owner behind a real AppState
/// whose manager runs the outage owner loop.
async fn vod_recovery_fixture(
    base: &std::path::Path,
    session_id: &str,
) -> (
    HlsDeliveryFixture,
    crate::transcode::MediaResponseOwner,
    tokio::task::JoinHandle<()>,
) {
    let fixture = HlsDeliveryFixture::publish(base, "rolling-unused").await;
    let owner = install_vod_http_session(&fixture, base, session_id).await;
    let loop_task = tokio::spawn(
        std::sync::Arc::clone(&fixture.state.transcode)
            .serving_fence_loop(fixture.state.serving.subscribe()),
    );
    fixture.state.transcode.outage_owner_started().await;
    (fixture, owner, loop_task)
}

fn recover_after(state: &crate::state::AppState, after: Duration) -> tokio::task::JoinHandle<()> {
    let serving = state.serving.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        serving.validation_set_ready(true).await;
    })
}

fn is_serving_fenced(error: &ApiError) -> bool {
    error.code() == Some("serving_fenced")
}

/// A loss seen at admission (`.admit()` refuses) waits for the manager's
/// resolution and then authorizes again from the top.
#[tokio::test]
async fn a_loss_at_admission_recovers_with_a_fresh_authorization() {
    let dir = crate::test_tempdir().expect("recovery dir");
    let (fixture, owner, _loop) = vod_recovery_fixture(dir.path(), "vod-admit-loss").await;
    fixture.state.serving.validation_set_ready(false).await;
    let recovery = recover_after(&fixture.state, Duration::from_millis(150));
    let started = Instant::now();
    authorize_and_commit_buffered(
        &fixture.state,
        "vod-admit-loss",
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media("playlist", Some("index.m3u8")),
        true,
        Instant::now() + Duration::from_secs(3),
    )
    .await
    .expect("authority came back inside the grace");
    assert!(started.elapsed() >= Duration::from_millis(150));
    recovery.await.expect("recovery");
}

/// A loss between authorization and commit abandons the first
/// authorization and runs the whole publication again: two runs, and the
/// second commits under the generation that came back.
#[tokio::test]
async fn a_loss_between_authorization_and_commit_recovers_with_a_fresh_authorization() {
    let dir = crate::test_tempdir().expect("recovery dir");
    let (fixture, owner, _loop) = vod_recovery_fixture(dir.path(), "vod-commit-loss").await;
    let state = &fixture.state;
    let deadline = Instant::now() + Duration::from_secs(3);
    let runs = std::sync::atomic::AtomicUsize::new(0);
    with_authority_recovery(state, "vod-commit-loss", deadline, || async {
        let run = runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let authorization = state
            .transcode
            .authorize_response_publication(
                "vod-commit-loss",
                &owner,
                crate::transcode::MediaResponsePublication::attempt_media(
                    "playlist",
                    Some("index.m3u8"),
                ),
                deadline,
            )
            .await
            .map_err(RawPublicationError::Rejected)?;
        if run == 0 {
            state.serving.validation_set_ready(false).await;
            recover_after(state, Duration::from_millis(100));
        }
        state
            .transcode
            .commit_authorized_media(authorization, true, deadline)
            .await
            .map_err(RawPublicationError::Rejected)
    })
    .await
    .expect("the second, fresh authorization commits");
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// A recovery that a new loss has already superseded by the time the
/// waiting request wakes is not one its single retry may be spent on: the
/// request waits past the new loss and commits once authority is actually
/// held, instead of re-running into a refusal and answering `serving_fenced`.
#[tokio::test(start_paused = true)]
async fn a_recovery_already_superseded_by_a_new_loss_does_not_spend_the_retry() {
    let dir = crate::test_tempdir().expect("recovery dir");
    let (fixture, owner, _loop) = vod_recovery_fixture(dir.path(), "vod-superseded").await;
    let state = fixture.state.clone();
    state.serving.validation_set_ready(false).await;
    let request = tokio::spawn({
        let state = state.clone();
        async move {
            authorize_and_commit_buffered(
                &state,
                "vod-superseded",
                &owner,
                crate::transcode::MediaResponsePublication::attempt_media(
                    "playlist",
                    Some("index.m3u8"),
                ),
                true,
                Instant::now() + Duration::from_secs(30),
            )
            .await
        }
    });
    // On a paused clock this returns only once every task is idle: the
    // request is parked on the outage.
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(!request.is_finished(), "fenced: the request waits");

    // Authority returns, and is lost again before the owner publishes the
    // recovery. The owner then stops (on the session lock) right after
    // publishing it, so the request wakes to a `Recovered` that a new loss
    // has already superseded.
    let pause = state
        .transcode
        .test_hooks()
        .outage_loss_resolved
        .arm("outage loss resolved");
    let sessions = state.transcode.hold_sessions_for_test().await;
    state.serving.validation_set_ready(true).await;
    let hold = pause.reached().await;
    state.serving.validation_set_ready(false).await;
    drop(hold);
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(
        state.transcode.outage_resolution_now().state,
        crate::serving_fence::OutageState::Recovered
    );
    assert!(
        !request.is_finished(),
        "the retry is not spent on a recovery that is already over"
    );
    drop(sessions);
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(
        matches!(
            state.transcode.outage_resolution_now().state,
            crate::serving_fence::OutageState::Pending { .. }
        ),
        "the new loss is pending"
    );
    assert!(!request.is_finished());

    state.serving.validation_set_ready(true).await;
    request
        .await
        .expect("request task")
        .expect("commits once authority is actually held");
}

/// A loss that is not won back inside the wait answers the router's own
/// `serving_fenced`, never `response_state_changed`.
#[tokio::test]
async fn a_fenced_then_expired_response_carries_serving_fenced() {
    let dir = crate::test_tempdir().expect("recovery dir");
    let (fixture, owner, _loop) = vod_recovery_fixture(dir.path(), "vod-expired").await;
    fixture.state.serving.validation_set_ready(false).await;
    let error = authorize_and_commit_buffered(
        &fixture.state,
        "vod-expired",
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media("playlist", Some("index.m3u8")),
        true,
        Instant::now() + Duration::from_millis(200),
    )
    .await
    .expect_err("still fenced at the deadline");
    assert!(is_serving_fenced(&error), "{error:?}");
    assert!(!response_publication_state_changed(&error));
}

/// The session is released while the request waits, and recovery races
/// that retirement: the fresh authorization meets the existing rejection
/// for a released session, never the bytes.
#[tokio::test]
async fn a_session_retired_during_the_wait_is_refused_after_recovery() {
    let dir = crate::test_tempdir().expect("recovery dir");
    let (fixture, owner, _loop) = vod_recovery_fixture(dir.path(), "vod-retired").await;
    fixture.state.serving.validation_set_ready(false).await;
    let retire_then_recover = tokio::spawn({
        let state = fixture.state.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let (_, ()) = tokio::join!(
                state
                    .transcode
                    .begin_session_publication_fence("vod-retired"),
                state.serving.validation_set_ready(true),
            );
        }
    });
    let error = authorize_and_commit_buffered(
        &fixture.state,
        "vod-retired",
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media("playlist", Some("index.m3u8")),
        true,
        Instant::now() + Duration::from_secs(3),
    )
    .await
    .expect_err("a released session is never published to");
    retire_then_recover.await.expect("retire and recover");
    assert!(
        !is_serving_fenced(&error),
        "authority came back, so the refusal is the session's: {error:?}"
    );
}

/// A streamed body commits after it is on the wire, so its completion is
/// never re-run through the authority recovery.
#[test]
fn a_streamed_commit_is_not_retried() {
    let source = include_str!("../response.rs");
    let streamed = source
        .split_once("pub(super) fn settle_streamed_response_completion(")
        .expect("streamed completion")
        .1
        .split_once("pub(super) type StreamedResponseCompletion")
        .expect("end of streamed completion")
        .0;
    assert!(streamed.contains("manager.commit_authorized_media("));
    assert!(!streamed.contains("with_authority_recovery"));
    assert!(!streamed.contains("authorize_and_commit_buffered"));
}

/// Router edge: a read of an existing session that arrives while the node
/// is fenced is held and, once the manager reports recovery, runs as a fresh
/// arrival; a session create and a control post fail fast as before.
#[tokio::test(start_paused = true)]
async fn a_held_read_runs_after_recovery_while_creates_and_control_fail_fast() {
    use tokio::time::Instant;
    let (app, state) = crate::http::tests::test_app_with_state();
    let _loop = tokio::spawn(
        std::sync::Arc::clone(&state.transcode).serving_fence_loop(state.serving.subscribe()),
    );
    state.transcode.outage_owner_started().await;
    state.serving.validation_set_ready(false).await;

    for (method, path) in [
        ("POST", "/api/v1/hls/sessions"),
        ("POST", "/api/v1/hls/probe/control"),
    ] {
        let started = Instant::now();
        let response = tower::ServiceExt::oneshot(
            app.clone(),
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(started.elapsed(), Duration::ZERO, "{path} must not be held");
    }

    let recovery = recover_after(&state, Duration::from_millis(200));
    let started = Instant::now();
    let response = tower::ServiceExt::oneshot(
        app.clone(),
        axum::http::Request::builder()
            .uri("/api/v1/hls/probe/status")
            .body(Body::empty())
            .expect("request"),
    )
    .await
    .expect("response");
    assert_eq!(
        started.elapsed(),
        Duration::from_millis(200),
        "held exactly until the recovery"
    );
    assert_ne!(
        response
            .extensions()
            .get::<crate::http::error::ApiErrorCode>(),
        Some(&crate::http::error::ApiErrorCode("serving_fenced")),
        "the held read ran after recovery instead of being fenced"
    );
    assert_ne!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    recovery.await.expect("recovery");
}

/// M2: the first run of a buffered rolling publication passes the first-media
/// handoff and the actor's authorization, then loses authority before it
/// commits: the handoff is applied and no bytes are committed. The second run
/// starts from the top as a second request would, commits under the
/// generation that came back, and the actor records first media and the
/// delivery frontier exactly once.
#[tokio::test]
async fn abandoning_after_the_first_media_handoff_then_reauthorizing_publishes_once() {
    let dir = crate::test_tempdir().expect("handoff dir");
    let session_id = uuid::Uuid::new_v4().to_string();
    let fixture = HlsDeliveryFixture::publish_actor_managed(dir.path(), &session_id).await;
    let _loop = tokio::spawn(
        std::sync::Arc::clone(&fixture.state.transcode)
            .serving_fence_loop(fixture.state.serving.subscribe()),
    );
    fixture.state.transcode.outage_owner_started().await;
    fixture.mark_started().await;
    tokio::fs::write(dir.path().join("seg00000.m4s"), b"first-media")
        .await
        .expect("first media object");
    let before = fixture.actor_snapshot().await;
    assert!(
        !before.producer_control.producer_media_published,
        "nothing has been published yet"
    );
    let crate::transcode::SegmentPublication::Ready(file) = fixture
        .state
        .transcode
        .segment_for_publication(&session_id, "seg00000.m4s")
        .await
        .expect("resolve first media")
    else {
        panic!("the first media object resolves before publication");
    };
    let owner = file.response_owner();

    let state = &fixture.state;
    let deadline = Instant::now() + Duration::from_secs(3);
    let runs = std::sync::atomic::AtomicUsize::new(0);
    let handoff_seen = std::sync::atomic::AtomicBool::new(false);
    with_authority_recovery(state, &session_id, deadline, || async {
        let run = runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let authorization = state
            .transcode
            .authorize_response_publication(
                &session_id,
                &owner,
                crate::transcode::MediaResponsePublication::attempt_media(
                    "media-segment",
                    Some("seg00000.m4s"),
                ),
                deadline,
            )
            .await
            .map_err(RawPublicationError::Rejected)?;
        if run == 0 {
            // Past the handoff and the actor's authorization, before any
            // byte is committed: lose authority here.
            handoff_seen.store(true, std::sync::atomic::Ordering::SeqCst);
            state.serving.validation_set_ready(false).await;
            recover_after(state, Duration::from_millis(100));
        }
        state
            .transcode
            .commit_authorized_media(authorization, true, deadline)
            .await
            .map_err(RawPublicationError::Rejected)
    })
    .await
    .expect("the second authorization, from the top, commits");
    assert!(handoff_seen.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "abandoned once, authorized again once"
    );

    let delivery = fixture
        .wait_for_delivery_projection("media-segment", Some(0))
        .await;
    assert_eq!(
        delivery.fetched_segment,
        Some(0),
        "the delivery frontier moved to the one object, once"
    );
    assert_eq!(delivery.pending_fetched_segment, None);
    let after = fixture.actor_snapshot().await;
    assert!(
        after.producer_control.producer_media_published,
        "the actor recorded first media"
    );
    assert_eq!(
        after.delivery.producer_attempt, before.delivery.producer_attempt,
        "the same attempt, not a replacement"
    );
}
