use super::*;
use crate::state::{AppState, Dirs};
use plurx_core::{
    domain::{MediaSessionActivation, MediaSessionActivationSettlement},
    playback_principal::PlaybackPrincipal,
    store::{MediaSessionStore, SqliteStore, UserStore},
};

struct Fixture {
    _root: tempfile::TempDir,
    store: Arc<SqliteStore>,
    state: AppState,
    principal: PlaybackPrincipal,
}

impl Fixture {
    async fn new() -> Self {
        let root = crate::test_tempdir().expect("release fixture");
        let store = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
        let user = store
            .create_user("release-viewer", "hash", false)
            .await
            .expect("viewer");
        let dirs = Dirs {
            artwork: root.path().join("artwork"),
            transcode: root.path().join("transcode"),
            cache: root.path().join("cache"),
            subs: root.path().join("subs"),
            runtime_cache: root.path().join("runtime"),
            renditions: root.path().join("renditions"),
        };
        for directory in [
            &dirs.artwork,
            &dirs.transcode,
            &dirs.cache,
            &dirs.subs,
            &dirs.runtime_cache,
            &dirs.renditions,
        ] {
            std::fs::create_dir(directory).expect("owned directory");
        }
        let state = AppState::new_unhooked(
            "release-test".into(),
            store.clone(),
            dirs,
            "entry-node".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(16)),
        );
        Self {
            _root: root,
            store,
            state,
            principal: PlaybackPrincipal::LocalUser { user_id: user.id },
        }
    }

    async fn active(&self, owner: &str, created_at: i64) -> MediaSessionRoute {
        let incarnation = uuid::Uuid::new_v4().to_string();
        let activation = MediaSessionActivation {
            incarnation_id: incarnation.clone(),
            session_id: uuid::Uuid::new_v4().to_string(),
            principal: self.principal.clone(),
            playback_id: uuid::Uuid::new_v4().to_string(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: owner.into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: created_at,
            lease_expires_at_ms: created_at + 900_000,
            expected_desired_revision: None,
        };
        self.store
            .activate_media_session(&activation)
            .await
            .expect("activate")
            .expect("activation wins");
        let route = self
            .store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                created_at,
            )
            .await
            .expect("confirm exact owner")
            .expect("confirmed route");
        self.state.media_sessions.cache_route(route.clone()).await;
        route
    }

    async fn ended(&self, owner: &str, created_at: i64) -> MediaSessionRoute {
        let active = self.active(owner, created_at).await;
        let ended = self
            .store
            .end_media_session(
                &active.session_id,
                crate::vodserve::Terminal::Deleted.durable_reason(),
                created_at + 1,
            )
            .await
            .expect("durable End")
            .expect("ended route");
        assert_eq!(ended.state, "ended");
        assert_eq!(
            ended.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );
        // Seed a real persisted post-End observation, as a restarted owner
        // would recover it. Historical tests use genuinely elapsed wall time.
        self.store
            .arm_media_session_terminal_projection(
                &ended.incarnation_id,
                &ended.owner_node_id,
                ended.owner_epoch,
                created_at + 2 + MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS,
                created_at + 2,
            )
            .await
            .expect("arm exact terminal projection")
            .expect("armed route")
    }

    async fn begin_release(&self, route: &MediaSessionRoute) -> Arc<ReleaseSettlement> {
        let settlement = match self
            .state
            .media_sessions
            .begin_release_reconciliation_with_intent(&route.session_id, ReleaseIntent::CLIENT)
            .await
        {
            ReleaseAdmission::Won(settlement) => settlement,
            ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("first release wins"),
        };
        let proof = self
            .state
            .media_sessions
            .complete_release_with_route(route.clone())
            .await
            .expect("authoritative ended route");
        assert!(
            !proof.terminal_projection_complete(),
            "End alone does not prove predecessor publication settled"
        );
        settlement
    }

    async fn durable(&self, route: &MediaSessionRoute) -> MediaSessionRoute {
        self.store
            .media_session_route_by_incarnation(&route.incarnation_id)
            .await
            .expect("read durable route")
            .expect("route retained")
    }

    async fn assert_terminal_cached(&self, route: &MediaSessionRoute, projection_complete: bool) {
        let cached = self
            .state
            .media_sessions
            .cached_route(&route.session_id)
            .await
            .expect("cached authority")
            .expect("durable terminal state must not become cached absence");
        assert_eq!(cached.incarnation_id, route.incarnation_id);
        assert_eq!(cached.owner_epoch, route.owner_epoch);
        assert_eq!(cached.state, "ended");
        assert_eq!(cached.publication_ready_at_ms == 0, projection_complete);
    }
}

#[tokio::test]
async fn remote_release_without_owner_ack_keeps_terminal_authority_and_projection_pending() {
    let fixture = Fixture::new().await;
    let route = fixture.ended("unreachable-owner", unix_ms()).await;
    let settlement = fixture.begin_release(&route).await;
    let deadline = tokio::time::Instant::now() + TERMINAL_PROJECTION_SAFETY_WINDOW;
    assert!(
        !cleanup_reconciled_release(&fixture.state, &route, deadline, ReleaseIntent::CLIENT).await,
        "an unreachable peer cannot acknowledge its predecessor generation"
    );
    let durable = fixture.durable(&route).await;
    assert_eq!(
        durable.publication_ready_at_ms,
        route.publication_ready_at_ms
    );
    fixture.assert_terminal_cached(&route, false).await;
    assert!(
        settlement.result.lock().expect("settlement").is_none(),
        "unacknowledged cleanup cannot complete DELETE"
    );
}

