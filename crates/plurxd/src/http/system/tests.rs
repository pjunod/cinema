#[test]
fn client_log_file_id_accepts_exact_decimal_text_and_legacy_numbers() {
    for (value, expected) in [
        (serde_json::json!(4), Some(4)),
        (serde_json::json!("4"), Some(4)),
        (
            serde_json::json!("9007199254740993"),
            Some(9_007_199_254_740_993),
        ),
        (serde_json::json!(i64::MAX.to_string()), Some(i64::MAX)),
        (serde_json::Value::Null, None),
    ] {
        let report: super::ClientLog =
            serde_json::from_value(serde_json::json!({"event":"ttff","file_id":value}))
                .expect("compatible file ID");
        assert_eq!(report.file_id, expected);
    }
    let absent: super::ClientLog =
        serde_json::from_value(serde_json::json!({"event":"ttff"})).expect("absent file ID");
    assert_eq!(absent.file_id, None);
    for invalid in [
        serde_json::json!(4.5),
        serde_json::json!("4.5"),
        serde_json::json!("9223372036854775808"),
        serde_json::json!({}),
        serde_json::json!([]),
    ] {
        assert!(
            serde_json::from_value::<super::ClientLog>(serde_json::json!({"file_id":invalid}))
                .is_err()
        );
    }
}

use std::time::{Duration, Instant};

use super::*;

#[tokio::test]
async fn invitation_setting_saves_without_provider_or_remote_readiness() {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    let (app, state) = super::super::tests::test_app_with_state();
    let admin = state
        .store
        .create_user("invitation-settings-admin", "hash", true)
        .await
        .expect("admin");
    let token = plurx_core::auth::generate_token().expect("synthetic token");
    state
        .store
        .create_token(&plurx_core::auth::hash_token(&token), admin.id, None)
        .await
        .expect("login");
    for enabled in [true, false, true] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/settings")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"cinema_remote_invitations":enabled}).to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let value: serde_json::Value = serde_json::from_slice(
            &to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("body"),
        )
        .expect("settings");
        assert_eq!(value["cinema_remote_invitations"], enabled);
        assert_eq!(value["cinema_remote_control"], false);
        assert_eq!(
            state
                .store
                .get_setting("cinema.remote_invitations")
                .await
                .expect("stored choice")
                .as_deref(),
            Some(if enabled { "1" } else { "0" })
        );
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/developer/readiness")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("readiness");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let value: serde_json::Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body"),
    )
    .expect("readiness JSON");
    let item = value["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["id"] == "cinema_remote_invitations")
        .expect("invitation advisory");
    assert_eq!(item["enabled"], true);
    assert_eq!(item["setting"], "cinema_remote_invitations");
    let ids = item["requirements"]
        .as_array()
        .expect("requirements")
        .iter()
        .map(|r| r["id"].as_str().expect("ID"))
        .collect::<Vec<_>>();
    assert_eq!(ids, ["broker", "consent", "delivery"]);
}

#[test]
fn display_mode_setting_is_an_independent_advisory_switch() {
    let request: UpdateSettings = serde_json::from_value(serde_json::json!({
        "playback_display_mode_match": true
    }))
    .expect("display-mode setting");
    assert_eq!(request.playback_display_mode_match, Some(true));
    assert!(request.has_non_live_tv_update());
}

#[test]
fn partial_coverage_preserves_legacy_settings_facts_and_reports_the_enabled_policy() {
    use plurx_core::transcode::{ArtifactQualification, DecodeBackend};

    let readiness = crate::transcode::ArtifactQualificationReadiness {
        requested: true,
        measured_build: Some("ffmpeg version 9.0.1".to_owned()),
        measured_decoders: vec![
            (
                "h264".to_owned(),
                DecodeBackend::Software,
                "h264".to_owned(),
            ),
            (
                "h264".to_owned(),
                DecodeBackend::VideoToolbox,
                "h264".to_owned(),
            ),
        ],
        covered_decoders: vec![(
            "h264".to_owned(),
            DecodeBackend::VideoToolbox,
            "h264".to_owned(),
        )],
        effective: ArtifactQualification::Unqualified,
        refusal: Some(crate::transcode::QualificationRefusal::IncompleteCoverage),
    };

    let wire = serde_json::to_value(DecoderHealthQualification::of(&readiness, true))
        .expect("serialize settings qualification");
    assert_eq!(
        wire["namespace"],
        plurx_core::transcode::UNQUALIFIED_ARTIFACT_NAMESPACE,
        "the legacy field must not claim one namespace for a mixed-path node"
    );
    assert_eq!(wire["enforcing"], false);
    assert_eq!(wire["policy_enabled"], true);
    assert_eq!(
        wire["requested_namespace"],
        plurx_core::transcode::HEALTH_QUALIFIED_ARTIFACT_NAMESPACE
    );
    assert_eq!(wire["path_scoped"], true);
    assert_eq!(wire["covered_decoders"], serde_json::json!([]));
    assert_eq!(
        wire["measured_decoders"],
        serde_json::json!(["h264/h264"]),
        "the v1 list remains the original software-only codec/decoder pair"
    );
    assert_eq!(
        wire["measured_paths_v2"],
        serde_json::json!(["h264/software/h264", "h264/videotoolbox/h264"])
    );
    assert_eq!(
        wire["covered_paths_v2"],
        serde_json::json!(["h264/videotoolbox/h264"])
    );

    let legacy_wire = serde_json::json!({
        "namespace": plurx_core::transcode::UNQUALIFIED_ARTIFACT_NAMESPACE,
        "enforcing": false,
        "eligible": true,
        "measured_build": "ffmpeg version 9.0.1",
        "measured_decoders": ["h264/h264"],
        "covered_decoders": ["h264/h264"],
        "refusal": "not_requested",
        "explanation": "Not requested on this node.",
        "pending_restart": false
    });
    let legacy: DecoderHealthQualification =
        serde_json::from_value(legacy_wire).expect("deserialize legacy settings response");
    assert!(!legacy.policy_enabled);
    assert_eq!(
        legacy.requested_namespace,
        plurx_core::transcode::UNQUALIFIED_ARTIFACT_NAMESPACE
    );
    assert!(!legacy.path_scoped);
    assert!(legacy.measured_paths_v2.is_empty());
    assert!(legacy.covered_paths_v2.is_empty());
}

fn test_delivery(method: &'static str, started_unix: i64) -> Delivery {
    Delivery {
        vod_observation: None,
        method,
        presentation: Some("live-recovery"),
        user: "paul".to_owned(),
        file_id: started_unix,
        item_id: started_unix + 100,
        title: format!("Title {started_unix}"),
        started_unix,
        idle_seconds: 2,
        session_id: Some(format!("session-{started_unix}")),
        delivered_bytes: Some(4_096),
        delivered_bps: Some(8_000),
    }
}

#[test]
fn shipped_marker_prewarm_placeholder_is_correlatable_without_a_session_id() {
    let vod = PlaybackEvent {
        event: "marker_prewarm".to_owned(),
        detail: Some("miss".to_owned()),
        file_id: Some(7),
        method: Some("remux".to_owned()),
        ..PlaybackEvent::default()
    };
    assert_eq!(vod.session_id, None, "this is the shipped payload shape");
    assert_eq!(vod_marker_prewarm_placeholder(&vod), Some((7, "remux")));

    let direct = PlaybackEvent {
        method: Some("direct_play".to_owned()),
        ..vod.clone()
    };
    assert!(
        vod_marker_prewarm_placeholder(&direct).is_none(),
        "direct play keeps emitting its client-owned miss"
    );
    let android_direct = PlaybackEvent {
        method: Some("direct".to_owned()),
        ..direct
    };
    assert!(
        vod_marker_prewarm_placeholder(&android_direct).is_none(),
        "the Android legacy direct label also keeps its client-owned miss"
    );
    let mut supplied_hit = beacon("marker_prewarm", 0);
    supplied_hit.file_id = Some(7);
    supplied_hit.detail = Some("hit".to_owned());
    supplied_hit.method = Some("direct_play".to_owned());
    let normalized_direct = client_playback_event(&supplied_hit, 11);
    assert_eq!(normalized_direct.detail.as_deref(), Some("miss"));
    assert!(
        vod_marker_prewarm_placeholder(&normalized_direct).is_none(),
        "client-supplied direct-play hits stay client-owned misses"
    );

    supplied_hit.method = Some("remux".to_owned());
    let normalized_vod = client_playback_event(&supplied_hit, 11);
    assert_eq!(normalized_vod.detail.as_deref(), Some("miss"));
    assert_eq!(
        vod_marker_prewarm_placeholder(&normalized_vod),
        Some((7, "remux")),
        "only server bookkeeping may upgrade a normalized client miss"
    );

    let streams = crate::progressive::Streams::new();
    let (_stream, guard) = streams.register("progressive", 11, "paul", 7, 70, 1.0);
    assert!(
        streams.contains_delivery(11, 7),
        "a concurrent progressive remux blocks sessionless VOD attribution"
    );
    drop(guard);
    assert!(
        streams.contains_delivery(11, 7),
        "a delayed fire-and-forget beacon stays ambiguous after deregistration"
    );
    streams.expire_marker_ambiguities();
    assert!(!streams.contains_delivery(11, 7));
}

