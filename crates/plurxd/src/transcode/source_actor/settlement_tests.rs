//! Actual Source factory/worker regressions for native closure versus Source
//! SQL disposition. These bodies are authored for later qualification only.
use super::*;
use plurx_core::sharing_source_sessions::{
    SourceIngressAdmissionPermission, SourceReleaseOutcome, SourceSessionWriteAuthority,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_existing_route_factory_releases_only_its_native_admission() {
    Box::pin(source_copy_preadmission_fixture(80)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_expired_factory_worker_leaves_no_native_restart_uncertainty() {
    Box::pin(source_copy_preadmission_fixture(81)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_sql_transient_exhaustion_after_original_physical_close_disarms_factory() {
    Box::pin(source_copy_preadmission_fixture(82)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_sql_release_refused_after_original_physical_close_disarms_factory() {
    Box::pin(source_copy_preadmission_fixture(83)).await;
}

/// Called after the original fixture has booted the real voter/runtime and
/// created its genuine claim, assignment, activation and accepted HTTP driver.
pub(super) async fn exercise(
    state: Arc<crate::state::AppState>,
    manager: Arc<TranscodeManager>,
    prepared: crate::http::hls::PreparedSourcePlayback,
    assignment: SourceDispatchAssignment,
    authority: (
        SourceSessionWriteAuthority,
        SourceIngressAdmissionPermission,
    ),
    ingress: SourceFactoryIngressFixture,
    mode: u8,
) {
    assert!((80..=83).contains(&mode));
    let (activation, permission) = authority;
    let incarnation = assignment.binding().incarnation_id().to_string();
    let existing = if mode == 80 {
        let session = uuid::Uuid::new_v4().to_string();
        let now = crate::fragment_index_cluster::unix_ms();
        // A genuine blocked activation is durable metadata, not a claim that
        // a producer or a published playback envelope exists. This factory
        // must leave that pre-existing route completely untouched.
        let activated = state
            .store
            .activate_source_media_session(
                &activation,
                &plurx_core::domain::MediaSessionActivation {
                    incarnation_id: incarnation.clone(),
                    session_id: session.clone(),
                    principal: assignment.binding().principal().clone(),
                    playback_id: prepared.request().playback_id.clone(),
                    recovery_epoch: String::new(),
                    expected_predecessor_incarnation_id: None,
                    fence_predecessor: true,
                    request_id: Some(assignment.binding().request_id().into()),
                    request_fingerprint: prepared.fingerprint().into(),
                    owner_node_id: assignment.owner_node_id().into(),
                    recipe_json: serde_json::to_string(prepared.request())
                        .expect("original prepared recipe"),
                    response_json: "{}".into(),
                    publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
                    media_origin_ms: 0,
                    now_ms: now,
                    lease_expires_at_ms: now + 60_000,
                    expected_desired_revision: None,
                },
            )
            .await
            .expect("actual Source activation before duplicate factory")
            .expect("actual current activation authority");
        assert_eq!(activated.route.session_id, session);
        Some(session)
    } else {
        None
    };
    let deadline = if mode == 81 {
        Instant::now()
    } else {
        Instant::now() + Duration::from_secs(30)
    };
    // Install these manager-local observers before the actual worker can
    // launch. A first spawned_pid() read after startup would install empty
    // test hooks after the original after_spawn event already used Noop.
    assert_eq!(manager.source_workers.index_hooks.spawned_pid(), 0);
    assert_eq!(manager.source_workers.probe_hooks.spawned_pid(), 0);
    assert_eq!(manager.source_workers.native_hooks.spawned_pid(), 0);
    let actor = Box::pin(manager.start_source_worker(
        Arc::clone(&state),
        assignment.clone(),
        activation,
        permission,
        prepared,
        deadline,
    ))
    .await
    .expect("real factory registered the original owner");

    if mode <= 81 {
        let start = tokio::time::timeout(
            Duration::from_secs(30),
            actor.wait_ready(Instant::now() + Duration::from_secs(30)),
        )
        .await
        .expect("FAILURE watchdog: original no-work owner did not finish startup");
        assert!(matches!(
            start,
            Err(SourceWorkerError::Unresolved | SourceWorkerError::Deadline)
        ));
        let settled = tokio::time::timeout(Duration::from_secs(30), actor.retire())
            .await
            .expect("FAILURE watchdog: no-work factory owner failed to retire");
        assert_eq!(settled, Err(SourceWorkerError::Unresolved));
        wait_finished(&actor).await;
        assert_eq!(manager.source_workers.index_hooks.spawned_pid(), 0);
        assert_eq!(manager.source_workers.probe_hooks.spawned_pid(), 0);
        assert_eq!(manager.source_workers.native_hooks.spawned_pid(), 0);
        if let Some(session) = existing.as_ref() {
            let route = state
                .store
                .media_session_route_by_incarnation(&incarnation)
                .await
                .expect("read original pre-existing route")
                .expect("duplicate factory cannot remove another route");
            assert_eq!(&route.session_id, session);
            assert_eq!(
                route.publication_ready_at_ms,
                MEDIA_SESSION_PUBLICATION_BLOCKED
            );
            assert_ne!(route.state, "ended");
        } else {
            assert!(state
                .store
                .media_session_route_by_incarnation(&incarnation)
                .await
                .expect("unactivated route read")
                .is_none());
        }
        drop(actor); // Exercise the actual last cached actor's guard Drop too.
        assert_native_disposition(&state, &manager, &assignment).await;
        ingress.close().await;
        cleanup_sql(&state, &assignment).await;
        return;
    }

    let ingress_monitor = ingress.monitor_actor(actor.clone());
    tokio::time::timeout(
        Duration::from_secs(30),
        actor.wait_ready(Instant::now() + Duration::from_secs(30)),
    )
    .await
    .expect("FAILURE watchdog: actual native Source startup did not finish")
    .expect("actual Source copy/native-caption producer ready");
    assert!(manager.source_workers.index_hooks.spawned_pid() > 0);
    assert!(manager.source_workers.native_hooks.spawned_pid() > 0);
    actor
        .0
        .control_hooks
        .fixture_arm_settlement_sql(if mode == 82 {
            control::SourceSettlementSqlFault::Transient
        } else {
            control::SourceSettlementSqlFault::Refused
        });
    let opened = actor
        .open_resource(
            &SharingHlsResource::parse("index.m3u8").expect("actual playlist request"),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .expect("actual counted Source response guard");
    let (payload, body) = opened.into_parts();
    assert!(matches!(
        payload,
        SourceResourcePayload::Playlist(ref bytes) if bytes.starts_with(b"#EXTM3U")
    ));
    let mut retiring = Box::pin(actor.retire());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut retiring)
            .await
            .is_err(),
        "the actual response guard must still block owner completion"
    );
    assert_eq!(
        state
            .serving
            .restart_drain_status(0)
            .await
            .admissions_in_flight,
        1,
        "native/SQL retry cannot release custody while a real body remains"
    );
    assert_eq!(
        actor.0.control_hooks.fixture_settlement_sql_observations(),
        (0, 0)
    );
    drop(body);
    drop(payload);
    let settled = tokio::time::timeout(Duration::from_secs(30), &mut retiring)
        .await
        .expect("FAILURE watchdog: original physical close/SQL retry bound did not finish");
    assert_eq!(settled, Err(SourceWorkerError::Unresolved));
    drop(retiring);
    // retire() can report an interim SQL error; only this original owner's
    // finished transition proves that the bounded retry loop has ended.
    wait_finished(&actor).await;
    let (attempts, registered) = actor.0.control_hooks.fixture_settlement_sql_observations();
    assert_eq!(
        attempts,
        if mode == 82 {
            SOURCE_SETTLEMENT_ATTEMPTS as usize
        } else {
            1
        },
        "real existing retry bound, not an injected deadline"
    );
    assert_eq!(
        registered, attempts,
        "each SQL fault followed its same matching producer proof"
    );
    let route = state
        .store
        .media_session_route_by_incarnation(&incarnation)
        .await
        .expect("actual terminal route read")
        .expect("actual activated route is retained");
    assert_eq!(route.state, "ended");
    drop(actor);
    ingress_monitor.join().await;
    assert_native_disposition(&state, &manager, &assignment).await;
    cleanup_sql(&state, &assignment).await;
}

async fn wait_finished(actor: &SourceViewerActor) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let changed = actor.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if actor.0.state.lock().expect("actual owner state").finished {
                break;
            }
            changed.await;
        }
    })
    .await
    .expect("FAILURE watchdog: original owner did not reach its finished transition");
}

async fn assert_native_disposition(
    state: &crate::state::AppState,
    manager: &TranscodeManager,
    assignment: &SourceDispatchAssignment,
) {
    assert!(manager.lookup_source_worker(assignment).is_none());
    assert_eq!(manager.admissions.software_in_use(), 0);
    assert!(manager.vod.live_session_ids().await.is_empty());
    let runtime = manager
        .resource_runtime
        .as_ref()
        .expect("real fixture runtime");
    assert_eq!(
        runtime
            .accounting()
            .expect("actual post-owner accounting")
            .used,
        plurx_core::adaptive_resources::ResourceVector::default()
    );
    // Unlike a new admission check (policy2), this status also observes the
    // persistent native-cleanup uncertainty bit after the last actor drops.
    let status = state.serving.restart_drain_status(0).await;
    assert_eq!(status.admissions_in_flight, 0);
    assert!(
        status.drained,
        "known native closure/absence must not poison restart drain"
    );
}

async fn cleanup_sql(state: &crate::state::AppState, assignment: &SourceDispatchAssignment) {
    let incarnation = assignment.binding().incarnation_id().to_string();
    if let Some(route) = state
        .store
        .media_session_route_by_incarnation(&incarnation)
        .await
        .expect("actual retained route")
    {
        let terminal = if route.state == "ended" {
            route
        } else {
            state
                .store
                .end_media_session_if_owner(&plurx_core::domain::MediaSessionEnd {
                    incarnation_id: route.incarnation_id,
                    session_id: route.session_id,
                    expected_owner_node_id: route.owner_node_id,
                    expected_owner_epoch: route.owner_epoch,
                    expected_lease_expires_at_ms: route.lease_expires_at_ms,
                    terminal_reason: "deleted".into(),
                    now_ms: crate::fragment_index_cluster::unix_ms(),
                })
                .await
                .expect("actual route fixture cleanup")
                .expect("fixture still owns exact activation")
        };
        assert_eq!(
            state
                .store
                .settle_source_terminal_worker(assignment, &terminal)
                .await
                .expect("real Source SQL cleanup after fault injection"),
            SourceReleaseOutcome::Released
        );
    } else {
        assert_eq!(
            state
                .store
                .settle_source_assigned_without_activation(assignment)
                .await
                .expect("real unactivated Source SQL cleanup"),
            SourceReleaseOutcome::Released
        );
    }
}