#[tokio::test]
async fn local_release_persists_exact_owner_projection_before_the_release_can_settle() {
    let fixture = Fixture::new().await;
    let route = fixture.ended("entry-node", unix_ms()).await;
    let settlement = fixture.begin_release(&route).await;
    assert!(
        cleanup_reconciled_release(
            &fixture.state,
            &route,
            tokio::time::Instant::now() + TERMINAL_PROJECTION_SAFETY_WINDOW,
            ReleaseIntent::CLIENT,
        )
        .await
    );
    let durable = fixture.durable(&route).await;
    assert_eq!(
        durable.publication_ready_at_ms, 0,
        "local fencing is followed by durable exact projection"
    );
    fixture.assert_terminal_cached(&route, true).await;
    assert!(
        settlement.result.lock().expect("settlement").is_none(),
        "cleanup does not impersonate the elected response owner"
    );
    fixture
        .state
        .media_sessions
        .complete_release_settlement(&route.session_id, &settlement, StatusCode::NO_CONTENT)
        .await;
    assert_eq!(settlement.wait().await, StatusCode::NO_CONTENT);
    assert!(
        cleanup_reconciled_release(
            &fixture.state,
            &durable,
            tokio::time::Instant::now(),
            ReleaseIntent::CLIENT,
        )
        .await,
        "a persisted complete projection is idempotent"
    );
}

#[tokio::test]
async fn remote_release_settles_only_after_the_exact_persisted_response_safety_boundary() {
    let fixture = Fixture::new().await;
    let created = unix_ms() - MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS - 10_000;
    let route = fixture.ended("unreachable-owner", created).await;
    let settlement = fixture.begin_release(&route).await;
    assert!(
        route.publication_ready_at_ms < unix_ms(),
        "the fixture represents a prior process's elapsed persisted boundary"
    );
    assert!(
        cleanup_reconciled_release(
            &fixture.state,
            &route,
            tokio::time::Instant::now(),
            ReleaseIntent::CLIENT,
        )
        .await,
        "actual elapsed durable safety proof does not require fabricated peer acknowledgement"
    );
    let durable = fixture.durable(&route).await;
    assert_eq!(durable.publication_ready_at_ms, 0);
    assert_eq!(
        durable.terminal_reason, route.terminal_reason,
        "reconciliation preserves the first terminal cause"
    );
    fixture.assert_terminal_cached(&route, true).await;
    assert!(settlement.result.lock().expect("settlement").is_none());
}

#[tokio::test]
async fn a_local_deadline_alone_cannot_clear_an_unelapsed_durable_safety_boundary() {
    let fixture = Fixture::new().await;
    let route = fixture.ended("unreachable-owner", unix_ms()).await;
    let settlement = fixture.begin_release(&route).await;
    assert!(!cleanup_reconciled_release(
        &fixture.state,&route,tokio::time::Instant::now(),ReleaseIntent::CLIENT,
    ).await,"the Store rejects a premature SafetyBoundaryElapsed proof even when a caller's monotonic deadline expired");
    assert_eq!(
        fixture.durable(&route).await.publication_ready_at_ms,
        route.publication_ready_at_ms
    );
    fixture.assert_terminal_cached(&route, false).await;
    assert!(settlement.result.lock().expect("settlement").is_none());
}

#[tokio::test]
async fn terminal_projection_rejects_wrong_owner_epoch_and_uncommitted_end_without_cache_erasure() {
    let fixture = Fixture::new().await;
    let route = fixture.ended("entry-node", unix_ms()).await;
    let settlement = fixture.begin_release(&route).await;
    let mut wrong = route.clone();
    wrong.owner_epoch += 1;
    assert!(
        !cleanup_reconciled_release(
            &fixture.state,
            &wrong,
            tokio::time::Instant::now() + TERMINAL_PROJECTION_SAFETY_WINDOW,
            ReleaseIntent::CLIENT,
        )
        .await,
        "local observation cannot clear a different owner generation"
    );
    assert_eq!(
        fixture.durable(&route).await.publication_ready_at_ms,
        route.publication_ready_at_ms
    );
    fixture.assert_terminal_cached(&route, false).await;
    assert!(settlement.result.lock().expect("settlement").is_none());

    let active = fixture.active("entry-node", unix_ms()).await;
    assert!(
        !persist_terminal_projection(
            &fixture.state,
            &active,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
        )
        .await,
        "an exact owner acknowledgement is insufficient before durable End"
    );
    assert!(
        fixture
            .state
            .media_sessions
            .complete_release_with_route(active.clone())
            .await
            .is_none(),
        "a live route cannot mint durable release proof"
    );
    let cached = fixture
        .state
        .media_sessions
        .cached_route(&active.session_id)
        .await
        .expect("active cache")
        .expect("active route");
    assert_eq!(cached.state, "active");
    assert_eq!(fixture.durable(&active).await.state, "active");
}