#[test]
fn clustered_activity_attributes_answered_rows_and_names_missing_nodes() {
    let peers = PeerActivityRead::Peers(
        vec![
            (
                "node-b".to_owned(),
                PeerActivityOutcome::Answered(crate::http::internal_activity::ActivitySnapshot {
                    workers: None,
                    node_id: "node-b".to_owned(),
                    deliveries: vec![ActivityDelivery {
                        vod_observation: None,
                        method: "direct".to_owned(),
                        presentation: None,
                        user: "viewer".to_owned(),
                        file_id: 2,
                        item_id: 102,
                        title: "Remote title".to_owned(),
                        started_unix: 2,
                        idle_seconds: 1,
                        delivered_bytes: None,
                        delivered_bps: None,
                    }],
                    analysis: Vec::new(),
                    live_tv: vec![crate::live_tv::LiveTvActivity {
                        channel_number: "7.1".into(),
                        channel_name: "Remote TV".into(),
                        user: "viewer".into(),
                        owner_node_id: "node-b".into(),
                        encoder: "software".into(),
                        age_seconds: 1,
                        output_height: 720,
                        state: "active".into(),
                        programme_title: Some("City Beat".into()),
                        delivery: None,
                    }],
                    dvr: None,
                }),
            ),
            ("node-c".to_owned(), PeerActivityOutcome::TimedOut),
            ("node-d".to_owned(), PeerActivityOutcome::Unhealthy),
            ("node-e".to_owned(), PeerActivityOutcome::Unsupported),
        ]
        .into(),
    );

    let rows = serde_json::to_value(clustered_deliveries(
        "node-a",
        vec![test_delivery("transcode", 1)],
        &peers,
    ))
    .expect("cluster deliveries serialize");
    let live_tv = clustered_live_tv(Vec::new(), &peers);
    assert_eq!(live_tv.len(), 1);
    assert_eq!(live_tv[0].owner_node_id, "node-b");
    assert_eq!(live_tv[0].channel_name, "Remote TV");
    let rows = rows.as_array().expect("delivery array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["node_id"], "node-b");
    assert_eq!(rows[0]["method"], "direct");
    assert!(rows[0]["session_id"].is_null());
    assert_eq!(rows[1]["node_id"], "node-a");
    assert_eq!(rows[1]["session_id"], "session-1");

    let nodes =
        serde_json::to_value(activity_nodes("node-a", &peers)).expect("node status serializes");
    assert_eq!(nodes[0]["status"], "answered");
    assert_eq!(nodes[1]["node_id"], "node-b");
    assert_eq!(nodes[1]["status"], "answered");
    assert_eq!(nodes[2]["status"], "timed_out");
    assert_eq!(nodes[3]["status"], "unhealthy");
    assert_eq!(nodes[4]["status"], "unsupported");

    let missing = missing_activity_summary(&peers).expect("missing-node summary");
    assert!(missing.contains("node-c (timed_out)"), "{missing}");
    assert!(missing.contains("node-d (unhealthy)"), "{missing}");
    assert!(missing.contains("node-e (unsupported)"), "{missing}");
    assert!(missing.ends_with("did not answer"), "{missing}");
}

#[test]
fn failed_peer_directory_is_visible_without_exposing_an_address() {
    let peers = PeerActivityRead::DirectoryUnavailable;
    let nodes =
        serde_json::to_value(activity_nodes("node-a", &peers)).expect("node status serializes");
    assert_eq!(nodes[1]["status"], "unavailable");
    assert!(nodes[1].get("node_id").is_none());
    let summary = missing_activity_summary(&peers).expect("directory failure summary");
    assert_eq!(summary, "cluster peer directory did not answer");
    assert!(!summary.contains("http"));
}

#[test]
fn snapshot_histogram_labels_are_derived_from_every_exported_bound() {
    let labels = DB_SNAPSHOT_HISTOGRAM_BOUNDS_NANOS.map(prometheus_seconds_label);
    assert_eq!(
        labels,
        [
            "0.001", "0.0025", "0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1", "5",
            "10", "30", "60", "120", "300",
        ]
    );
}

#[test]
fn snapshot_storage_metrics_are_passive_fixed_label_and_unknown_is_absent() {
    let zero = plurx_core::cluster::migration::status::DbSnapshotHistogram {
        count: 0,
        sum_nanos: 0,
        cumulative_buckets: [0; 16],
    };
    let mut sample = DbSnapshotMetricsSnapshot {
        storage_deferrals_total: 7,
        required_storage_bytes: Some(123456),
        build_ok: zero,
        build_error: zero,
        install_ok: zero,
        install_error: zero,
        last_build: None,
        last_install: None,
    };
    let mut output = String::new();
    render_snapshot_metrics(&mut output, sample);
    assert!(output.contains("plurx_raft_snapshot_deferrals_total{reason=\"storage\"} 7\n"));
    assert!(output.contains("plurx_raft_snapshot_required_storage_bytes 123456\n"));
    assert!(!output.contains("node_id="));
    sample.required_storage_bytes = None;
    output.clear();
    render_snapshot_metrics(&mut output, sample);
    assert!(!output.contains("plurx_raft_snapshot_required_storage_bytes"));
    assert!(output.contains("plurx_raft_snapshot_deferrals_total{reason=\"storage\"} 7\n"));
}

/// Every route that reads the roster's machine names, enumerated.
///
/// `node_hostnames` is `pub(super)`, so any of the modules under
/// `http/` can call it, and the argument that says whether the caller may
/// see the names is a plain `bool` the callee cannot check. A new caller
/// passing `true` from a household route compiles, passes every other test
/// in this file, and hands the fleet's machine names to anyone signed in.
///
/// So the call sites are a closed set. Adding one is allowed; adding one
/// without saying so here is not.
#[test]
fn the_roster_reader_has_exactly_these_callers() {
    let http = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/http");
    // Every module under `http/`, child modules included: a split moves a
    // route into `http/<parent>/<child>.rs`, and a scan of the top level
    // alone would stop seeing it without failing. Test children
    // (`tests.rs`, `tests/`) quote call sites as literals and are skipped
    // for the same reason the inline test module is split off below.
    let mut modules = Vec::new();
    let mut directories = vec![http.clone()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).expect("an http module directory") {
            let path = entry.expect("a directory entry").path();
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            if stem == Some("tests") {
                continue;
            }
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                modules.push(path);
            }
        }
    }
    assert!(
        modules
            .iter()
            .any(|path| path.parent() != Some(http.as_path())),
        "the walk no longer reaches the child modules under http/"
    );
    let mut found: Vec<(String, String)> = Vec::new();
    for path in modules {
        let name = path
            .strip_prefix(&http)
            .expect("a module under http/")
            .to_str()
            .expect("a UTF-8 module path")
            .replace('\\', "/");
        let source = std::fs::read_to_string(&path).expect("a readable module");
        // Production halves only: the test modules quote these call sites
        // as string literals, and a test is not a route. Split on the test
        // *module*, not on the attribute — it also sits on ordinary items.
        let source = source
            .split_once("\n#[cfg(test)]\nmod tests {")
            .map_or(source.as_str(), |(production, _)| production);
        for (at, _) in source.match_indices("node_hostnames(&state,") {
            let argument = source[at..]
                .split_once(',')
                .expect("an argument")
                .1
                .split_once(')')
                .expect("a closing paren")
                .0
                .trim()
                .to_owned();
            found.push((name.clone(), argument));
        }
    }
    found.sort();
    assert_eq!(
        found,
        vec![
            // Admin-only route: `AdminUser` was extracted before the body
            // ran, so `true` is that proof handed on.
            ("analysis.rs".to_owned(), "true".to_owned()),
            // Reconciliation also requires AdminUser before previewing
            // candidate target names from the roster.
            ("analysis_reconcile.rs".to_owned(), "true".to_owned()),
            // Any signed-in household member reaches this one, so the
            // permission is this reader's own admin flag.
            ("system.rs".to_owned(), "user.0.is_admin".to_owned()),
        ],
        "a route reads the roster's machine names without this test knowing"
    );
}

