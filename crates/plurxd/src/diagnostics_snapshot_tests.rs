//! Real SQLite-backed diagnostic snapshot ownership and retention regressions.
use super::*;
use crate::state::{AppState, Dirs};
use plurx_core::{
    domain::MediaSessionActivation, playback_principal::PlaybackPrincipal, store::SqliteStore,
};
use serde_json::{json, Value};

struct Fixture {
    _root: tempfile::TempDir,
    state: AppState,
    user: User,
    other: User,
    admin: User,
}
impl Fixture {
    async fn new() -> Self {
        let root = crate::test_tempdir().expect("diagnostic fixture");
        let store = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
        use plurx_core::store::UserStore;
        let user = store
            .create_user("snapshot-viewer", "first-password-generation", false)
            .await
            .expect("viewer");
        let other = store
            .create_user("snapshot-other", "other-generation", false)
            .await
            .expect("other");
        let admin = store
            .create_user("snapshot-admin", "admin-generation", true)
            .await
            .expect("admin");
        let dirs = Dirs {
            artwork: root.path().join("artwork"),
            transcode: root.path().join("transcode"),
            cache: root.path().join("cache"),
            subs: root.path().join("subs"),
            runtime_cache: root.path().join("runtime"),
            renditions: root.path().join("renditions"),
        };
        for path in [
            &dirs.artwork,
            &dirs.transcode,
            &dirs.cache,
            &dirs.subs,
            &dirs.runtime_cache,
            &dirs.renditions,
        ] {
            std::fs::create_dir(path).expect("owned directory");
        }
        let state = AppState::new_unhooked(
            "diagnostics".into(),
            store,
            dirs,
            "diagnostic-node".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(16)),
        );
        Self {
            _root: root,
            state,
            user,
            other,
            admin,
        }
    }
    fn recorder(&self) -> Arc<Recorder> {
        crate::telemetry::diagnostics(Arc::clone(&self.state.store))
    }
    fn event(&self, user: &User, event: &str) {
        self.recorder().observe(&PlaybackEvent {
            user_id: Some(user.id),
            at_unix_ms: unix_ms(),
            event: event.into(),
            ms: Some(23),
            detail: Some("PRIVATE_PATH_AND_TOKEN_CANARY".into()),
            ..Default::default()
        });
    }
    async fn route(&self, owner: &str) -> MediaSessionRoute {
        let now = unix_ms();
        let activation = MediaSessionActivation {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            principal: PlaybackPrincipal::LocalUser {
                user_id: self.user.id,
            },
            playback_id: uuid::Uuid::new_v4().to_string(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: owner.into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60_000,
            expected_desired_revision: None,
        };
        self.state
            .store
            .activate_media_session(&activation)
            .await
            .expect("durable route")
            .expect("new activation");
        self.state
            .store
            .settle_media_session_activation(
                &activation,
                plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                now,
            )
            .await
            .expect("confirm exact owner")
            .expect("confirmed route")
    }
    async fn start(&self, user: &User, body: Value) -> Result<Response, StatusCode> {
        create(
            AuthUser(user.clone()),
            State(self.state.clone()),
            Bytes::from(serde_json::to_vec(&body).expect("request")),
        )
        .await
    }
    async fn finish(&self) {
        self.state.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self
                    .recorder()
                    .jobs
                    .lock()
                    .expect("jobs")
                    .entries
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("snapshot expiry owners observe shutdown");
    }
}
async fn document(response: Response) -> Value {
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    let bytes = axum::body::to_bytes(response.into_body(), JOB_BYTES + 4096)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("response JSON")
}
fn request() -> Value {
    json!({"incident_at_ms":unix_ms(),"window_ms":1000})
}
async fn status(result: Result<Response, StatusCode>, expected: StatusCode) {
    match result {
        Err(actual) => assert_eq!(actual, expected),
        Ok(_) => panic!("expected refusal {expected}"),
    }
}
async fn complete(f: &Fixture, user: &User, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = document(
                read(
                    AuthUser(user.clone()),
                    State(f.state.clone()),
                    Path(id.to_owned()),
                )
                .await
                .expect("owned snapshot"),
            )
            .await;
            if value["state"] != "pending" {
                return value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("snapshot collection completes")
}

#[tokio::test]
async fn snapshots_collect_only_the_owner_and_delete_retires_the_owned_job() {
    let f = Fixture::new().await;
    f.event(&f.user, "stall");
    f.event(&f.other, "control_terminal");
    let capability = document(capabilities(State(f.state.clone())).await).await;
    assert_eq!(capability["snapshots"], true);
    assert_eq!(capability["max_window_ms"], 600_000);
    let response = f.start(&f.user, request()).await.expect("accepted");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let id = document(response).await["id"]
        .as_str()
        .expect("job id")
        .to_owned();
    let captured = complete(&f, &f.user, &id).await;
    assert_eq!(captured["state"], "complete");
    assert_eq!(captured["events"].as_array().expect("events").len(), 1);
    assert_eq!(captured["events"][0]["facts"]["state"], "stalled");
    assert!(!captured.to_string().contains("CANARY"));
    assert_eq!(captured["components"][1]["reason"], "not_requested");
    status(
        read(
            AuthUser(f.other.clone()),
            State(f.state.clone()),
            Path(id.clone()),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    status(
        erase(
            AuthUser(f.other.clone()),
            State(f.state.clone()),
            Path(id.clone()),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    let mut new_generation = f.user.clone();
    new_generation.password_hash = "changed-password".into();
    status(
        read(
            AuthUser(new_generation),
            State(f.state.clone()),
            Path(id.clone()),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    let job = owned_job(&f.recorder(), &f.user, &id).expect("retained exact owner");
    assert_eq!(
        erase(
            AuthUser(f.user.clone()),
            State(f.state.clone()),
            Path(id.clone())
        )
        .await
        .expect("delete")
        .status(),
        StatusCode::NO_CONTENT
    );
    assert!(job.cancel.is_cancelled());
    status(
        read(AuthUser(f.user.clone()), State(f.state.clone()), Path(id)).await,
        StatusCode::NOT_FOUND,
    )
    .await;
    f.finish().await;
}

#[tokio::test]
async fn snapshots_validate_bounds_strict_json_and_admin_system_authority() {
    let f = Fixture::new().await;
    for body in [
        json!({"incident_at_ms":unix_ms(),"window_ms":600001}),
        json!({"incident_at_ms":unix_ms()+180000,"window_ms":1}),
        json!({"incident_at_ms":unix_ms()-1000000,"window_ms":1}),
        json!({"incident_at_ms":unix_ms(),"window_ms":1,"correlation":"arbitrary"}),
        json!({"incident_at_ms":unix_ms(),"window_ms":1,"unknown":true}),
    ] {
        status(f.start(&f.user, body).await, StatusCode::BAD_REQUEST).await;
    }
    status(
        create(
            AuthUser(f.user.clone()),
            State(f.state.clone()),
            Bytes::from_static(br#"{"incident_at_ms":1,"incident_at_ms":2,"window_ms":0}"#),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    let mut body = request();
    body["include_system"] = true.into();
    status(f.start(&f.user, body.clone()).await, StatusCode::FORBIDDEN).await;
    let id = document(f.start(&f.admin, body).await.expect("admin request")).await["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let value = complete(&f, &f.admin, &id).await;
    let events = value["events"].as_array().expect("system events");
    assert!(events
        .iter()
        .any(|e| e["domain"] == "coverage" && e["facts"]["phase"] == "readiness"));
    assert!(!value.to_string().contains("password"));
    f.finish().await;
}

#[tokio::test]
async fn correlation_requires_durable_route_receipt_and_missing_workers_are_explicit() {
    let f = Fixture::new().await;
    let route = f.route("missing-worker").await;
    let correlation = crate::transcode::session_log_id(&route.session_id);
    let recorder = f.recorder();
    recorder.observe(&PlaybackEvent {
        user_id: Some(f.user.id),
        at_unix_ms: unix_ms(),
        session_id: Some(route.session_id.clone()),
        event: "session_start".into(),
        ..Default::default()
    });
    assert_eq!(
        recorder
            .receipt_nodes(f.user.id, Some(&correlation))
            .expect_err("beacon is not ownership"),
        StatusCode::NOT_FOUND
    );
    recorder.route(&route);
    recorder.route(&route);
    assert_eq!(
        recorder.history.lock().expect("history").receipts.len(),
        1,
        "repeated durable observation is deduplicated"
    );
    assert!(recorder
        .receipt_nodes(f.other.id, Some(&correlation))
        .is_err());
    let mut body = request();
    body["correlation"] = correlation.clone().into();
    let id = document(f.start(&f.user, body).await.expect("correlated request")).await["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let value = complete(&f, &f.user, &id).await;
    assert_eq!(value["state"], "partial");
    assert_eq!(value["components"][1]["reason"], "peer_unavailable");
    assert_eq!(value["events"][0]["facts"]["correlation"], correlation);
    let mut unknown = request();
    unknown["correlation"] = format!("s-{}", "0".repeat(64)).into();
    let id = document(
        f.start(&f.user, unknown)
            .await
            .expect("asynchronous unavailable context"),
    )
    .await["id"]
        .as_str()
        .expect("id")
        .to_owned();
    assert_eq!(
        complete(&f, &f.user, &id).await["components"][0]["reason"],
        "context_unavailable"
    );
    f.finish().await;
}

#[tokio::test]
async fn snapshot_quotas_are_per_user_and_expired_jobs_cancel_without_releasing_new_ones() {
    let f = Fixture::new().await;
    for _ in 0..2 {
        f.start(&f.user, request())
            .await
            .expect("two simultaneous jobs");
    }
    status(
        f.start(&f.user, request()).await,
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    let recorder = f.recorder();
    let ids: Vec<_> = recorder
        .jobs
        .lock()
        .expect("jobs")
        .entries
        .keys()
        .cloned()
        .collect();
    for id in ids {
        erase(AuthUser(f.user.clone()), State(f.state.clone()), Path(id))
            .await
            .expect("retire");
    }
    let response = f
        .start(&f.user, request())
        .await
        .expect("third start in five minutes");
    let id = document(response).await["id"]
        .as_str()
        .expect("id")
        .to_owned();
    erase(AuthUser(f.user.clone()), State(f.state.clone()), Path(id))
        .await
        .expect("retire third");
    status(
        f.start(&f.user, request()).await,
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    f.start(&f.other, request())
        .await
        .expect("another viewer has its own quota");
    let expired = Arc::new(Job {
        user: f.user.id,
        generation: generation(&f.user),
        created: Instant::now() - JOB_TTL - Duration::from_secs(1),
        cancel: CancellationToken::new(),
        result: Mutex::new(None),
    });
    let token = expired.cancel.clone();
    let mut jobs = Jobs {
        entries: HashMap::from([("expired".into(), expired)]),
        starts: VecDeque::from([
            (f.user.id, Instant::now() - Duration::from_secs(301)),
            (f.other.id, Instant::now()),
        ]),
    };
    Recorder::cleanup(&mut jobs);
    assert!(jobs.entries.is_empty());
    assert!(token.is_cancelled());
    assert_eq!(jobs.starts.len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn local_projection_prunes_age_and_prefers_important_events_under_pressure() {
    let f = Fixture::new().await;
    let recorder = f.recorder();
    recorder.observe(&PlaybackEvent {
        user_id: Some(f.user.id),
        at_unix_ms: unix_ms() - 700000,
        event: "ttff".into(),
        ..Default::default()
    });
    f.event(&f.user, "control_terminal");
    assert_eq!(
        recorder.history.lock().expect("history").rows.len(),
        1,
        "expired event removed before insertion"
    );
    for _ in 0..9000 {
        f.event(&f.user, "ttff");
    }
    let scope = PeerScope {
        user_id: f.user.id,
        generation: generation(&f.user),
        since_ms: unix_ms() - 1000,
        until_ms: unix_ms() + 1000,
        correlation: None,
    };
    {
        let h = recorder.history.lock().expect("history");
        assert!(h.bytes <= HISTORY_BYTES);
        assert!(h.rows.len() <= 8000);
        assert!(h.rows.iter().any(|r| r.important));
        assert!(h.losses[&f.user.id] > 0);
    }
    let projection = recorder.local(&scope).expect("bounded local projection");
    assert!(projection.events.len() <= 2000);
    assert_ne!(projection.dropped_events, "0");
    let before = recorder.dropped.load(Ordering::Relaxed);
    recorder.observe(&PlaybackEvent {
        user_id: None,
        ..Default::default()
    });
    assert_eq!(
        recorder.dropped.load(Ordering::Relaxed),
        before,
        "unattributed events never enter history"
    );
    f.finish().await;
}

#[tokio::test]
async fn peer_snapshot_refuses_unsigned_requests_before_exposing_local_history() {
    let f = Fixture::new().await;
    f.event(&f.user, "stall");
    status(
        peer(
            State(f.state.clone()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert!(eligible(&Method::GET, "/api/v1/diagnostics/capabilities"));
    assert!(!eligible(&Method::PUT, "/api/v1/diagnostics/snapshots"));
    f.finish().await;
}
