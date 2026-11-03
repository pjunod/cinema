//! Public Live TV reads and admission failures use actual saved settings.
use super::*;
use axum::response::IntoResponse;
use plurx_core::{domain::User, store::keys};
use serde_json::{json, Value};

async fn fixture() -> (AppState, User) {
    let state = crate::http::tests::test_state_without_router();
    let user = state
        .store
        .create_user("live-viewer", "unused", true)
        .await
        .expect("viewer");
    (state, user)
}

async fn refused<T>(result: Result<T, ApiError>, status: StatusCode) -> Value {
    let error = match result {
        Ok(_) => panic!("request unexpectedly succeeded"),
        Err(error) => error,
    };
    let response = error.into_response();
    assert_eq!(response.status(), status);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("error body");
    serde_json::from_slice(&bytes).expect("error JSON")
}

#[tokio::test]
async fn saved_disabled_live_tv_refuses_public_reads_and_starts_without_creating_intents() {
    let (state, user) = fixture().await;
    let error = refused(
        channels(AuthUser(user.clone()), State(state.clone())).await,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(error["code"], "live_tv_disabled");
    assert_eq!(error["retry"], "never");
    let error = refused(
        guide_document(
            AuthUser(user.clone()),
            State(state.clone()),
            axum::extract::Query(GuideQuery {
                from: None,
                hours: None,
            }),
        )
        .await,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(error["code"], "live_tv_disabled");
    let error = refused(
        start_session(
            Path("7.1".into()),
            AuthUser(user.clone()),
            State(state.clone()),
            None,
        )
        .await,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(error["code"], "live_tv_disabled");
    let error = refused(
        issue_start(
            Path("7.1".into()),
            AuthUser(user.clone()),
            State(state.clone()),
            None,
        )
        .await,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(error["code"], "live_tv_disabled");
    let error = refused(
        resume_start(Path("a".repeat(32)), AuthUser(user), State(state.clone())).await,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(error["code"], "live_tv_disabled");
    assert!(state
        .live_tv
        .resource_snapshot(0, "")
        .await
        .expect("ledger")
        .records
        .is_empty());
}

#[tokio::test]
async fn malformed_recovery_ids_are_rejected_before_any_owner_or_resource_lookup() {
    let (state, user) = fixture().await;
    for id in ["", "a", "../../private", "0123456789ABCDEF0123456789ABCDEF"] {
        for error in [
            refused(
                retire_start(
                    Path(id.into()),
                    AuthUser(user.clone()),
                    State(state.clone()),
                )
                .await,
                StatusCode::BAD_REQUEST,
            )
            .await,
            refused(
                resume_start(
                    Path(id.into()),
                    AuthUser(user.clone()),
                    State(state.clone()),
                )
                .await,
                StatusCode::BAD_REQUEST,
            )
            .await,
            refused(
                start_state(
                    Path(id.into()),
                    AuthUser(user.clone()),
                    State(state.clone()),
                )
                .await,
                StatusCode::BAD_REQUEST,
            )
            .await,
        ] {
            assert_eq!(error["code"], "invalid_request");
        }
    }
    state
        .store
        .put_setting(keys::LIVE_TV_ENABLED, "1")
        .await
        .expect("enable");
    let error = refused(
        start_session(
            Path("7.1".into()),
            AuthUser(user),
            State(state.clone()),
            Some(Json(PublicLiveTvStart {
                playback: None,
                request_id: Some("../bad".into()),
            })),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(error["code"], "invalid_request");
    assert!(state
        .live_tv
        .resource_snapshot(0, "")
        .await
        .expect("ledger")
        .records
        .is_empty());
}

#[tokio::test]
async fn enabled_guide_without_a_source_is_an_unavailable_document_and_readiness_stays_advisory() {
    let (state, user) = fixture().await;
    state
        .store
        .put_setting(keys::LIVE_TV_ENABLED, "1")
        .await
        .expect("enable");
    let Json(guide) = guide_document(
        AuthUser(user.clone()),
        State(state.clone()),
        axum::extract::Query(GuideQuery {
            from: Some(10_000),
            hours: Some(0),
        }),
    )
    .await
    .expect("renderable guide");
    assert_eq!(guide.freshness, crate::live_tv::GuideFreshness::Unavailable);
    assert_eq!(
        guide.window,
        GuideWindow {
            start: 10_000,
            end: 13_600
        }
    );
    assert!(guide.channels.is_empty());
    let Json(advisory) = guide_readiness(AdminUser(user.clone()), State(state.clone()))
        .await
        .expect("guide advisory");
    assert!(advisory.advisory);
    assert_eq!(advisory.source, "off");
    assert_eq!(advisory.programmes, 0);
    assert!(
        !advisory
            .checks
            .iter()
            .find(|check| check.id == "guide_source")
            .expect("source check")
            .ready
    );
    assert!(
        advisory
            .checks
            .iter()
            .find(|check| check.id == "outbound_host")
            .expect("no outbound request")
            .ready
    );
    assert_eq!(
        state
            .store
            .get_setting(keys::LIVE_TV_ENABLED)
            .await
            .expect("saved switch")
            .as_deref(),
        Some("1")
    );
    let error = refused(
        channels(AuthUser(user), State(state)).await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(
        error["code"], "invalid_request",
        "LiveTvError::InvalidConfig uses the stable invalid_request wire code"
    );
    assert_eq!(error["retry"], "later");
    assert!(error["message"]
        .as_str()
        .expect("configuration diagnostic")
        .contains("HDHomeRun IPv4 address is required"));
}

#[tokio::test]
async fn cached_guide_reads_clip_the_window_and_generation_changes_discard_old_programmes() {
    let (state, user) = fixture().await;
    for (key, value) in [
        (keys::LIVE_TV_ENABLED, "1"),
        (keys::LIVE_TV_GUIDE_SOURCE, "hdhomerun"),
    ] {
        state.store.put_setting(key, value).await.expect("settings");
    }
    let start = crate::live_tv::unix_seconds();
    let programme = |at, title: &str| crate::live_tv::LiveTvProgramme {
        start: at,
        end: at + 1_800,
        title: title.into(),
        episode_title: None,
        episode: None,
        synopsis: None,
        image_url: None,
        original_air_date: None,
        series_id: None,
        programme_id: None,
        is_new: None,
        filters: Vec::new(),
    };
    state
        .live_tv
        .remember_relayed_guide(
            0,
            LiveTvGuide {
                source: "hdhomerun".into(),
                freshness: crate::live_tv::GuideFreshness::Fresh,
                age_seconds: 0,
                fetched_at: Some(start),
                window: GuideWindow {
                    start: start - 3_600,
                    end: start + 10_800,
                },
                refresh_error: None,
                next_refresh_at: Some(start + 600),
                matched_channels: 1,
                lineup_channels: 1,
                channels: vec![crate::live_tv::LiveTvGuideChannel {
                    id: "7.1".into(),
                    guide_number: "7.1".into(),
                    affiliate: None,
                    image_url: None,
                    programmes: vec![
                        programme(start - 1_800, "Past"),
                        programme(start, "Current"),
                        programme(start + 1_800, "Next"),
                        programme(start + 3_600, "Later"),
                    ],
                }],
            },
        )
        .await;
    let Json(clipped) = guide_document(
        AuthUser(user.clone()),
        State(state.clone()),
        axum::extract::Query(GuideQuery {
            from: Some(start),
            hours: Some(1),
        }),
    )
    .await
    .expect("cached guide");
    assert_eq!(clipped.freshness, crate::live_tv::GuideFreshness::Fresh);
    assert_eq!(
        clipped.channels[0]
            .programmes
            .iter()
            .map(|programme| programme.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Current", "Next"]
    );
    let Json(advisory) = guide_readiness(AdminUser(user.clone()), State(state.clone()))
        .await
        .expect("cached advisory");
    assert!(advisory.advisory);
    assert_eq!(advisory.programmes, 4);
    assert!(
        advisory
            .checks
            .iter()
            .find(|check| check.id == "guide_age")
            .expect("age")
            .ready
    );
    // Adopting cached guide bytes does not run the owner's refresh loop.
    // GuideCache::read takes scheduling evidence from its local loop state,
    // rather than trusting next_refresh_at carried inside the adopted bytes.
    let refresh = advisory
        .checks
        .iter()
        .find(|check| check.id == "guide_next_refresh")
        .expect("refresh scheduling observation");
    assert!(!refresh.ready);
    assert!(refresh.message.contains("has not completed a tick"));
    assert!(clipped.next_refresh_at.is_none());
    state
        .store
        .put_setting(keys::LIVE_TV_CONFIG_GENERATION, "1")
        .await
        .expect("new configuration");
    let Json(reconfigured) = guide_document(
        AuthUser(user),
        State(state),
        axum::extract::Query(GuideQuery {
            from: Some(start),
            hours: Some(1),
        }),
    )
    .await
    .expect("new generation document");
    assert_eq!(
        reconfigured.freshness,
        crate::live_tv::GuideFreshness::Unavailable
    );
    assert!(
        reconfigured.channels.is_empty(),
        "previous configuration cannot qualify a new guide"
    );
}

#[tokio::test]
async fn readiness_reports_missing_device_and_invalid_configuration_without_changing_saved_choices()
{
    let (state, user) = fixture().await;
    state
        .store
        .put_setting(keys::LIVE_TV_ENABLED, "1")
        .await
        .expect("enable");
    let Json(read) = readiness(AdminUser(user.clone()), State(state.clone()))
        .await
        .expect("readiness");
    assert!(!read.ready);
    assert!(read.enabled);
    assert!(read.snapshot.is_none());
    assert!(
        !read
            .checks
            .iter()
            .find(|check| check.id == "owner_network")
            .expect("device failure")
            .ready
    );
    assert!(
        !read
            .checks
            .iter()
            .find(|check| check.id == "worker_observations")
            .expect("worker failure")
            .ready
    );
    let Json(refreshed) = refresh_readiness(AdminUser(user.clone()), State(state.clone()))
        .await
        .expect("explicit readiness refresh");
    assert!(!refreshed.ready && refreshed.snapshot.is_none());
    state
        .store
        .put_setting(keys::LIVE_TV_MAX_SESSIONS, "0")
        .await
        .expect("invalid saved configuration");
    let Json(invalid) = readiness(AdminUser(user.clone()), State(state.clone()))
        .await
        .expect("invalid configuration remains inspectable");
    assert!(
        !invalid
            .checks
            .iter()
            .find(|check| check.id == "configuration")
            .expect("configuration error")
            .ready
    );
    assert!(
        !invalid
            .checks
            .iter()
            .any(|check| check.id == "owner_network"),
        "invalid configuration stops before the final owner exchange"
    );
    assert_eq!(
        state
            .store
            .get_setting(keys::LIVE_TV_ENABLED)
            .await
            .expect("saved switch")
            .as_deref(),
        Some("1")
    );
    state
        .store
        .put_setting(keys::LIVE_TV_GUIDE_SOURCE, "xmltv")
        .await
        .expect("XMLTV selected");
    let Json(guide) = guide_readiness(AdminUser(user), State(state))
        .await
        .expect("XMLTV readiness");
    assert_eq!(guide.source, "xmltv");
    assert!(
        !guide
            .checks
            .iter()
            .find(|check| check.id == "outbound_host")
            .expect("URL diagnostic")
            .ready
    );
    assert_eq!(
        serde_json::to_value(&guide).expect("advisory JSON")["advisory"],
        json!(true)
    );
}