#[test]
fn a_roster_read_costs_the_page_its_labels_and_never_the_page() {
    let source = include_str!("../system.rs");
    let reader = source
        .split_once("pub(super) async fn node_hostnames(")
        .expect("the roster reader")
        .1
        .split_once("\nasync fn peer_activity(")
        .expect("function after node_hostnames")
        .0;
    // A household member is refused before the roster is touched at all.
    let refused = reader.find("if !permitted {").expect("the admin gate");
    let read = reader
        .find("state.membership.node_hostnames().await")
        .expect("the roster read");
    assert!(refused < read);
    // No `?`: a roster that will not answer must not turn the page into an
    // error, and must not be reported as an empty roster either.
    assert!(!reader.contains("node_hostnames().await?"));
    assert!(reader.contains("Err(error) =>"));
    assert!(reader.contains("tracing::warn!"));
    // The cheap accessor, not the full membership status.
    assert!(!reader.contains("membership.status()"));
}

#[test]
fn machine_names_reach_the_activity_page_only_for_an_admin() {
    let source = include_str!("../system.rs");
    let handler = source
        .split_once("pub async fn activity_detail(")
        .expect("activity handler")
        .1
        .split_once("\n/// DELETE /api/v1/activity/producer")
        .expect("handler after activity_detail")
        .0;
    // `GET /api/v1/cluster/nodes` is `AdminUser`. This page is `AuthUser`,
    // so an ungated map here would make the activity page the one place an
    // ordinary household member can read the fleet's hostnames.
    //
    // Asserted as "every use of the map value", not "the text near the
    // publication I happen to know about". An earlier form of this test
    // found the publication by its key and checked the branch above it,
    // and a second publication under a *different* key from an ungated
    // `if clustered {` satisfied it while leaking to every signed-in
    // household member.
    let names = handler
        .match_indices("hostnames")
        // `node_hostnames` is the reader's name, not the value's.
        .filter(|(at, _)| !handler[..*at].ends_with("node_"))
        .map(|(at, _)| at)
        .collect::<Vec<_>>();
    let bound = names.first().copied().expect("the map is read at all");
    let gate = handler
        .find("if clustered && user.0.is_admin {")
        .expect("machine names are gated on the admin the roster is gated on");
    for at in &names {
        assert!(
            *at <= bound || handler[..*at].ends_with("(local, peers, ") || *at > gate,
            "the roster map is used outside the admin branch at byte {at}: {}",
            &handler[at.saturating_sub(80)..*at + 40]
        );
    }
    // …and the one use past the gate is the publication itself.
    assert_eq!(
        handler[gate..]
            .matches("response[\"node_hostnames\"] = serde_json::to_value(&hostnames)")
            .count(),
        1
    );
    // The reader itself is asked for the map exactly once, with this
    // reader's own admin flag and not a literal.
    assert_eq!(
        handler
            .matches("node_hostnames(&state, user.0.is_admin)")
            .count(),
        1
    );
    assert!(!handler.contains("node_hostnames(&state, true)"));
}

#[test]
fn prometheus_scrape_has_no_store_operation() {
    let source = include_str!("../system.rs");
    let handler = source
        .split_once("pub(crate) async fn metrics")
        .expect("metrics handler")
        .1
        .split_once("#[cfg(test)]")
        .expect("metrics test module")
        .0;
    let compact = handler
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(compact.starts_with("(State(state):State<MetricsState>"));
    assert!(!compact.contains("AppState"));
    assert!(!compact.contains(".store."));
    assert!(!compact.contains("Store>"));
    assert!(!compact.contains("Client"));
    assert!(!compact.contains("ReplicationMonitor"));
    assert!(!compact.contains("LocalDbRaft"));
    assert!(compact.contains("prometheus_cluster_activity_authority()"));
    assert!(compact.contains("prometheus_cluster_activity()"));

    let substate = source
        .split_once("pub(crate) struct MetricsState")
        .expect("metrics substate")
        .1
        .split_once("impl FromRef<AppState> for MetricsState")
        .expect("metrics substate conversion")
        .0;
    assert!(!substate.contains("Manager"));
    assert!(!substate.contains("Arc<dyn Store>"));
    assert!(!substate.contains("Client"));
    assert!(!substate.contains("ReplicationMonitor"));
    assert!(!substate.contains("LocalDbRaft"));
}

#[test]
fn absent_and_stale_store_samples_are_explicit_in_exposition() {
    let absent = render_store_metrics(StoreMetricsView {
        sample: None,
        age_seconds: None,
        valid: false,
        errors: 2,
        queue_health: None,
    });
    assert!(absent.contains("plurx_store_metrics_sample_valid 0"));
    assert!(absent.contains("plurx_store_metrics_sample_errors_total 2"));
    assert!(!absent.contains("plurx_store_metrics_sample_age_seconds "));
    assert!(!absent.contains("plurx_libraries_total"));

    let stale = render_store_metrics(StoreMetricsView {
        sample: Some(plurx_core::store::PrometheusStoreSnapshot {
            libraries: 3,
            users: 4,
            ..Default::default()
        }),
        age_seconds: Some(121),
        valid: false,
        errors: 3,
        queue_health: None,
    });
    assert!(stale.contains("plurx_store_metrics_sample_valid 0"));
    assert!(stale.contains("plurx_store_metrics_sample_age_seconds 121"));
    assert!(stale.contains("plurx_background_jobs{kind=\"transcode_prepare\",state=\"queued\"} 0"));
    assert!(
        stale.contains("plurx_background_resource_reservations{resource_class=\"source_io\"} 0")
    );
    assert!(stale.contains("plurx_libraries_total 3"));
    assert!(stale.contains("plurx_users_total 4"));
    // The whole verdict family is absent, not zero, until a sample lands.
    // A dashboard has to reach for `absent()` to tell those apart, which
    // is why the runbook says so.
    assert!(!absent.contains("plurx_analysis_queue_health"));
    assert!(!stale.contains("plurx_analysis_queue_health"));
    assert!(!stale.contains("plurx_analysis_queue_ready_24h"));
}

#[test]
fn the_queue_verdict_is_exposed_as_one_series_per_verdict_with_its_evidence() {
    let health = plurx_core::store::AnalysisQueueHealth {
        ready_24h: 0,
        attempt_limit_24h: 84,
        claimed_24h: 396,
        claimable: 1_118,
        running_past_lease: 11,
        last_ready_at_ms: 1_700_000_000_000,
    };
    let rendered = render_store_metrics(StoreMetricsView {
        sample: Some(plurx_core::store::PrometheusStoreSnapshot::default()),
        age_seconds: Some(4),
        valid: true,
        errors: 0,
        queue_health: Some(crate::state::AnalysisQueueHealthReport {
            health,
            claims_since_start: 412,
            lease_losses_since_start: 398,
            verdict: plurx_core::store::analysis_queue_verdict(health, 412, 398, 3),
        }),
    });
    // Exactly one verdict is 1 and the rest are 0, so a dashboard can sum
    // the family and get 1 rather than having to know which label to ask
    // for.
    assert!(rendered.contains("plurx_analysis_queue_health{verdict=\"dead\"} 1\n"));
    for quiet in ["idle", "healthy", "degraded"] {
        assert!(
            rendered.contains(&format!(
                "plurx_analysis_queue_health{{verdict=\"{quiet}\"}} 0\n"
            )),
            "{quiet} should be published as 0"
        );
    }
    // Every figure the verdict ruled on is published beside it, so the
    // one-word answer can be checked rather than trusted.
    for line in [
        "plurx_analysis_queue_ready_24h 0\n",
        "plurx_analysis_queue_attempt_limit_24h 84\n",
        "plurx_analysis_queue_claimed_24h 396\n",
        "plurx_analysis_queue_claimable 1118\n",
        "plurx_analysis_queue_running_past_lease 11\n",
        "plurx_analysis_queue_claims_since_start 412\n",
        "plurx_analysis_queue_lease_losses_since_start 398\n",
    ] {
        assert!(rendered.contains(line), "missing {line:?}");
    }
    for family in [
        "plurx_analysis_queue_claimed_24h",
        "plurx_analysis_queue_claimable",
    ] {
        assert!(
            rendered.contains(&format!("# TYPE {family} gauge\n")),
            "{family} needs a TYPE line"
        );
    }
}

#[test]
fn membership_exposition_reports_quorum_without_node_identity_labels() {
    use plurx_core::cluster::membership::{MembershipMetricsSample, PassiveMembershipMetricsView};

    let rendered = render_passive_membership_metrics(PassiveMembershipMetricsView {
        replicated: true,
        valid: true,
        age_seconds: Some(3),
        errors: 2,
        sample: Some(MembershipMetricsSample {
            voters: 4,
            learners: 1,
            heartbeat_fresh_voters: 3,
            heartbeat_stale_voters: 1,
            removals_pending: 1,
            local_is_voter: true,
        }),
    });
    assert!(rendered.contains("plurx_cluster_replicated 1"));
    assert!(rendered.contains("plurx_cluster_membership_sample_valid 1"));
    assert!(rendered.contains("plurx_cluster_membership_sample_age_seconds 3"));
    assert!(rendered.contains("plurx_cluster_membership_sample_errors_total 2"));
    assert!(rendered.contains("plurx_cluster_nodes{role=\"voter\",heartbeat=\"fresh\"} 3"));
    assert!(rendered.contains("plurx_cluster_nodes{role=\"voter\",heartbeat=\"stale\"} 1"));
    assert!(rendered.contains("plurx_cluster_quorum_required 3"));
    assert!(rendered.contains("plurx_cluster_heartbeat_quorum_available 1"));
    assert!(rendered.contains("plurx_cluster_local_is_voter 1"));
    assert!(rendered.contains("plurx_cluster_removals_pending 1"));
    assert!(!rendered.contains("node_id"));
    assert!(!rendered.contains("hostname"));
}

#[test]
fn raft_exposition_is_fixed_and_privacy_safe() {
    use plurx_core::cluster::migration::status::{
        PassiveRaftMetricsView, PassiveRaftSample, QuorumWatermarkSample,
    };
    let zero_snapshot_histogram = DbSnapshotHistogram {
        count: 0,
        sum_nanos: 0,
        cumulative_buckets: [0; 16],
    };

    let rendered = render_passive_raft_metrics(PassiveRaftMetricsView {
        local_source: true,
        sample: Some(PassiveRaftSample {
            node_id: 1,
            current_term: 7,
            leader_id: Some(1),
            last_applied_index: Some(42),
            leader_known: true,
            is_leader: true,
        }),
        age_seconds: Some(2),
        valid: true,
        errors: 3,
        leader_changes: 4,
        watermark_source: true,
        watermark_requires_local_binding: true,
        watermark: Some(QuorumWatermarkSample {
            committed_index: 45,
            apply_lag_entries: Some(3),
        }),
        watermark_age_millis: Some(250),
        watermark_valid: true,
        watermark_local_reads_supported: true,
        watermark_errors: 5,
        snapshot_metrics: Some(DbSnapshotMetricsSnapshot {
            storage_deferrals_total: 0,
            required_storage_bytes: None,
            build_ok: DbSnapshotHistogram {
                count: 2,
                sum_nanos: 1_250_000_000,
                cumulative_buckets: [0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2],
            },
            build_error: zero_snapshot_histogram,
            install_ok: zero_snapshot_histogram,
            install_error: zero_snapshot_histogram,
            last_build: None,
            last_install: None,
        }),
        state_machine_bytes: Some(plurx_core::cluster::migration::status::StateMachineBytes {
            db: 10,
            wal: 20,
            snapshots: 30,
            logs: 40,
        }),
    });

    assert!(rendered.contains("plurx_raft_metric_sample_valid{source=\"local\"} 1"));
    assert!(rendered.contains("plurx_raft_metric_sample_age_seconds{source=\"local\"} 2"));
    assert!(rendered.contains("plurx_raft_metric_sample_errors_total{source=\"local\"} 3"));
    assert!(rendered.contains("plurx_raft_current_term 7"));
    assert!(rendered.contains("plurx_raft_applied_index 42"));
    assert!(rendered.contains("plurx_raft_leader_known 1"));
    assert!(rendered.contains("plurx_raft_is_leader 1"));
    assert!(rendered.contains("plurx_raft_leader_changes_total 4"));
    assert!(rendered.contains("plurx_raft_metric_sample_valid{source=\"watermark\"} 1"));
    assert!(rendered.contains("plurx_raft_metric_sample_age_seconds{source=\"watermark\"} 0.250"));
    assert!(rendered.contains("plurx_raft_metric_sample_errors_total{source=\"watermark\"} 5"));
    assert!(rendered.contains("plurx_raft_commit_index 45"));
    assert!(rendered.contains("plurx_raft_apply_lag_entries 3"));
    assert!(rendered.contains("plurx_raft_local_read_protocol_supported 1"));
    assert!(rendered.contains(
        "plurx_raft_snapshot_seconds_bucket{operation=\"build\",outcome=\"ok\",le=\"0.1\"} 1"
    ));
    assert!(rendered.contains(
        "plurx_raft_snapshot_seconds_bucket{operation=\"build\",outcome=\"ok\",le=\"+Inf\"} 2"
    ));
    assert!(rendered.contains(
        "plurx_raft_snapshot_seconds_sum{operation=\"build\",outcome=\"ok\"} 1.250000000"
    ));
    assert!(rendered.contains("plurx_raft_state_machine_bytes{file=\"db\"} 10"));
    assert!(rendered.contains("plurx_raft_state_machine_bytes{file=\"wal\"} 20"));
    assert!(rendered.contains("plurx_raft_state_machine_bytes{file=\"snapshots\"} 30"));
    assert!(rendered.contains("plurx_raft_state_machine_bytes{file=\"logs\"} 40"));
    assert!(!rendered.contains("node_id"));
    assert!(!rendered.contains("leader_id"));

    let stale = render_passive_raft_metrics(PassiveRaftMetricsView {
        watermark_valid: false,
        watermark_age_millis: Some(1_001),
        watermark: Some(QuorumWatermarkSample {
            committed_index: 45,
            apply_lag_entries: None,
        }),
        ..PassiveRaftMetricsView {
            local_source: true,
            sample: Some(PassiveRaftSample {
                node_id: 2,
                current_term: 7,
                leader_id: Some(1),
                last_applied_index: Some(42),
                leader_known: true,
                is_leader: false,
            }),
            age_seconds: Some(1),
            valid: true,
            errors: 0,
            leader_changes: 0,
            watermark_source: true,
            watermark_requires_local_binding: true,
            watermark: None,
            watermark_age_millis: None,
            watermark_valid: false,
            watermark_local_reads_supported: true,
            watermark_errors: 1,
            snapshot_metrics: None,
            state_machine_bytes: None,
        }
    });
    assert!(stale.contains("plurx_raft_metric_sample_valid{source=\"watermark\"} 0"));
    assert!(stale.contains("plurx_raft_commit_index 45"));
    assert!(!stale.contains("plurx_raft_apply_lag_entries"));

    let absent = render_passive_raft_metrics(PassiveRaftMetricsView {
        local_source: false,
        sample: None,
        age_seconds: None,
        valid: false,
        errors: 0,
        leader_changes: 0,
        watermark_source: false,
        watermark_requires_local_binding: false,
        watermark: None,
        watermark_age_millis: None,
        watermark_valid: false,
        watermark_local_reads_supported: false,
        watermark_errors: 0,
        snapshot_metrics: None,
        state_machine_bytes: None,
    });
    assert!(absent.contains("plurx_raft_metric_sample_valid{source=\"local\"} 0"));
    assert!(!absent.contains("plurx_raft_metric_sample_age_seconds"));
    assert!(!absent.contains("plurx_raft_applied_index"));
    assert!(absent.contains("plurx_raft_metric_sample_valid{source=\"watermark\"} 0"));
    assert!(!absent.contains("plurx_raft_commit_index"));
    assert!(!absent.contains("plurx_raft_apply_lag_entries"));
}

fn beacon(event: &str, ms: i64) -> ClientLog {
    ClientLog {
        transport: None,
        frame_drop: None,
        level: "warn".into(),
        event: event.into(),
        message: "x".into(),
        method: Some("remux".into()),
        code: None,
        title: None,
        file_id: None,
        vcodec: None,
        src: None,
        line: None,
        col: None,
        stack: None,
        detail: None,
        seek_trace: None,
        ua: None,
        attempt: None,
        reason: None,
        runway: None,
        ms: Some(ms),
        bandwidth: None,
        height: None,
        encoder: None,
        decode_hw: None,
        decode_supported: None,
        decode_smooth: None,
        decode_power_efficient: None,
        session_id: None,
        snapshot: None,
        link_sample: None,
        candidate_recovery: None,
        control: None,
        control_trigger: None,
        delivered_range: None,
        delivered_dv_profile: None,
        declared_dv_profiles: None,
    }
}

#[test]
fn transport_provenance_is_bounded_and_retained_in_playback_events() {
    for origin in [
        "viewer_control",
        "viewer_keyboard",
        "media_session",
        "internal",
        "native_unknown",
    ] {
        let mut ev = beacon("transport_command", 0);
        ev.transport = Some(ClientTransportRecord {
            command_id: "tc-123-1".into(),
            action: "pause".into(),
            origin: origin.into(),
            reason: "recovery_exhausted".into(),
            intent_generation: Some(2),
            position_ms: Some(123000),
            desired_before: Some(true),
            desired_after: Some(false),
            media_paused: Some(false),
            media_ended: Some(false),
            client_timestamp_ms: Some(100),
        });
        let projected = client_playback_event(&ev, 7);
        let extra: serde_json::Value =
            serde_json::from_str(projected.extra.as_deref().expect("retained transport"))
                .expect("JSON");
        assert_eq!(extra["transport"]["origin"], origin);
        assert_eq!(extra["transport"]["desired_before"], true);
        assert!(extra["transport"]["server_receipt_timestamp_ms"]
            .as_i64()
            .is_some());
        ev.transport.as_mut().expect("record").reason = "private\nforged".into();
        let malformed = client_playback_event(&ev, 7);
        let extra: serde_json::Value =
            serde_json::from_str(malformed.extra.as_deref().expect("message")).expect("JSON");
        assert!(extra.get("transport").is_none());
    }
}

#[test]
fn client_seek_trace_is_bounded_to_one_log_line() {
    let mut event = beacon("seek_route", 0);
    event.seek_trace = Some(format!("{{\"target_ms\":50000}}\n{}", "x".repeat(2_000)));
    let line = client_log_line(&event, 0);
    let trace = line.split(" seek_trace=").nth(1).expect("seek trace");
    assert!(trace.starts_with("{\"target_ms\":50000}x"));
    assert!(!trace.contains('\n'));
    assert!(trace.chars().count() <= 641);
}

#[test]
fn client_log_error_report_fields_are_bounded() {
    let mut event = beacon("client_error", 0);
    event.src = Some("/assets/core/cards.js\nforged?token=secret".into());
    event.line = Some(17);
    event.col = Some(9);
    event.stack = Some(format!("{}\nforged", "x".repeat(3_000)));
    let line = client_log_line(&event, 0);
    assert!(line.contains("src=/assets/core/cards.jsforged?token=secret"));
    assert!(line.contains(" line=17 col=9 stack="));
    assert!(!line.contains('\n'));
    assert!(
        line.chars().count() < 2_300,
        "bounded stack keeps one log line bounded"
    );
}

/// The accusation is printed where a delivery actually failed, and the
/// facts are printed everywhere.
///
/// `playbackContext()` is spread by sixteen beacons, so without the scope
/// `caps_mismatch=true` rides `ttff` and `stall` lines — and a `grep
/// caps_mismatch` over an operator's log returns successful playbacks,
/// which is the opposite of what the field is for. The three facts still
/// ride everything: they describe the delivery whatever became of it.
#[test]
fn the_caps_accusation_is_scoped_to_a_delivery_that_failed() {
    let facts = |event: &str| {
        let mut ev = beacon(event, 0);
        ev.delivered_range = Some("dolby_vision".into());
        ev.delivered_dv_profile = Some(7);
        ev.declared_dv_profiles = Some("5,8".into());
        client_log_line(&ev, 0)
    };

    for failed in DELIVERY_FAILURE_EVENTS {
        let line = facts(failed);
        assert!(line.contains(" caps_mismatch=true"), "{line}");
    }
    for served in ["ttff", "stall", "stall_recovery"] {
        let line = facts(served);
        assert!(
            !line.contains("caps_mismatch"),
            "a playback that worked is not a server defect: {line}"
        );
        assert!(
            line.contains(" delivered_dv_profile=7") && line.contains(" declared_dv_profiles=5,8"),
            "but the facts still ride it: {line}"
        );
    }
}

/// The verdict reads exactly the set the line prints, and no further.
///
/// They used to disagree: the line clipped at 32 characters and the
/// verdict parsed the whole string. A declared set longer than that with
/// its matching profile in the tail printed a set the operator can see
/// contains no match, beside a verdict that silently found one — a
/// contradiction with nothing on the line to explain it.
#[test]
fn the_declared_set_the_verdict_reads_is_the_one_the_line_prints() {
    let mut ev = beacon("stream_rejected", 0);
    ev.delivered_range = Some("dolby_vision".into());
    ev.delivered_dv_profile = Some(7);

    // Longer than the bound, and the match is in the tail.
    let long = format!("{},7", "9,".repeat(20));
    assert!(long.len() > DECLARED_DV_PROFILES_MAX);
    ev.declared_dv_profiles = Some(long.clone());
    let line = client_log_line(&ev, 0);
    assert!(!line.contains(" declared_dv_profiles="), "{line}");
    assert!(
        !line.contains("caps_mismatch"),
        "a set nobody can read convicts nobody: {line}"
    );
    assert_eq!(client_caps_mismatch(Some(7), Some(&long)), None);

    // At the bound it is printed and read, both.
    let fits = "5,8";
    ev.declared_dv_profiles = Some(fits.into());
    let readable = client_log_line(&ev, 0);
    assert!(readable.contains(" declared_dv_profiles=5,8"), "{readable}");
    assert!(readable.contains(" caps_mismatch=true"), "{readable}");

    // Control characters are stripped before either reads it, so a
    // forged second line cannot ride the field the verdict parses.
    ev.declared_dv_profiles = Some("5,8\nclient[x] admin login ok".into());
    let forged = client_log_line(&ev, 0);
    assert!(!forged.contains('\n'), "{forged}");
}

/// The rejection report names what was handed over and what was claimed,
/// and says when they disagree.
///
/// The 2026-09-02 observation was logged as "browser refused the remux
/// stream", which is the browser doing exactly what its own capabilities
/// document promised. The line has to carry the other party's half.
#[test]
fn a_stream_rejection_names_the_profile_it_was_handed() {
    let mut ev = beacon("stream_rejected", 0);
    ev.ua = Some("Safari".into());
    ev.message = "the server handed this browser a Dolby Vision Profile 7 stream \
                      it did not declare"
        .into();
    ev.delivered_range = Some("dolby_vision".into());
    ev.delivered_dv_profile = Some(7);
    ev.declared_dv_profiles = Some("5,8".into());

    let line = client_log_line(&ev, 0);
    assert!(line.contains(" delivered_range=dolby_vision"), "{line}");
    assert!(line.contains(" delivered_dv_profile=7"), "{line}");
    assert!(line.contains(" declared_dv_profiles=5,8"), "{line}");
    assert!(
        line.contains(" caps_mismatch=true"),
        "7 is not in {{5,8}}: {line}"
    );

    // The ordinary case — the client got what it asked for and could not
    // decode it anyway — leaves the field off. A flag on every line is a
    // flag nobody reads.
    ev.delivered_dv_profile = Some(8);
    let agreed = client_log_line(&ev, 0);
    assert!(agreed.contains(" delivered_dv_profile=8"), "{agreed}");
    assert!(!agreed.contains("caps_mismatch"), "{agreed}");

    // And silence on either side is silence, not a verdict: a stripped
    // stream carries no profile, and a build that declares none has made
    // no claim to contradict.
    assert_eq!(client_caps_mismatch(None, Some("5,8")), None);
    assert_eq!(client_caps_mismatch(Some(7), None), None);
    assert_eq!(client_caps_mismatch(Some(7), Some("")), None);
    assert_eq!(client_caps_mismatch(Some(7), Some(" 5 , 7 ")), Some(false));
    // A non-positive profile is nothing said, not profile zero. A client
    // that defaults the field rather than omitting it would otherwise
    // report a fleet-wide server fault that is not happening — and with no
    // delivered profile on the line to disprove it, because both print
    // sites drop a non-positive one.
    assert_eq!(client_caps_mismatch(Some(0), Some("5,8")), None);
    assert_eq!(client_caps_mismatch(Some(-1), Some("5,8")), None);
    let mut defaulted = beacon("stream_rejected", 0);
    defaulted.delivered_dv_profile = Some(0);
    defaulted.declared_dv_profiles = Some("5,8".into());
    assert!(
        !client_log_line(&defaulted, 0).contains("caps_mismatch"),
        "{}",
        client_log_line(&defaulted, 0)
    );

    // A newline inside a client-supplied field is how one log line becomes
    // two forged ones.
    let mut forged = beacon("stream_rejected", 0);
    forged.delivered_dv_profile = Some(7);
    forged.declared_dv_profiles = Some("5\nERROR plurxd: forged".into());
    let one_line = client_log_line(&forged, 0);
    assert!(!one_line.contains('\n'), "{one_line}");
    assert!(
        one_line.contains("declared_dv_profiles=5ERROR"),
        "{one_line}"
    );
}

/// The same three facts survive into the stored event, because the log
/// ring is 2000 lines and a defect that takes a week to reproduce is not
/// in it by the time anyone looks.
#[test]
fn a_stored_rejection_keeps_what_the_line_printed() {
    let mut ev = beacon("stream_rejected", 0);
    ev.detail = Some("bufferAppendError".into());
    ev.delivered_range = Some("dolby_vision".into());
    ev.delivered_dv_profile = Some(7);
    ev.declared_dv_profiles = Some("5,8".into());
    let event = client_playback_event(&ev, 1);
    let extra: serde_json::Value =
        serde_json::from_str(&event.extra.expect("extra")).expect("extra parses");
    assert_eq!(extra["delivered_range"], "dolby_vision");
    assert_eq!(extra["delivered_dv_profile"], 7);
    assert_eq!(extra["declared_dv_profiles"], "5,8");
    assert_eq!(extra["caps_mismatch"], true);
    // `detail` is the hls.js error type on that path and is not this
    // event's to overwrite. Set on the way in, so the assertion is about
    // what survives rather than about the default it was given.
    assert_eq!(event.detail.as_deref(), Some("bufferAppendError"));
}

/// The decoder verdict rides with the client facts, and says which one it
/// is loudly enough to notice in a wall of log lines. `null` (a browser
/// without mediaCapabilities) prints nothing rather than guessing.
#[test]
fn a_software_decode_is_named_in_the_beacon() {
    let mut ev = beacon("stall", 900);
    assert!(
        !client_log_line(&ev, 0).contains("decode="),
        "silent when unknown"
    );

    ev.decode_hw = Some(true);
    assert!(client_log_line(&ev, 0).contains(" decode=hw"));

    ev.decode_hw = Some(false);
    ev.decode_smooth = Some(false);
    let line = client_log_line(&ev, 0);
    assert!(line.contains(" decode=SOFTWARE/not-smooth"), "{line}");
}

#[test]
fn browser_predictions_do_not_claim_decoder_identity() {
    let mut ev = beacon("stall", 900);
    ev.decode_supported = Some(false);
    ev.decode_smooth = Some(false);
    ev.decode_power_efficient = Some(false);
    let line = client_log_line(&ev, 0);
    assert!(line.contains("decode_supported_prediction=false"), "{line}");
    assert!(line.contains("decode_smooth_prediction=false"), "{line}");
    assert!(
        line.contains("decode_power_efficient_prediction=false"),
        "{line}"
    );
    assert!(!line.contains("decode=SOFTWARE"), "{line}");
    assert!(!line.contains("decode=hw"), "{line}");
    let absent = beacon("stall", 900);
    assert!(!client_log_line(&absent, 0).contains("_prediction="));
    let event = client_playback_event(&ev, 1);
    let extra: serde_json::Value =
        serde_json::from_str(event.extra.as_deref().expect("prediction JSON"))
            .expect("valid prediction JSON");
    assert_eq!(extra["decode_supported"], false);
    assert_eq!(extra["decode_smooth"], false);
    assert_eq!(extra["decode_power_efficient"], false);
    assert!(extra.get("decode_hw").is_none());
}

#[test]
fn session_identity_does_not_change_the_human_log_line() {
    let mut event = beacon("ttff", 684);
    let before = client_log_line(&event, 0);
    event.session_id = Some("session-a".into());
    assert_eq!(client_log_line(&event, 0), before);
}

#[test]
fn telemetry_retention_does_not_change_the_human_log_surface() {
    let mut event = beacon("disabled_telemetry_proof", 0);
    event.message = "still logs".into();
    assert_eq!(
        client_log_line(&event, 0),
        "client disabled_telemetry_proof method=remux: still logs ms=0"
    );
}

/// A measurement is named for what it measured.
///
/// `ms` carries a start time on a `ttff` event and a stall's *duration* on
/// a `stall` one, and both were printed as `ttff_ms`. So a grep for
/// `ttff_ms` over the log ring returned a series that was part start times
/// and part stall lengths — with the stalls, being longer, owning the whole
/// tail. That is the one number M0 exists to produce, wrong in the
/// direction that makes the server look worse than it is: on media1 a real
/// p90 of 1.5 s read as 4.6 s.
#[test]
fn a_stalls_duration_is_never_reported_as_a_start_time() {
    let start = client_log_line(&beacon("ttff", 684), 0);
    assert!(start.contains("ttff_ms=684"), "{start}");

    let stall = client_log_line(&beacon("stall", 4584), 0);
    assert!(stall.contains("stall_ms=4584"), "{stall}");
    assert!(
        !stall.contains("ttff_ms"),
        "a stall's duration entered the start-time series: {stall}"
    );

    // An event nobody has taught this about says the neutral thing rather
    // than borrowing whichever name is nearest.
    let other = client_log_line(&beacon("hls_fatal", 12), 0);
    assert!(other.contains(" ms=12"), "{other}");
    assert!(
        !other.contains("ttff_ms") && !other.contains("stall_ms"),
        "{other}"
    );
}

/// The log ring must survive a client stuck in an error loop: the burst gets
/// through, the flood does not, and the gap is accounted for rather than
/// silently dropped.
#[test]
fn client_log_bucket_bounds_a_flood_and_counts_the_gap() {
    let t0 = Instant::now();
    let mut b = LogBucket::new(30);

    // The full burst is admitted, nothing suppressed yet.
    for _ in 0..30 {
        assert_eq!(b.admit(t0), Some(0));
    }
    // The next 500 in the same instant are dropped.
    for _ in 0..500 {
        assert_eq!(b.admit(t0), None);
    }

    // Two seconds later one token has refilled (30/min), and the report that
    // spends it carries the count of everything dropped meanwhile.
    let t1 = t0 + Duration::from_secs(2);
    assert_eq!(b.admit(t1), Some(500));
    // That count resets once reported.
    let t2 = t1 + Duration::from_secs(2);
    assert_eq!(b.admit(t2), Some(0));

    // A long quiet period refills to the burst cap and no further: an idle
    // client can't bank hours of credit and then dump it.
    let t3 = t2 + Duration::from_secs(3600);
    for _ in 0..30 {
        assert_eq!(b.admit(t3), Some(0));
    }
    assert_eq!(b.admit(t3), None);
}

/// A steady trickle below the limit is never throttled — normal playback
/// errors keep flowing.
#[test]
fn client_log_bucket_passes_a_normal_trickle() {
    let mut now = Instant::now();
    let mut b = LogBucket::new(30);
    // One report every 5s for an hour: well under 30/min.
    for _ in 0..720 {
        assert_eq!(b.admit(now), Some(0));
        now += Duration::from_secs(5);
    }
}

#[test]
fn client_log_limits_are_isolated_per_user() {
    let now = Instant::now();
    let mut limiter = ClientLogLimiter::new();
    for _ in 0..CLIENT_LOG_USER_PER_MIN {
        assert_eq!(limiter.admit(1, now), Some(0));
    }
    assert_eq!(limiter.admit(1, now), None);
    assert_eq!(
        limiter.admit(2, now),
        Some(0),
        "one user's flood must not silence another user"
    );
}

#[test]
fn client_log_user_bucket_registry_is_bounded() {
    let now = Instant::now();
    let mut limiter = ClientLogLimiter::new();
    for user_id in 0..=CLIENT_LOG_USER_BUCKETS_MAX as i64 {
        let _ = limiter.admit(user_id, now + Duration::from_nanos(user_id as u64));
    }
    assert_eq!(limiter.users.len(), CLIENT_LOG_USER_BUCKETS_MAX);
    assert!(limiter
        .users
        .contains_key(&(CLIENT_LOG_USER_BUCKETS_MAX as i64)));
    assert!(
        !limiter.users.contains_key(&0),
        "the oldest idle bucket was evicted"
    );
}

#[test]
fn client_log_suppression_counts_survive_the_global_ceiling() {
    let now = Instant::now();
    let mut limiter = ClientLogLimiter {
        global: LogBucket::new(1),
        users: HashMap::from([(1, LogBucket::new(1)), (2, LogBucket::new(1))]),
    };
    assert_eq!(limiter.admit(1, now), Some(0));
    assert_eq!(limiter.admit(1, now), None, "user bucket counts one drop");

    let minute = now + Duration::from_secs(60);
    assert_eq!(
        limiter.admit(2, minute),
        Some(0),
        "another user consumes the refilled global token"
    );
    assert_eq!(
        limiter.admit(1, minute),
        None,
        "user one's recovered token meets the global ceiling"
    );

    assert_eq!(
        limiter.admit(1, minute + Duration::from_secs(60)),
        Some(2),
        "one per-user and one global drop are both reported"
    );
}

#[tokio::test]
async fn client_event_joins_and_persists_every_live_session_measurement() {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry test store"));
    let mut beacon = beacon("stall", 900);
    beacon.session_id = Some("session-a".into());
    let event = client_playback_event(&beacon, 7);
    let info = crate::transcode::SessionInfo {
        vod_observation: None,
        id: "session-a".into(),
        presentation: "live-recovery",
        file_id: 42,
        item_id: 4,
        item_title: "not persisted".into(),
        user_name: "not persisted".into(),
        target_height: 1080,
        encoder: "qsv",
        tone_map_peak_nits: None,
        tone_map_peak_source: None,
        started_unix: 0,
        idle_seconds: 0,
        last_request: "segment",
        lease_mode: "explicit",
        lease_state: "active",
        lease_timeout_ms: Some(30_000),
        startup_state: Some("awaiting_presentation"),
        startup_remaining_ms: Some(18_000),
        presentation_progress_seen: Some(true),
        produced_end_ms: Some(48_000),
        served_end_ms: Some(44_000),
        budget_anchor_sequence: Some(7),
        allowed_end_ms: Some(64_000),
        carried_surplus_ms: Some(2_000),
        demand_observation_age_ms: Some(1_000),
        staged_bytes: 400_000,
        playlist_target_ms: Some(16_000),
        served_revision: Some(9),
        last_segment_advanced_idle_ms: Some(250),
        next_publication_in_ms: Some(3_000),
        publication_deadline_remaining_ms: Some(8_000),
        maintenance_state: "idle",
        rate_estimate_source: "recent_progress",
        estimate_active_speed: Some(1.25),
        pause_grace_remaining_ms: None,
        retirement_reason: None,
        advertised_bytes: 4_000_000,
        grace_bytes: 500_000,
        reserved_bytes: Some(8_000_000),
        live_bytes: 4_500_000,
        control_demand: Some("active"),
        reported_position_ms: Some(9_000),
        client_runway_ms: Some(20_000),
        render_state: Some("stalled"),
        server_ready_state: "ready",
        server_ready_anchor_ms: Some(9_000),
        server_ready_end_ms: Some(44_000),
        server_ready_seconds: Some(35.0),
        server_next_ready_start_ms: Some(9_000),
        server_next_ready_end_ms: Some(44_000),
        production_policy: "explicit_demand",
        production_ahead_seconds: Some(35),
        production_target_seconds: Some(50),
        producer_control: Some(
            crate::playback_control::RollingProducerOperationalSnapshot {
                phase: "held",
                deadline_attempt: None,
                deadline_mode: None,
                deadline_remaining_ms: None,
                due_attempt: None,
                due_mode: None,
                due_overdue_ms: None,
                process_exit_attempt: None,
                process_exit_due: false,
                physical_flow: "held",
                last_flow_applied_sequence: 11,
                last_applied_sequence: 17,
                observation_only: true,
                action_owner: "legacy_compatibility",
                startup_kind: None,
                presentation_contract_fingerprint: None,
                metadata_response_authorized: false,
                producer_media_published: false,
                retry_state: "legacy_compatibility",
                decision_sequence: None,
                decision_reason: None,
                executor_state: "registered",
                executor_pending_decision_age_ms: None,
                executor_last_observed_sequence: 0,
                executor_last_action_failure: None,
                executor_registered: true,
                decision_applied_sequence: None,
                decision_installed_attempt: None,
                pending_probe_sequence: None,
                pending_probe_attempt: None,
                pending_probe_deadline_remaining_ms: None,
                last_probe_outcome: "none",
                producer_ended_with_proposal: false,
                proposal: None,
                decode_fault: None,
                decode_error_records: None,
                diagnostics_qualification: None,
                completion: "incomplete",
                completion_attempt: None,
                completion_final_segment: None,
                completion_final_end_ms: None,
            },
        ),
        producer_state: "held",
        producer_attempt: Some(3),
        playlist_ready: Some(true),
        published_segment: Some(5),
        next_media_sequence: Some(6),
        pending_fetched_segment: None,
        speed: Some(2.0),
        recent_speed: Some(1.7),
        out_time_ms: Some(10_000),
        progress_idle_ms: 15,
        producer_exit_success: Some(false),
        producer_exit_code: Some(23),
        producer_exit_signal: None,
        producer_exit_idle_ms: Some(7),
        published_end_ms: Some(44_000),
        fetched_end_ms: 10_000,
        fetched_segment: Some(4),
        first_retained_segment: Some(1),
        playlist_shape: "sliding",
        ahead_seconds: Some(34),
        hold_reason: Some(crate::transcode::AheadHoldReason::Time),
        resume_below_seconds: Some(30),
        resume_below_bytes: None,
        ahead_bytes: Some(123),
        delivered_bytes: 456,
        delivered_bps: Some(8_000_000),
        delivered_idle_ms: 25,
        http_wait_count: 0,
        http_wait_oldest_ms: None,
        http_wait_segment: None,
        status_generated_unix_ms: 1_000,
        readrate: 2.0,
        suspended: true,
        suspend_count: 1,
    };
    emit_client_playback_event(Arc::clone(&store), event, Some(&info), None, None);
    let row = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(row) = store
                .playback_events(&PlaybackEventQuery {
                    event: Some("stall".into()),
                    limit: 10,
                    ..PlaybackEventQuery::default()
                })
                .await
                .expect("query joined client event")
                .into_iter()
                .next()
            {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("joined client event persisted");
    let correlation = crate::transcode::session_log_id("session-a");
    assert_eq!(row.session_id.as_deref(), Some(correlation.as_str()));
    assert_eq!(row.file_id, Some(42));
    assert_eq!(row.speed_recent, Some(1.7));
    assert_eq!(row.ahead_seconds, Some(34));
    assert_eq!(row.suspended, Some(true));
    assert_eq!(row.hold_reason.as_deref(), Some("time"));
    assert_eq!(row.delivered_bps, Some(8_000_000));
    assert_eq!(row.readrate, Some(2.0));
    let extra: serde_json::Value =
        serde_json::from_str(row.extra.as_deref().expect("joined status JSON"))
            .expect("valid joined status JSON");
    assert_eq!(extra["server"]["progress_idle_ms"], 15);
    assert_eq!(extra["server"]["producer_state"], "held");
    assert_eq!(extra["server"]["producer_exit_success"], false);
    assert_eq!(extra["server"]["producer_exit_code"], 23);
    assert_eq!(
        extra["server"]["producer_exit_signal"],
        serde_json::Value::Null
    );
    assert_eq!(extra["server"]["producer_exit_idle_ms"], 7);
    assert_eq!(extra["server"]["producer_attempt"], 3);
    assert_eq!(extra["server"]["playlist_ready"], true);
    assert_eq!(extra["server"]["published_segment"], 5);
    assert_eq!(extra["server"]["published_end_ms"], 44_000);
    assert_eq!(extra["server"]["next_media_sequence"], 6);
    assert_eq!(extra["server"]["fetched_segment"], 4);
    assert_eq!(extra["server"]["playlist_shape"], "sliding");
    assert_eq!(extra["server"]["last_request"], "segment");
    assert_eq!(extra["server"]["lease_mode"], "explicit");
    assert_eq!(extra["server"]["lease_state"], "active");
    assert_eq!(extra["server"]["lease_timeout_ms"], 30_000);
    assert_eq!(extra["server"]["produced_end_ms"], 48_000);
    assert_eq!(extra["server"]["budget_anchor_sequence"], 7);
    assert_eq!(extra["server"]["demand_observation_age_ms"], 1_000);
    assert_eq!(extra["server"]["served_end_ms"], 44_000);
    assert_eq!(extra["server"]["staged_bytes"], 400_000);
    assert_eq!(extra["server"]["playlist_target_ms"], 16_000);
    assert_eq!(extra["server"]["served_revision"], 9);
    assert_eq!(extra["server"]["rate_estimate_source"], "recent_progress");
    assert_eq!(extra["server"]["advertised_bytes"], 4_000_000);
    assert_eq!(extra["server"]["grace_bytes"], 500_000);
    assert_eq!(extra["server"]["reserved_bytes"], 8_000_000);
    assert_eq!(extra["server"]["live_bytes"], 4_500_000);
    assert_eq!(extra["server"]["control_demand"], "active");
    assert_eq!(extra["server"]["production_policy"], "explicit_demand");
    assert_eq!(extra["server"]["production_ahead_seconds"], 35);
    assert_eq!(extra["server"]["production_target_seconds"], 50);
    assert_eq!(extra["server"]["producer_control"]["phase"], "held");
    assert_eq!(
        extra["server"]["producer_control"]["last_applied_sequence"],
        17
    );
}

#[test]
fn client_stall_snapshot_survives_a_superseded_session() {
    let mut event = beacon("stall", 12_000);
    event.session_id = Some("gone-session".into());
    event.snapshot = Some(ClientPlaybackSnapshot {
        position_ms: Some(90_000),
        runway: Some(0.4),
        time_control_status: Some("waiting".into()),
        waiting_reason: Some("AVPlayerWaitingToMinimizeStallsReason".into()),
        playback_buffer_empty: Some(true),
        media_requests: Some(17),
        bytes_transferred: Some(8_192),
        observed_bitrate_bps: Some(12_345_000.0),
        server: Some(ClientServerSnapshot {
            observed_age_ms: Some(2_345),
            recent_speed: Some(0.7),
            ahead_seconds: Some(4),
            delivered_bps: Some(6_000_000),
            readrate: Some(0.8),
            progress_idle_ms: Some(11_000),
            published_end_ms: Some(125_000),
            produced_end_ms: Some(141_000),
            playlist_target_ms: Some(16_000),
            budget_anchor_sequence: Some(8),
            demand_observation_age_ms: Some(1_500),
            fetched_end_ms: Some(121_000),
            playlist_shape: Some("sliding".into()),
            last_request: Some("segment".into()),
            ..ClientServerSnapshot::default()
        }),
        ..ClientPlaybackSnapshot::default()
    });

    let persisted = client_playback_event(&event, 7);
    assert_eq!(persisted.runway_ds, Some(4));
    assert_eq!(persisted.bandwidth_kbps, Some(12_345));
    assert_eq!(persisted.speed_recent, Some(0.7));
    assert_eq!(persisted.ahead_seconds, Some(4));
    assert_eq!(persisted.delivered_bps, Some(6_000_000));
    assert_eq!(persisted.readrate, Some(0.8));
    let extra: serde_json::Value =
        serde_json::from_str(persisted.extra.as_deref().expect("client snapshot JSON"))
            .expect("valid client snapshot JSON");
    assert_eq!(extra["client"]["position_ms"], 90_000);
    assert_eq!(extra["client"]["time_control_status"], "waiting");
    assert_eq!(extra["client"]["media_requests"], 17);
    assert_eq!(extra["client"]["server"]["observed_age_ms"], 2_345);
    assert_eq!(extra["client"]["server"]["progress_idle_ms"], 11_000);
    assert_eq!(extra["client"]["server"]["produced_end_ms"], 141_000);
    assert_eq!(extra["client"]["server"]["budget_anchor_sequence"], 8);
    assert_eq!(extra["client"]["server"]["playlist_shape"], "sliding");

    let line = client_log_line(&event, 0);
    assert!(line.contains("runway=0.4s"), "{line}");
    assert!(line.contains("bw=12345kbps"), "{line}");
}

#[test]
fn legacy_recovery_keeps_the_preceding_control_snapshot() {
    let mut event = beacon("stall_recovery", 8_000);
    event.control = Some(ClientControlSnapshot {
        generation: Some("11111111-1111-4111-8111-111111111111".into()),
        control_epoch: Some(17),
        sequence: Some(23),
        demand: Some("active".into()),
        render_state: Some("stalled".into()),
        position_ms: Some(90_000),
        buffered_through_ms: Some(90_400),
        observed_download_bps: Some(1_500_000),
        observation: Some(ClientControlObservation {
            dropped_frames: Some(3),
            decoder_state: Some("starved".into()),
            error_code: None,
            error_detail: None,
        }),
    });
    event.control_trigger = Some(ClientControlSnapshot {
        generation: Some("11111111-1111-4111-8111-111111111111".into()),
        control_epoch: Some(17),
        sequence: None,
        demand: Some("active".into()),
        render_state: Some("failed".into()),
        position_ms: Some(90_000),
        buffered_through_ms: Some(90_400),
        observed_download_bps: Some(1_500_000),
        observation: Some(ClientControlObservation {
            dropped_frames: Some(4),
            decoder_state: Some("failed".into()),
            error_code: Some("decoder".into()),
            error_detail: Some("persistent_decode_stall".into()),
        }),
    });

    let persisted = client_playback_event(&event, 7);
    let extra: serde_json::Value =
        serde_json::from_str(persisted.extra.as_deref().expect("control snapshot JSON"))
            .expect("valid control snapshot JSON");
    let accepted = &extra["control_accepted_client"];
    assert_eq!(accepted["sequence"], 23);
    assert_eq!(accepted["render_state"], "stalled");
    assert_eq!(accepted["buffered_through_ms"], 90_400);
    assert_eq!(accepted["observed_download_bps"], 1_500_000);
    assert_eq!(accepted["observation"]["decoder_state"], "starved");
    assert!(accepted["generation"]
        .as_str()
        .is_some_and(|value| value.starts_with("g-")));
    assert!(!persisted
        .extra
        .as_deref()
        .unwrap_or_default()
        .contains("11111111-1111-4111-8111-111111111111"));
    assert_eq!(extra["control_trigger_client"]["render_state"], "failed");
    assert_eq!(
        extra["control_trigger_client"]["observation"]["error_code"],
        "decoder"
    );
    assert_eq!(
        extra["control_trigger_client"]["observation"]["error_detail"],
        "persistent_decode_stall"
    );
    assert!(extra["control_trigger_client"].get("sequence").is_none());
}

#[test]
fn playback_event_reader_caps_the_requested_row_count() {
    assert_eq!(
        bounded_playback_query(PlaybackEventsQuery {
            since: None,
            event: None,
            limit: i64::MAX,
        })
        .limit,
        2_000
    );
}
