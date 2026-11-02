use super::*;
use std::convert::Infallible;

use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use futures_util::{stream, StreamExt};

use crate::transcode::ReopenReason;

fn clock_fixture(offset: Option<(i64, i64)>) -> Arc<ClusterClockGuard> {
    let guard = Arc::new(ClusterClockGuard::new(true));
    // These fixtures exercise the operator-enabled guard; the default
    // (advisory) mode is covered by the advisory test below.
    guard.set_enforced(true);
    if let Some((offset_us, uncertainty_us)) = offset {
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(
            ticket,
            std::collections::BTreeMap::from([(
                "peer".into(),
                plurx_core::cluster::clock::PeerClockOffset::Bounded {
                    offset_us,
                    uncertainty_us,
                    observed_at: Instant::now(),
                },
            )]),
        ));
    }
    guard
}

#[tokio::test(start_paused = true)]
async fn takeover_clock_guard_expiry_requires_bound_and_discards_changed_page() {
    for (offset, allowed) in [
        (Some((0, 1_000)), true),
        (Some((1_999_000, 1_000)), true),
        (Some((1_500_000, 601_000)), false),
        (Some((2_500_000, 1_000)), false),
        (None, false),
    ] {
        let clock = clock_fixture(offset);
        let called = AtomicU64::new(0);
        let called_ref = &called;
        let result = clock_guarded_expiry_scan(&clock, |now_ms| async move {
            called_ref.fetch_add(1, Ordering::Relaxed);
            assert!(now_ms > 0);
            Ok(plurx_core::domain::TakeoverInventory::Expired(vec![
                media_route("expired-clock-fixture"),
            ]))
        })
        .await;
        assert_eq!(result.is_ok(), allowed, "{offset:?}");
        assert_eq!(called.load(Ordering::Relaxed), u64::from(allowed));
    }
    for clock in [Arc::new(ClusterClockGuard::new(false)), clock_fixture(None)] {
        clock.roster(&[]);
        assert!(clock_guarded_expiry_scan(&clock, |_| async {
            Ok(plurx_core::domain::TakeoverInventory::Expired(Vec::new()))
        })
        .await
        .is_ok());
    }
    let clock = clock_fixture(Some((0, 1_000)));
    let error = clock_guarded_expiry_scan(&clock, |_| async {
        tokio::task::yield_now().await;
        clock.roster_failed();
        Ok(plurx_core::domain::TakeoverInventory::Expired(vec![
            media_route("must-not-advance-cursor"),
        ]))
    })
    .await
    .expect_err("discard a page invalidated during the Store await");
    assert!(error.contains("GenerationChanged"));
    assert!(clock.prometheus().contains(
        "plurx_cluster_clock_refusals_total{decision=\"expiry_scan\",cause=\"generation_changed\"} 1\n"
    ));
    let clock = clock_fixture(Some((0, 1_000)));
    assert!(clock_guarded_expiry_scan(&clock, |_| {
        std::future::pending::<Result<plurx_core::domain::TakeoverInventory, StoreError>>()
    })
    .await
    .expect_err("existing inventory timeout remains finite")
    .contains("timed out"));
}

#[tokio::test(start_paused = true)]
async fn takeover_clock_guard_is_advisory_while_enforcement_is_off() {
    for offset in [None, Some((2_500_000, 1_000))] {
        let clock = clock_fixture(offset);
        clock.set_enforced(false);
        assert!(
            acquire_takeover_clock(&clock, "node-old").is_ok(),
            "an unbounded or unknown clock never blocks takeover while off: {offset:?}"
        );
        let called = AtomicU64::new(0);
        let called_ref = &called;
        assert!(clock_guarded_expiry_scan(&clock, |_| async move {
            called_ref.fetch_add(1, Ordering::Relaxed);
            Ok(plurx_core::domain::TakeoverInventory::Expired(Vec::new()))
        })
        .await
        .is_ok());
        assert_eq!(called.load(Ordering::Relaxed), 1);
        let metrics = clock.prometheus();
        assert!(
            metrics.contains(
                "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"unknown\"} 0\n"
            ) && metrics.contains(
                "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"offset\"} 0\n"
            ),
            "nothing is actually refused"
        );
        let cause = if offset.is_none() {
            "unknown"
        } else {
            "offset"
        };
        for decision in ["takeover", "expiry_scan"] {
            assert!(
                metrics.contains(&format!(
                    "plurx_cluster_clock_advisory_refusals_total{{decision=\"{decision}\",cause=\"{cause}\"}} "
                )) && !metrics.contains(&format!(
                    "plurx_cluster_clock_advisory_refusals_total{{decision=\"{decision}\",cause=\"{cause}\"}} 0\n"
                )),
                "{decision} records what enforcement would have refused"
            );
        }
    }
}

struct FixedClockMembership(plurx_core::cluster::clock::ClockMembershipIdentity);

impl plurx_core::cluster::clock::ClockMembershipSource for FixedClockMembership {
    fn current(&self) -> Option<plurx_core::cluster::clock::ClockMembershipIdentity> {
        Some(self.0.clone())
    }
}

fn clock_node(raft_id: u64) -> String {
    format!("00000000-0000-0000-0000-{raft_id:012}")
}

/// Local node 1; voters 1-3; node 4 a committed learner. The listed raft
/// ids are measured within the bound, every other peer is Unknown.
fn learner_clock_fixture(bounded: &[u64], enforced: bool) -> Arc<ClusterClockGuard> {
    use plurx_core::cluster::clock::{ClockMembershipIdentity, PeerClockOffset};
    use plurx_core::cluster::membership::{ActivityPeer, ClockPeerRoster};
    let identity = ClockMembershipIdentity {
        local_node: 1,
        log: (2, 1, 7),
        members: [1, 2, 3, 4].into(),
        voters: [1, 2, 3].into(),
    };
    let guard = Arc::new(ClusterClockGuard::with_membership_source(Arc::new(
        FixedClockMembership(identity.clone()),
    )));
    guard.set_enforced(enforced);
    let roster = ClockPeerRoster {
        membership: Some(identity),
        peers: [2, 3, 4]
            .into_iter()
            .map(|raft_id| ActivityPeer {
                node_id: clock_node(raft_id),
                raft_id,
                http_base: Some(format!("https://node{raft_id}:443")),
                reachable: true,
            })
            .collect(),
    };
    let round = guard
        .roster_for_peer_directory(&roster)
        .expect("exact directory");
    assert!(guard.publish(
        round,
        roster
            .peers
            .iter()
            .map(|peer| {
                let observation = if bounded.contains(&peer.raft_id) {
                    PeerClockOffset::Bounded {
                        offset_us: 0,
                        uncertainty_us: 1_000,
                        observed_at: Instant::now(),
                    }
                } else {
                    PeerClockOffset::Unknown
                };
                (peer.node_id.clone(), observation)
            })
            .collect(),
    ));
    guard
}

/// K-06 review P1. Every voter bounded, learner 4 unobserved: coverage
/// excuses the learner, but R1 is ITS lease, written by its clock. The
/// expiry scan contests only the voter's R2 and takeover of R1 is
/// refused; the cursor still passes R1 so it cannot pin the scanner.
/// Advisory mode contests both and counts what it would have refused.
#[tokio::test(start_paused = true)]
async fn takeover_clock_guard_skips_routes_owned_by_an_unobserved_learner() {
    let learner_route = MediaSessionRoute {
        owner_node_id: clock_node(4),
        incarnation_id: "00000000-0000-4000-8000-0000000000b2".to_owned(),
        ..media_route("owned-by-learner")
    };
    let voter_route = MediaSessionRoute {
        owner_node_id: clock_node(2),
        incarnation_id: "00000000-0000-4000-8000-0000000000b1".to_owned(),
        ..media_route("owned-by-voter")
    };
    let page = || {
        let routes = vec![voter_route.clone(), learner_route.clone()];
        async move { Ok(plurx_core::domain::TakeoverInventory::Expired(routes)) }
    };

    let clock = learner_clock_fixture(&[2, 3], true);
    let scanned = clock_guarded_expiry_scan(&clock, |_| page())
        .await
        .expect("every voter is bounded")
        .expect("takeover enabled");
    assert_eq!(
        scanned
            .contestable
            .iter()
            .map(|route| route.session_id.as_str())
            .collect::<Vec<_>>(),
        ["owned-by-voter"],
        "the unobserved learner's lease is not expired"
    );
    assert_eq!(
        scanned.cursor,
        Some(MediaSessionTakeoverCursor::from(&learner_route)),
        "the cursor passes the skipped route"
    );
    assert_eq!(
        acquire_takeover_clock(&clock, &learner_route.owner_node_id).err(),
        Some(ClockRefusal::Unknown)
    );
    assert!(acquire_takeover_clock(&clock, &voter_route.owner_node_id).is_ok());
    let metrics = clock.prometheus();
    for decision in ["expiry_scan", "takeover"] {
        assert!(
            metrics.contains(&format!(
                "plurx_cluster_clock_refusals_total{{decision=\"{decision}\",cause=\"unknown\"}} 1\n"
            )),
            "{decision}: {metrics}"
        );
    }

    // Measured within the bound, the learner's route is contested too.
    let clock = learner_clock_fixture(&[2, 3, 4], true);
    let scanned = clock_guarded_expiry_scan(&clock, |_| page())
        .await
        .expect("bounded")
        .expect("takeover enabled");
    assert_eq!(scanned.contestable.len(), 2);
    assert!(acquire_takeover_clock(&clock, &learner_route.owner_node_id).is_ok());

    // Advisory: contested as before, each would-be refusal counted.
    let clock = learner_clock_fixture(&[2, 3], false);
    let scanned = clock_guarded_expiry_scan(&clock, |_| page())
        .await
        .expect("advisory")
        .expect("takeover enabled");
    assert_eq!(scanned.contestable.len(), 2);
    assert!(acquire_takeover_clock(&clock, &learner_route.owner_node_id).is_ok());
    let metrics = clock.prometheus();
    for decision in ["expiry_scan", "takeover"] {
        assert!(
            metrics.contains(&format!(
                "plurx_cluster_clock_advisory_refusals_total{{decision=\"{decision}\",cause=\"unknown\"}} 1\n"
            )) && metrics.contains(&format!(
                "plurx_cluster_clock_refusals_total{{decision=\"{decision}\",cause=\"unknown\"}} 0\n"
            )),
            "{decision}: {metrics}"
        );
    }

    // An empty page ends the pass.
    let clock = learner_clock_fixture(&[2, 3], true);
    let scanned = clock_guarded_expiry_scan(&clock, |_| async {
        Ok(plurx_core::domain::TakeoverInventory::Expired(Vec::new()))
    })
    .await
    .expect("bounded")
    .expect("takeover enabled");
    assert!(scanned.contestable.is_empty() && scanned.cursor.is_none());

    // A switch off at the authority refuses the page, before any
    // admission, whatever the caller's gate held.
    let scanned = clock_guarded_expiry_scan(&clock, |_| async {
        Ok(plurx_core::domain::TakeoverInventory::Disabled)
    })
    .await
    .expect("an off switch is an answer, not an error");
    assert!(scanned.is_none());
}

pub(super) fn valid_start_request() -> RemoteStartRequest {
    let incarnation_id = "00000000-0000-4000-8000-0000000000a1".to_owned();
    RemoteStartRequest {
        retained_output: None,
        retained_output_receiver: None,
        candidate_catalog: None,
        candidate_id: None,
        presentation_target: None,
        decoder_caps: None,
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: incarnation_id.clone(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        source_size: 123_456,
        source_mtime: 1_700_000_000,
        typeless_playlist: true,
        library_channel: None,
        request: SessionRequest {
            dv_processing_destination: None,
            sdr_master_codecs: None,
            continuous_media: None,
            quality_catalog: None,
            candidate_context: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            control_sequence: None,
            file_id: 11,
            playback_id: "player-a".to_owned(),
            request_id: Some(incarnation_id),
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 720 },
            start_seconds: 12.5,
            audio_index: Some(1),
            audio_delivery: None,
            audio_claim: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        },
    }
}

#[test]
fn sharing_forwarded_principal_preserves_local_wire_and_refuses_mixed_ownership() {
    use plurx_core::playback_principal::PlaybackPrincipal;
    let start = valid_start_request();
    let prepare = valid_prepare_request();
    let shared = PlaybackPrincipal::sharing(uuid::Uuid::new_v4(), &"a".repeat(64))
        .expect("valid grant/viewer namespace");
    for (is_start, local) in [
        (true, serde_json::to_value(&start).expect("local start")),
        (
            false,
            serde_json::to_value(&prepare).expect("local preparation"),
        ),
    ] {
        let decode = |value: serde_json::Value| -> Option<PlaybackPrincipal> {
            if is_start {
                serde_json::from_value::<RemoteStartRequest>(value)
                    .ok()
                    .map(|r| r.principal)
            } else {
                serde_json::from_value::<RemotePrepareRequest>(value)
                    .ok()
                    .map(|r| r.principal)
            }
        };
        assert!(local["user_id"].is_i64());
        assert!(
            local.get("principal").is_none(),
            "legacy local envelope is unchanged"
        );
        assert_eq!(
            decode(local.clone()),
            Some(PlaybackPrincipal::LocalUser {
                user_id: local["user_id"].as_i64().expect("real local user")
            })
        );
        let mut mixed = local.clone();
        mixed["principal"] = serde_json::to_value(&shared).expect("shared principal");
        assert!(
            decode(mixed.clone()).is_none(),
            "two namespaces cannot authorize one worker"
        );
        mixed
            .as_object_mut()
            .expect("wire object")
            .remove("user_id");
        assert_eq!(decode(mixed.clone()), Some(shared.clone()));
        mixed["principal"]["viewer_key"] = serde_json::json!("A".repeat(64));
        assert!(
            decode(mixed).is_none(),
            "malformed viewer key is not accepted"
        );
        let mut absent = local.clone();
        absent
            .as_object_mut()
            .expect("wire object")
            .remove("user_id");
        assert!(decode(absent).is_none());
        let mut invalid = local.clone();
        invalid["user_id"] = serde_json::json!(0);
        assert!(
            decode(invalid).is_none(),
            "no zero user stands for an absent principal"
        );
        let mut unknown = local;
        unknown["foreign_owner_key"] = serde_json::json!(shared.owner_key());
        assert!(decode(unknown).is_none(), "wire fields remain closed");
    }
    let mut shared_start = start;
    shared_start.principal = shared.clone();
    let wire = serde_json::to_value(&shared_start).expect("shared wire");
    assert!(wire.get("user_id").is_none());
    assert_eq!(
        wire["principal"],
        serde_json::to_value(&shared).expect("typed ownership")
    );
    let recovered: RemoteStartRequest =
        serde_json::from_value(wire).expect("shared recovery recipe");
    assert_eq!(recovered.principal, shared);
}

#[test]
fn retained_receipt_extensions_are_omitted_for_legacy_and_require_negotiation() {
    // The pre-extension strict worker key set, not a permissive JSON map.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct LegacyStart {
        candidate_id: Option<plurx_core::playback::candidate::CandidateId>,
        presentation_target: Option<plurx_core::playback::candidate::PresentationTarget>,
        decoder_caps: Option<crate::playback_control::DecoderCapsSnapshot>,
        protocol_version: i64,
        incarnation_id: String,
        user_id: i64,
        source_size: i64,
        source_mtime: i64,
        typeless_playlist: bool,
        library_channel: Option<serde_json::Value>,
        request: SessionRequest,
    }
    let mut request = valid_start_request();
    let legacy_wire = serde_json::to_value(&request).expect("legacy serialization");
    assert!(legacy_wire.get("retained_output_receiver").is_none());
    assert!(legacy_wire.get("retained_output").is_none());
    assert!(serde_json::from_value::<LegacyStart>(legacy_wire.clone()).is_ok());
    let decoded: RemoteStartRequest =
        serde_json::from_value(legacy_wire).expect("new parser accepts legacy");
    assert_eq!(decoded.retained_output_receiver, None);
    assert!(decoded.retained_output.is_none());
    request.retained_output_receiver = Some(1);
    let negotiated = serde_json::to_value(&request).expect("negotiated serialization");
    assert!(
        serde_json::from_value::<LegacyStart>(negotiated).is_err(),
        "never send extension to a strict old worker"
    );
    assert!(request.is_valid());
    request.retained_output_receiver = Some(2);
    assert!(!request.is_valid());
    request.retained_output_receiver = None;
    request.retained_output = Some(crate::transcode::RetainedOutputFacts {
        artifact_id: uuid::Uuid::new_v4().to_string(),
        output_identity: "ab".repeat(32),
        average_bps: 8000,
        peak_bps: 12000,
    });
    assert!(
        !request.is_valid(),
        "unnegotiated retained descriptor is not accepted"
    );
    request.retained_output_receiver = Some(1);
    assert!(request.is_valid());
    let mut response = valid_start_response();
    let legacy_response = serde_json::to_value(&response).expect("legacy response");
    assert!(
        legacy_response.get("retained_output").is_none(),
        "new worker with legacy capture emits no extension"
    );
    response.retained_output = request.retained_output;
    assert!(response.is_valid());
    response.vod = false;
    assert!(
        !response.is_valid(),
        "retained VOD proof cannot be replayed onto rolling output"
    );
}

fn valid_prepare_request() -> RemotePrepareRequest {
    RemotePrepareRequest {
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: "00000000-0000-4000-8000-0000000000a1".to_owned(),
        session_id: "00000000-0000-4000-8000-0000000000b1".to_owned(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        expected_owner_epoch: 1,
        expected_predecessor_owner_epoch: None,
    }
}

#[test]
fn remote_prepare_names_only_an_exact_reserved_identity() {
    let request = valid_prepare_request();
    assert!(request.is_valid());

    let value = serde_json::to_value(&request).expect("prepare request JSON");
    let object = value.as_object().expect("prepare request object");
    assert_eq!(object.len(), 5, "the recipe must stay in the durable row");
    assert!(!object.contains_key("request"));
    assert!(!object.contains_key("recipe_json"));

    let mut wrong_epoch = request.clone();
    wrong_epoch.expected_owner_epoch = 2;
    assert!(!wrong_epoch.is_valid());

    // The admitted predecessor epoch is additive: omitted when unknown so
    // an older peer's strict wire still parses the request, carried
    // exactly when sent, and never a non-positive fence.
    let mut fenced = request.clone();
    fenced.expected_predecessor_owner_epoch = Some(3);
    assert!(fenced.is_valid());
    let fenced_wire = serde_json::to_value(&fenced).expect("fenced prepare JSON");
    assert_eq!(fenced_wire["expected_predecessor_owner_epoch"], 3);
    let decoded: RemotePrepareRequest =
        serde_json::from_value(fenced_wire).expect("fenced prepare request");
    assert_eq!(decoded.expected_predecessor_owner_epoch, Some(3));
    fenced.expected_predecessor_owner_epoch = Some(0);
    assert!(!fenced.is_valid());
    let legacy: RemotePrepareRequest =
        serde_json::from_value(value.clone()).expect("an older origin's request");
    assert_eq!(legacy.expected_predecessor_owner_epoch, None);

    let mut extended = value;
    extended
        .as_object_mut()
        .expect("prepare request object")
        .insert("recipe_json".to_owned(), serde_json::json!("forged"));
    assert!(serde_json::from_value::<RemotePrepareRequest>(extended).is_err());
}

fn valid_start_response() -> RemoteStartResponse {
    let session_id = "00000000-0000-4000-8000-0000000000b1".to_owned();
    RemoteStartResponse {
        processed_fel: None,
        processed_dv_profile: None,
        retained_output: None,
        audio_delivery: None,
        playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
        session_id,
        duration_ms: Some(7_200_000),
        start_seconds: 12.5,
        media_origin_seconds: 12.5,
        target_height: 720,
        kind: SessionKind::Transcode { height: 720 },
        encoder: "qsv".to_owned(),
        grade: OutputGrade::Sdr,
        vod: true,
        control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
        activation_generation: Some(0),
    }
}

#[test]
fn remote_start_status_carries_created_ownership_and_legacy_is_conservative() {
    let body = serde_json::to_vec(&valid_start_response()).expect("start response JSON");
    let created = decode_remote_start_response(crate::http::peer_transport::PeerResponse {
        dv_processing_header: None,
        clock_timing: None,
        status: reqwest::StatusCode::CREATED,
        body: body.clone(),
    })
    .expect("created response");
    assert_eq!(
        created.ownership,
        RemoteSessionStartOwnership::Created,
        "201 response carries creation ownership"
    );

    let recovered = decode_remote_start_response(crate::http::peer_transport::PeerResponse {
        dv_processing_header: None,
        clock_timing: None,
        status: reqwest::StatusCode::ALREADY_REPORTED,
        body: body.clone(),
    })
    .expect("recovered response");
    assert_eq!(
        recovered.ownership,
        RemoteSessionStartOwnership::Recovered,
        "208 response carries replay/recovery ownership"
    );
    assert_eq!(created.info.session_id, recovered.info.session_id);

    let mut legacy_info = valid_start_response();
    legacy_info.activation_generation = None;
    let legacy = decode_remote_start_response(crate::http::peer_transport::PeerResponse {
        dv_processing_header: None,
        clock_timing: None,
        status: reqwest::StatusCode::OK,
        body: serde_json::to_vec(&legacy_info).expect("legacy start response JSON"),
    })
    .expect("legacy response");
    assert_eq!(
        legacy.ownership,
        RemoteSessionStartOwnership::LegacyAmbiguous,
        "legacy success remains explicitly ambiguous"
    );
}

#[test]
fn remote_activation_accepts_initial_generation_and_requires_request_claim() {
    let mut request = RemoteActivateRequest {
        target_generation: 0,
        activation: MediaSessionActivation {
            recovery_epoch: String::new(),
            expected_desired_revision: None,
            incarnation_id: "00000000-0000-4000-8000-0000000000a1".to_owned(),
            session_id: "00000000-0000-4000-8000-0000000000b1".to_owned(),
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
            playback_id: "player-a".to_owned(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: Some("request-a".to_owned()),
            request_fingerprint: "11".repeat(32),
            owner_node_id: "node-a".to_owned(),
            lease_expires_at_ms: 20_000,
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: 10_000,
        },
    };
    assert!(request.is_valid_for("node-a"));
    request.activation.request_id = None;
    assert!(!request.is_valid_for("node-a"));
}

fn relay_request(resource: RelayResource, deadline_unix_ms: i64) -> RelayRequest {
    RelayRequest {
        session_id: uuid::Uuid::new_v4().to_string(),
        resource,
        deadline_unix_ms,
        headers: RelayHeaders::default(),
    }
}

#[test]
fn private_media_relay_refuses_path_and_identity_aliases() {
    let mut child = crate::vodserve::ChildMediaRequest {
        role: "video".into(),
        rendition: "a".repeat(64),
        kind: "segment".into(),
        object: "12.m4s".into(),
    };
    assert!(RelayResource::ChildSegment {
        child: child.clone()
    }
    .is_valid());
    assert!(RelayResource::ChildPlaylist {
        role: "audio".into(),
        rendition: child.rendition.clone()
    }
    .is_valid());
    assert!(!RelayResource::ChildPlaylist {
        role: "subtitle".into(),
        rendition: child.rendition.clone()
    }
    .is_valid());
    assert!(!RelayResource::ChildPlaylist {
        role: "video".into(),
        rendition: "../cached".into()
    }
    .is_valid());
    assert_eq!(child.media_name().as_deref(), Some("seg00012.m4s"));
    for object in [
        "../12.m4s",
        "012.m4s",
        "-1.m4s",
        "12.m4s/extra",
        "9999999999.m4s",
    ] {
        child.object = object.into();
        assert!(!RelayResource::ChildSegment {
            child: child.clone()
        }
        .is_valid());
        assert!(child.media_name().is_none());
    }
    child.kind = "init".into();
    child.object = format!("{}.mp4", "b".repeat(64));
    assert!(child.is_valid());
    assert_eq!(child.media_name().as_deref(), Some("init.mp4"));
    child.object = format!("{}.mp4", "B".repeat(64));
    assert!(!child.is_valid());
    child.object = format!("{}.mp4", "b".repeat(64));
    child.role = "subtitles".into();
    assert!(!child.is_valid());
    child.role = "audio".into();
    child.rendition = "../cached".into();
    assert!(!child.is_valid());
    assert!(!RelayResource::Segment {
        segment: "video/child/12.m4s".into()
    }
    .is_valid());
    let now = 1_700_000_000_000_i64;
    child.rendition = "a".repeat(64);
    let request = relay_request(RelayResource::ChildSegment { child }, now + 90_000);
    assert_eq!(
        request.owner_budget_at(now),
        Some(RELAY_SEGMENT_MAX_LIFETIME)
    );
    assert!(!request.deadline_is_plausible_at(now));
}

#[test]
fn relay_resource_deadlines_are_bounded_by_the_public_request_class() {
    let now = 1_700_000_000_000_i64;
    let short = relay_request(RelayResource::Status, now.saturating_add(90_000));
    assert_eq!(
        short.transport_budget_at(now),
        Some(RELAY_SHORT_MAX_LIFETIME),
        "an authenticated peer cannot turn status into an arbitrary wait"
    );
    assert_eq!(short.owner_budget_at(now), Some(RELAY_SHORT_MAX_LIFETIME));
    assert!(
        !short.deadline_is_plausible_at(now),
        "the authenticated envelope rejects a deadline beyond its resource ceiling"
    );

    let playlist = relay_request(RelayResource::VideoPlaylist, now.saturating_add(90_000));
    assert_eq!(
        playlist.owner_budget_at(now),
        Some(RELAY_PLAYLIST_MAX_LIFETIME)
    );
    let segment = relay_request(
        RelayResource::Segment {
            segment: "segment-1.m4s".to_owned(),
        },
        now.saturating_add(90_000),
    );
    assert_eq!(
        segment.owner_budget_at(now),
        Some(RELAY_SEGMENT_MAX_LIFETIME)
    );
}

#[test]
fn relay_resource_deadline_preserves_the_earlier_origin_boundary() {
    let origin_now = 1_700_000_000_000_i64;
    let origin_deadline = origin_now.saturating_add(4_000);
    let request = relay_request(RelayResource::Status, origin_deadline);
    assert_eq!(
        request.transport_budget_at(origin_now),
        Some(Duration::from_secs(4))
    );

    // At 1.25 seconds of real elapsed time, model the maximum accepted
    // owner clock lag: its wall clock is still 0.75 seconds behind the
    // origin's send timestamp. Subtracting the allowance reconstructs the
    // same real absolute deadline as the ingress transport.
    let owner_now = origin_now.saturating_sub(750);
    let owner_remaining = request
        .owner_budget_at(owner_now)
        .expect("the origin deadline remains live");
    assert_eq!(owner_remaining, Duration::from_millis(2_750));

    let synchronized_owner_now = origin_now.saturating_add(1_250);
    assert_eq!(
        request.owner_budget_at(synchronized_owner_now),
        Some(Duration::from_millis(750)),
        "accepted skew is conservatively removed instead of becoming extra owner work"
    );
    assert!(
        request.owner_budget_at(origin_deadline).is_none(),
        "publication cannot start after ingress abandonment"
    );
}

#[test]
fn passive_presence_relay_resource_round_trips_and_bounds_its_identity() {
    let resource = RelayResource::PassivePresence {
        user: "[\"user_id\",1]".into(),
        player: "jellyfin:player".into(),
        request: "11111111-1111-4111-8111-111111111111".into(),
    };
    let wire = serde_json::to_value(&resource).expect("relay resource");
    assert_eq!(wire["resource"], "passive_presence");
    let decoded: RelayResource = serde_json::from_value(wire).expect("round trip");
    assert!(decoded.is_valid());
    assert_eq!(decoded.max_lifetime(), RELAY_SHORT_MAX_LIFETIME);
    let long = "x".repeat(257);
    for bad in ["", "  ", "line\nbreak", long.as_str()] {
        let invalid = RelayResource::PassivePresence {
            user: "[\"user_id\",1]".into(),
            player: bad.into(),
            request: "request".into(),
        };
        assert!(!invalid.is_valid(), "{bad:?}");
    }
}

#[test]
fn relay_request_validation_rejects_a_far_future_authenticated_deadline() {
    let now = unix_ms();
    let valid = relay_request(
        RelayResource::Status,
        now.saturating_add(
            i64::try_from(
                (RELAY_SHORT_MAX_LIFETIME + RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE).as_millis(),
            )
            .expect("relay lifetime fits in i64"),
        ),
    );
    assert!(valid.is_valid());

    let far_future = relay_request(RelayResource::Status, now.saturating_add(60_000));
    assert!(!far_future.is_valid());
}

pub(super) fn media_route(session_id: &str) -> MediaSessionRoute {
    MediaSessionRoute {
        recovery_epoch: String::new(),
        incarnation_id: format!("incarnation-{session_id}"),
        session_id: session_id.to_owned(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        playback_id: "player-c".to_owned(),
        request_fingerprint: "c".repeat(64),
        owner_node_id: "node-c".to_owned(),
        owner_epoch: 1,
        lease_expires_at_ms: unix_ms().saturating_add(10_000),
        state: "active".to_owned(),
        terminal_reason: None,
        publication_ready_at_ms: 0,
        recipe_json: "{}".to_owned(),
        response_json: "{}".to_owned(),
        produced_playable_through_ms: 0,
        fetched_through_ms: 0,
        media_origin_ms: 0,
        media_sequence: 0,
        discontinuity_sequence: 0,
        updated_at_ms: unix_ms(),
        drain_deadline_ms: None,
    }
}

fn owned_lease(session_id: &str) -> OwnedMediaSessionLease {
    OwnedMediaSessionLease {
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
        incarnation_id: format!("incarnation-{session_id}"),
        session_id: session_id.to_owned(),
        owner_epoch: 1,
        lease_expires_at_ms: unix_ms().saturating_add(10_000),
        drain_deadline_ms: None,
    }
}

struct ProbeTakeoverWorker {
    id: &'static str,
    events: Arc<StdMutex<Vec<String>>>,
    stopped: Option<Arc<tokio::sync::Notify>>,
    settled: bool,
}

impl ProbeTakeoverWorker {
    fn record(&self, event: &str) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("worker:{}:{event}", self.id));
    }
}

impl Drop for ProbeTakeoverWorker {
    fn drop(&mut self) {
        if !self.settled {
            // Production Drop starts exact detached teardown. Recording
            // that transition makes cancellation/panic ownership visible
            // without needing a TranscodeManager fixture in every script.
            self.record("drop-teardown");
        }
    }
}

impl TakeoverWorkerLifecycle for ProbeTakeoverWorker {
    fn retain_until(&mut self, _expires_at_ms: i64, _monotonic_expiry: tokio::time::Instant) {
        self.record("retain");
    }

    fn stop(mut self, reason: &'static str) -> BoxFuture<'static, ()> {
        self.settled = true;
        let events = Arc::clone(&self.events);
        let stopped = self.stopped.take();
        let id = self.id;
        Box::pin(async move {
            events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("worker:{id}:stop:{reason}"));
            if let Some(stopped) = stopped {
                stopped.notify_one();
            }
        })
    }

    fn stop_and_retain_until(
        mut self,
        reason: &'static str,
        expires_at_ms: i64,
        _monotonic_expiry: tokio::time::Instant,
    ) -> BoxFuture<'static, ()> {
        self.settled = true;
        let events = Arc::clone(&self.events);
        let stopped = self.stopped.take();
        let id = self.id;
        Box::pin(async move {
            events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("worker:{id}:stop-retain:{reason}:{expires_at_ms}"));
            if let Some(stopped) = stopped {
                stopped.notify_one();
            }
        })
    }

    fn publish(mut self) {
        self.settled = true;
        self.record("publish");
    }
}

struct ProbeTakeoverAdoption(&'static str);

struct ScriptedPause {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct ScriptedChildDrop(Arc<StdMutex<Vec<String>>>);

impl Drop for ScriptedChildDrop {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("creation-child:drop".to_owned());
    }
}

impl ScriptedPause {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        })
    }

    async fn wait(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

enum ScriptedOutcome<T> {
    Ready(T),
    Error(&'static str),
    Never,
    Panic(&'static str),
    Paused(Arc<ScriptedPause>),
}

impl<T> ScriptedOutcome<T> {
    async fn resolve(self) -> Result<T, StoreError> {
        match self {
            Self::Ready(value) => Ok(value),
            Self::Error(message) => Err(StoreError::Database(message.to_owned())),
            Self::Never => std::future::pending().await,
            Self::Panic(message) => panic!("{message}"),
            Self::Paused(pause) => {
                pause.wait().await;
                std::future::pending().await
            }
        }
    }
}

struct ScriptedTakeoverIo {
    events: Arc<StdMutex<Vec<String>>>,
    claims: StdMutex<std::collections::VecDeque<ScriptedOutcome<Option<MediaSessionRoute>>>>,
    replays: StdMutex<std::collections::VecDeque<ScriptedOutcome<Option<MediaSessionRoute>>>>,
    reads: StdMutex<std::collections::VecDeque<ScriptedOutcome<Option<MediaSessionRoute>>>>,
    pins: StdMutex<std::collections::VecDeque<bool>>,
    adoptions: StdMutex<std::collections::VecDeque<bool>>,
    renewals: StdMutex<std::collections::VecDeque<ScriptedOutcome<bool>>>,
    cache_results: StdMutex<std::collections::VecDeque<bool>>,
    next_generation: AtomicU64,
    fail_clock_on_initial_claim: Option<Arc<ClusterClockGuard>>,
}

impl ScriptedTakeoverIo {
    fn new(events: Arc<StdMutex<Vec<String>>>) -> Self {
        Self {
            events,
            claims: StdMutex::new(std::collections::VecDeque::new()),
            replays: StdMutex::new(std::collections::VecDeque::new()),
            reads: StdMutex::new(std::collections::VecDeque::new()),
            pins: StdMutex::new(std::collections::VecDeque::new()),
            adoptions: StdMutex::new(std::collections::VecDeque::new()),
            renewals: StdMutex::new(std::collections::VecDeque::new()),
            cache_results: StdMutex::new(std::collections::VecDeque::new()),
            next_generation: AtomicU64::new(10),
            fail_clock_on_initial_claim: None,
        }
    }

    fn record(&self, event: impl Into<String>) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.into());
    }

    fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn allow_publication(&self) {
        self.pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(true);
        self.renewals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Ready(true));
        self.adoptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(true);
    }
}

impl TakeoverSettlementIo<ProbeTakeoverAdoption, ProbeTakeoverWorker> for ScriptedTakeoverIo {
    fn abort_inherited_preparation<'a>(
        &'a self,
        _route: &'a MediaSessionRoute,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn route_generation(&self, _session_id: &str) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.record(format!("generation:{generation}"));
        generation
    }

    fn claim<'a>(
        &'a self,
        _claim: &'a MediaSessionTakeover,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        Box::pin(async move {
            self.record("claim");
            if let Some(clock) = self.fail_clock_on_initial_claim.as_ref() {
                clock.roster_failed();
            }
            let outcome = self
                .claims
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("scripted claim result");
            outcome.resolve().await
        })
    }

    fn replay<'a>(
        &'a self,
        _claim: &'a MediaSessionTakeover,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        Box::pin(async move {
            self.record("replay");
            let outcome = self
                .replays
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("scripted replay result");
            outcome.resolve().await
        })
    }

    fn read<'a>(
        &'a self,
        _incarnation_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        Box::pin(async move {
            self.record("read");
            let outcome = self
                .reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("scripted exact read result");
            outcome.resolve().await
        })
    }

    fn pin<'a>(
        &'a self,
        _provisional_id: &'a str,
        _route: &'a MediaSessionRoute,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            self.record("pin");
            Ok(self
                .pins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("scripted pin result"))
        })
    }

    fn adopt<'a>(
        &'a self,
        _provisional_id: &'a str,
        durable_session_id: &'a str,
        worker: ProbeTakeoverWorker,
        adoption: ProbeTakeoverAdoption,
    ) -> BoxFuture<'a, Result<ProbeTakeoverWorker, ProbeTakeoverWorker>> {
        Box::pin(async move {
            self.record(format!("adopt:{}", adoption.0));
            let adopted = self
                .adoptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("scripted adoption result");
            if adopted {
                worker.record(&format!("adopted:{durable_session_id}"));
                Ok(worker)
            } else {
                Err(worker)
            }
        })
    }

    fn renew_first<'a>(
        &'a self,
        route: &'a MediaSessionRoute,
        local_session_id: &'a str,
        lease_expires_at_ms: i64,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        Box::pin(async move {
            self.record(format!("renew:{local_session_id}"));
            let outcome = self
                .renewals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("scripted renewal result");
            let won = outcome.resolve().await?;
            Ok(won.then(|| {
                let mut renewed = route.clone();
                renewed.lease_expires_at_ms = lease_expires_at_ms;
                renewed
            }))
        })
    }

    fn seed<'a>(&'a self, _route: &'a MediaSessionRoute) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.record("seed");
        })
    }

    fn cache<'a>(
        &'a self,
        _route: MediaSessionRoute,
        observed_generation: u64,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let accepted = self
                .cache_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .unwrap_or(true);
            self.record(format!("cache:{observed_generation}:{accepted}"));
            accepted
        })
    }
}

fn scripted_takeover(
    events: Arc<StdMutex<Vec<String>>>,
) -> (
    PendingTakeoverSettlement<ProbeTakeoverWorker, ProbeTakeoverAdoption>,
    MediaSessionRoute,
) {
    let now_ms = unix_ms();
    let mut original = media_route("session-scripted-takeover");
    original.incarnation_id = "00000000-0000-4000-8000-0000000000d1".to_owned();
    original.owner_node_id = "node-old".to_owned();
    original.lease_expires_at_ms = now_ms.saturating_sub(1);
    original.updated_at_ms = now_ms.saturating_sub(2);
    let claim = MediaSessionTakeover {
        incarnation_id: original.incarnation_id.clone(),
        expected_owner_node_id: original.owner_node_id.clone(),
        expected_owner_epoch: original.owner_epoch,
        next_owner_node_id: "node-new".to_owned(),
        now_ms,
        lease_expires_at_ms: now_ms.saturating_add(60_000),
    };
    let mut winner = original.clone();
    winner.owner_node_id = claim.next_owner_node_id.clone();
    winner.owner_epoch = claim.expected_owner_epoch.saturating_add(1);
    winner.lease_expires_at_ms = claim.lease_expires_at_ms;
    winner.discontinuity_sequence = original.discontinuity_sequence.saturating_add(1);
    winner.updated_at_ms = claim.now_ms;
    (
        PendingTakeoverSettlement {
            original,
            claim,
            provisional_id: "provisional-scripted".to_owned(),
            worker: ProbeTakeoverWorker {
                id: "worker-a",
                events,
                stopped: None,
                settled: false,
            },
            adoption: ProbeTakeoverAdoption("adoption-a"),
            monotonic_expiry: tokio::time::Instant::now() + Duration::from_secs(60),
            claim_cache_generation: 9,
            metric: TakeoverMetricGuard::new(),
        },
        winner,
    )
}

fn terminal_relay_request() -> crate::playback_control::ControlRelayRequest {
    let generation = uuid::Uuid::new_v4().to_string();
    crate::playback_control::ControlRelayRequest {
        session_id: uuid::Uuid::new_v4().to_string(),
        generation: generation.clone(),
        expected_owner_node_id: "node-terminal".to_owned(),
        expected_owner_epoch: 1,
        deadline_unix_ms: unix_ms().saturating_add(4_000),
        control: crate::playback_control::ControlRequestV1 {
            intent: None,
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation,
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 7,
            demand: crate::playback_control::PlaybackDemand::End,
            position_ms: 10_000,
            buffered_from_ms: Some(9_000),
            buffered_through_ms: 25_000,
            playback_rate: 0.0,
            render_state: crate::playback_control::RenderState::Ended,
            seek_target_ms: None,
            observed_download_bps: Some(8_000_000),
            selection: crate::playback_control::ClientSelection {
                quality: crate::playback_control::QualitySelection::Auto {
                    height: None,
                    candidate_id: None,
                },
                audio_track: Some(0),
                subtitle: crate::playback_control::SubtitleSelection {
                    mode: crate::playback_control::SubtitleMode::Off,
                    track: None,
                },
                audio_offset_ms: 0,
                codec: crate::playback_control::CodecPolicy::Auto,
                dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
            },
            capabilities: Some(crate::playback_control::DynamicCapabilities {
                presentation_target: None,
                decoder_caps: None,
                platform: crate::playback_control::ClientPlatform::Web,
                max_height: 2160,
                codecs: vec![crate::playback_control::CodecPolicy::H264],
                dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                dual_player_preparation: false,
            }),
            observation: None,
            acknowledgement: None,
            supported_actions: None,
        },
    }
}

fn terminal_relay_response(
    request: &crate::playback_control::ControlRelayRequest,
) -> crate::playback_control::ControlResponseV1 {
    let server_time_unix_ms = unix_ms();
    crate::playback_control::ControlResponseV1 {
        protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
        generation: request.generation.clone(),
        control_epoch: 1,
        accepted_sequence: request.control.sequence,
        server_time_unix_ms,
        lease: crate::playback_control::PlaybackLeaseView {
            state: "ended".to_owned(),
            renew_after_ms: crate::playback_control::NEXT_EXCHANGE_MS,
            expires_at_unix_ms: server_time_unix_ms,
        },
        delivery: crate::playback_control::DeliveryView {
            presentation: "vod".to_owned(),
            producer_state: "complete".to_owned(),
            produced_through_ms: Some(7_200_000),
            fetched_through_ms: 25_000,
            delivered_bps: None,
            delivered_idle_ms: None,
            recent_producer_speed: None,
            client_runway_ms: 15_000,
            admitted: Some(true),
            producer_decision: None,
            hold_reason: None,
            subtitle_readiness: None,
            subtitle_revision: None,
            preparation: None,
            owner_node_hash: "n-0123456789abcdef".to_owned(),
            owner_epoch: 1,
        },
        effective_selection: crate::playback_control::EffectiveSelection {
            candidate_id: None,
            quality_auto: true,
            height: 1080,
            audio_track: Some(0),
            subtitle_burn: None,
            audio_offset_ms: 0,
            codec: "source".to_owned(),
            dynamic_range: Some("sdr".to_owned()),
        },
        action: crate::playback_control::ControlAction::None,
    }
}

#[test]
fn auto_candidate_parser_floor_remote_context_is_retained_but_not_executed() {
    let mut request = valid_start_request();
    assert!(request.is_valid());
    request.decoder_caps = Some(crate::playback_control::DecoderCapsSnapshot {
        revision: 4,
        video: vec![],
    });
    assert!(
        !request.is_valid(),
        "explicit decoder loss must not be ignored without a candidate"
    );
    assert!(!takeover_recipe_is_valid(&request));
    let snapshot = request
        .decoder_caps
        .as_mut()
        .expect("decoder snapshot fixture");
    snapshot
        .video
        .push(crate::playback_control::RuntimeVideoConstraint {
            codec: "h264".to_owned(),
            profiles: vec!["high".to_owned()],
            available: false,
            dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
            dv_profiles: vec![],
            max_width: Some(1920),
            max_height: Some(1080),
            max_frame_rate: None,
            max_bitrate_bps: None,
        });
    assert!(
        !request.is_valid(),
        "all-unavailable means decoder loss too"
    );
    request
        .decoder_caps
        .as_mut()
        .expect("decoder snapshot fixture")
        .video[0]
        .available = true;
    assert!(
        request.is_valid(),
        "ordinary negotiated routes retain usable caps"
    );
    let mut rolling = request.clone();
    rolling.request.presentation = crate::transcode::Presentation::Live;
    assert!(takeover_recipe_is_valid(&rolling));
    rolling
        .decoder_caps
        .as_mut()
        .expect("decoder snapshot fixture")
        .video[0]
        .available = false;
    assert!(!takeover_recipe_is_valid(&rolling));
    rolling
        .decoder_caps
        .as_mut()
        .expect("decoder snapshot fixture")
        .video
        .clear();
    assert!(!takeover_recipe_is_valid(&rolling));
    let mut missing_grade = request.clone();
    missing_grade
        .decoder_caps
        .as_mut()
        .expect("decoder snapshot fixture")
        .video[0]
        .dynamic_ranges
        .clear();
    assert!(!missing_grade.is_valid());
    missing_grade.request.presentation = crate::transcode::Presentation::Live;
    assert!(!takeover_recipe_is_valid(&missing_grade));
    let recipe_digest = [0x12; 32];
    let candidate_id =
        plurx_core::playback::candidate::CandidateId::for_recipe_digest(recipe_digest);
    request.candidate_id = Some(candidate_id);
    assert!(
        !request.is_valid(),
        "an identity and decoder snapshot alone cannot authorize a candidate"
    );
    request.candidate_catalog = Some(CandidateCatalogContext {
        caps: serde_json::from_value(serde_json::json!({
            "v": 2, "video": [{"codec":"h264", "present":["sdr"]}],
            "audio":["aac"], "containers":["mp4"], "transports":["hls"]
        }))
        .expect("retained capabilities"),
        candidate: plurx_core::playback::candidate::QualityCandidate {
            processed_dv: false,
            id: candidate_id,
            recipe_digest,
            route: plurx_core::playback::candidate::CandidateRoute::Encode,
            planned_codec: None,
            normalized_geometry: true,
            width: 1920,
            height: 1080,
            target_height: 1080,
            average_bps: None,
            peak_bps: None,
            grade: plurx_core::transcode::OutputGrade::Sdr,
            decoder_compatible: true,
            complete_cache: false,
            sustainable: true,
        },
        binding: crate::media_pool::PlanningBinding {
            generation: 1,
            source_digest: "0".repeat(64),
        },
    });
    request.presentation_target = Some(plurx_core::playback::candidate::PresentationTarget {
        width_px: 2400,
        height_px: 1600,
        revision: 3,
    });
    let value = serde_json::to_value(&request).expect("valid parser-floor test fixture");
    let parsed: RemoteStartRequest =
        serde_json::from_value(value.clone()).expect("valid parser-floor test fixture");
    assert_eq!(
        serde_json::to_value(&parsed).expect("valid parser-floor test fixture"),
        value
    );
    assert!(
        parsed.is_valid(),
        "the completed protocol accepts a usable snapshot structurally; exact recipe revalidation still precedes dispatch"
    );
    assert!(
        !takeover_recipe_is_valid(&parsed),
        "VOD is not a rolling takeover recipe"
    );
    let mut missing_snapshot = parsed.clone();
    missing_snapshot.decoder_caps = None;
    assert!(
        !missing_snapshot.is_valid(),
        "candidate execution requires its snapshot"
    );
    let mut unknown = value;
    unknown["unexpected"] = true.into();
    assert!(serde_json::from_value::<RemoteStartRequest>(unknown).is_err());
    let legacy =
        serde_json::to_value(valid_start_request()).expect("valid parser-floor test fixture");
    assert!(legacy.get("candidate_id").is_none());
    assert!(legacy.get("presentation_target").is_none());
    assert!(legacy.get("decoder_caps").is_none());
}

/// ffmpeg's HLS muxer carries the segment number through a C `int`. A
/// number past `i32::MAX` is truncated into a negative filename that no
/// allowlist accepts, so every segment of that generation 404s — reached
/// by an ordinary second failover during one long film.
#[test]
fn the_published_start_number_stays_inside_what_ffmpeg_can_name() {
    // The floor, not the replicated sequence, is what a successor starts
    // from: the predecessor's frontier is seconds stale by design, so
    // segments it produced after its last heartbeat are numbered above
    // the sequence anyone replicated.
    assert_eq!(
        takeover_start_number(41, 2),
        Some(TAKEOVER_SEQUENCE_STRIDE * 2)
    );
    assert_eq!(
        takeover_start_number(TAKEOVER_SEQUENCE_STRIDE * 2 - 1, 2),
        Some(TAKEOVER_SEQUENCE_STRIDE * 2),
        "a predecessor sequence anywhere inside the prior epoch's range \
             cannot pull the successor below its own floor"
    );
    assert_eq!(
        takeover_start_number(3, 3).expect("second successor")
            - takeover_start_number(3, 2).expect("first successor"),
        TAKEOVER_SEQUENCE_STRIDE,
        "each epoch owns a strictly higher, equally wide range"
    );

    // The boundary is the whole point of the check: the epoch either side
    // of it must be decided differently.
    let ceiling = i64::from(i32::MAX);
    let last_epoch = ceiling / TAKEOVER_SEQUENCE_STRIDE;
    assert!(
        takeover_start_number(0, last_epoch).is_some_and(|start| start <= ceiling),
        "epoch {last_epoch} is the last representable one and must be allowed"
    );
    assert_eq!(
        takeover_start_number(0, last_epoch + 1),
        None,
        "the first epoch past the ceiling is refused, not truncated"
    );
    assert_eq!(takeover_start_number(i64::MAX, 2), None);
    assert_eq!(takeover_start_number(0, i64::MAX), None);
}

/// `fetched_through_ms` advances to the END of a segment as soon as the
/// client requests it, so an owner that dies mid-response has published a
/// frontier well past what the viewer holds. Overlapping by less than one
/// whole segment of this session's own shape leaves a span of media that
/// no generation produces, and the viewer sees a hard cut at the
/// discontinuity.
#[test]
fn a_successor_overlaps_a_whole_segment_of_its_own_shape() {
    let mut route = media_route("session-resume");
    route.media_origin_ms = 90_000;
    route.produced_playable_through_ms = 600_000;
    route.fetched_through_ms = 600_000;

    let copy = SessionKind::Copy {
        convert_dolby_vision: false,
        dual_layer_as_copied: false,
        aac: false,
        preserve_dolby_vision: false,
    };
    let (copy_offset, copy_restart) = takeover_resume(&route, &copy);
    let copy_overlap = route.fetched_through_ms - copy_offset;
    assert!(
        copy_overlap >= i64::from(plurx_core::transcode::COPY_SEGMENT_MAX_SECS) * 1_000,
        "a remux must re-cover the longest segment it could have been \
             serving when its owner died, not a fixed two seconds: {copy_overlap}"
    );
    assert_eq!(
        copy_restart,
        route.media_origin_ms + copy_offset,
        "the restart is measured from the row's write-once origin"
    );

    let transcode = SessionKind::Transcode { height: 1080 };
    let (transcode_offset, _) = takeover_resume(&route, &transcode);
    assert!(
        route.fetched_through_ms - transcode_offset
            >= i64::from(plurx_core::transcode::SEGMENT_SECONDS) * 1_000
    );
    assert!(
        transcode_offset > copy_offset,
        "a two-second segment does not pay a fifteen-second segment's overlap"
    );

    // Never behind the start of the film, and never past what was made.
    route.fetched_through_ms = 1_000;
    route.produced_playable_through_ms = 1_000;
    assert_eq!(takeover_resume(&route, &copy), (0, route.media_origin_ms));
    route.produced_playable_through_ms = 0;
    route.fetched_through_ms = 0;
    assert_eq!(takeover_resume(&route, &transcode).0, 0);
}

/// A rolling recipe the takeover path can act on is a transition, however
/// long its lease has been dead. There is no deadline here on purpose: the
/// gate that replaces a dead node's sessions is itself shut while that node
/// is unresolved, so a survivor cannot know when a successor stops being
/// possible — only whether it was ever possible at all.
#[test]
fn an_eligible_rolling_route_is_a_transition_however_long_it_has_been_dead() {
    let mut route = eligible_route("session-transition");

    assert!(matches!(
        classify_owner_loss(&route, EXPIRED_NOW_MS),
        OwnerLoss::Transitioning(_)
    ));

    route.lease_expires_at_ms = 1;
    assert!(
        matches!(
            classify_owner_loss(&route, EXPIRED_NOW_MS),
            OwnerLoss::Transitioning(_)
        ),
        "a lease dead since the epoch still says nothing about whether a successor is coming"
    );
}

/// The refusals that decide it are the takeover path's own, read off the
/// durable recipe. Each is permanent: no amount of waiting turns a VOD
/// handle, an EVENT playlist, or an exhausted sequence space into
/// something a successor can replace.
#[test]
fn a_route_the_takeover_path_can_never_accept_is_unrecoverable() {
    let base = eligible_route("session-lost");
    let eligible = eligible_recipe(&base);

    let mut vod = base.clone();
    vod.recipe_json = serde_json::to_string(&RemoteStartRequest {
        request: SessionRequest {
            dv_processing_destination: None,
            sdr_master_codecs: None,
            continuous_media: None,
            quality_catalog: None,
            candidate_context: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            presentation: crate::transcode::Presentation::Vod,
            ..eligible.request.clone()
        },
        ..eligible.clone()
    })
    .expect("serialize vod recipe");
    assert!(
        matches!(
            classify_owner_loss(&vod, EXPIRED_NOW_MS),
            OwnerLoss::Unrecoverable(_)
        ),
        "an immutable VOD handle is never replaced by a renumbered successor"
    );

    let mut event = base.clone();
    event.recipe_json = serde_json::to_string(&RemoteStartRequest {
        typeless_playlist: false,
        ..eligible.clone()
    })
    .expect("serialize event recipe");
    assert!(
        matches!(
            classify_owner_loss(&event, EXPIRED_NOW_MS),
            OwnerLoss::Unrecoverable(_)
        ),
        "an EVENT playlist cannot be renumbered, so nothing will take it over"
    );

    let mut exhausted = base.clone();
    exhausted.recipe_json = event.recipe_json.clone();
    exhausted.recipe_json = serde_json::to_string(&eligible).expect("serialize");
    exhausted.owner_epoch = i64::MAX;
    assert!(
        matches!(
            classify_owner_loss(&exhausted, EXPIRED_NOW_MS),
            OwnerLoss::Unrecoverable(_)
        ),
        "an exhausted epoch space is permanent too"
    );

    let mut unreadable = base.clone();
    unreadable.recipe_json = "{}".to_owned();
    assert!(
        matches!(
            classify_owner_loss(&unreadable, EXPIRED_NOW_MS),
            OwnerLoss::Unrecoverable(_)
        ),
        "a recipe no successor could act on is refused rather than waited on"
    );

    let mut foreign = base.clone();
    foreign.recipe_json = serde_json::to_string(&RemoteStartRequest {
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
            user_id: eligible
                .principal
                .local_user_id()
                .expect("local fixture principal")
                + 1,
        },
        ..eligible.clone()
    })
    .expect("serialize foreign recipe");
    assert!(
        matches!(
            classify_owner_loss(&foreign, EXPIRED_NOW_MS),
            OwnerLoss::Unrecoverable(_)
        ),
        "a recipe that no longer describes its route is stale, and staleness does not heal"
    );
}

/// The position offered to a client is the one a successor would restart
/// from, computed by the same overlap. `fetched_through_ms` runs to the
/// *end* of a segment the moment the client asks for it, so handing it
/// back unadjusted would tell a viewer to reopen past media they never
/// saw — up to a whole copy segment of it.
#[test]
fn the_offered_resume_matches_what_a_successor_would_restart_from() {
    let mut route = eligible_route("session-resume");
    route.media_origin_ms = 90_000;
    route.fetched_through_ms = 45_000;
    route.produced_playable_through_ms = 61_000;
    let recipe = eligible_recipe(&route);

    let resume = classify_owner_loss(&route, EXPIRED_NOW_MS).resume();
    let (_, successor_restart_ms) = takeover_resume(&route, &recipe.request.kind);
    assert_eq!(
        resume.film_position_ms, successor_restart_ms,
        "the client is told to reopen exactly where a successor would have resumed"
    );
    assert!(
        resume.film_position_ms
            < route
                .media_origin_ms
                .saturating_add(route.fetched_through_ms),
        "and that is behind the fetched frontier, which overshoots by a whole segment"
    );
    assert_eq!(resume.film_frontier_ms, 90_000 + 61_000);
}

/// A recipe that will not parse still has to produce a usable resume, and
/// the safe direction is early: repeating seen media is recoverable,
/// skipping unseen media is not.
#[test]
fn an_unreadable_recipe_still_resumes_and_errs_early() {
    let mut route = media_route("session-unreadable");
    route.recipe_json = "{}".to_owned();
    route.media_origin_ms = 10_000;
    route.fetched_through_ms = 300_000;
    route.produced_playable_through_ms = 300_000;

    let resume = classify_owner_loss(&route, EXPIRED_NOW_MS).resume();
    assert_eq!(
        resume.film_position_ms,
        10_000 + 300_000 - resume_overlap_ms(None),
        "an unknown shape gets the wider of the two overlaps"
    );
    assert!(
        resume_overlap_ms(None) >= resume_overlap_ms(Some(&SessionKind::Transcode { height: 720 }))
    );
}

/// A session that served nothing, or whose frontier sits behind its own
/// fetch pointer, still resumes inside the film rather than before it.
#[test]
fn a_resume_never_lands_before_the_session_start() {
    let mut route = media_route("session-empty");
    route.media_origin_ms = 90_000;
    route.fetched_through_ms = 0;
    route.produced_playable_through_ms = 0;

    let resume = classify_owner_loss(&route, EXPIRED_NOW_MS).resume();
    assert_eq!(resume.film_position_ms, 90_000);
    assert_eq!(resume.film_frontier_ms, 90_000);
}

/// The compatibility takeover path must not be expanded to VOD or EVENT
/// sessions (remaining-roadmap §4). The VOD refusal is the recipe
/// validator's; the EVENT refusal lives in `attempt_takeover` and is
/// driven there by `attempt_takeover_refuses_an_event_session` in the HTTP
/// tests, which is the only place an `AppState` fixture exists.
#[test]
fn the_takeover_recipe_validator_refuses_vod() {
    let route = eligible_route("session-nonexpansion");
    let eligible = eligible_recipe(&route);
    assert!(takeover_recipe_matches_route(&eligible, &route));

    let vod = RemoteStartRequest {
        request: SessionRequest {
            dv_processing_destination: None,
            sdr_master_codecs: None,
            continuous_media: None,
            quality_catalog: None,
            candidate_context: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            presentation: crate::transcode::Presentation::Vod,
            ..eligible.request.clone()
        },
        ..eligible.clone()
    };
    assert!(
        !takeover_recipe_matches_route(&vod, &route),
        "an immutable VOD handle is never replaced by a renumbered successor"
    );

    // A recipe written before the `typeless_playlist` flag existed
    // defaults to refusing rather than to guessing.
    let legacy: RemoteStartRequest = serde_json::from_value(serde_json::json!({
        "protocol_version": crate::media_pool::PROTOCOL_VERSION,
        "incarnation_id": eligible.incarnation_id,
        "user_id": eligible.principal.local_user_id().expect("local fixture principal"),
        "source_size": eligible.source_size,
        "source_mtime": eligible.source_mtime,
        "request": serde_json::to_value(&eligible.request).expect("request"),
    }))
    .expect("a recipe predating the flag still parses");
    assert!(!legacy.typeless_playlist);
}

const ELIGIBLE_INCARNATION: &str = "00000000-0000-4000-8000-0000000000f1";
/// Later than any fixture lease, so the route's owner has stopped
/// renewing and the recipe is what decides the answer.
const EXPIRED_NOW_MS: i64 = i64::MAX / 2;

fn eligible_route(session_id: &str) -> MediaSessionRoute {
    super::takeover_eligible_route(session_id, ELIGIBLE_INCARNATION)
}

/// The regression that shipped in the first version of this: a committed
/// replacement holds a live lease for the whole of its publication
/// window while it owns and renews. Its recipe is often ineligible — every
/// VOD handle's is, by design — so a classifier that reads only the recipe
/// tells a viewer their session is gone for the 372 seconds of an entirely
/// ordinary handoff.
#[test]
fn a_renewing_successor_awaiting_publication_is_never_owner_loss() {
    let now_ms: i64 = 5_000_000;
    let mut route = media_route("session-publishing");
    // A VOD recipe: nothing will ever take this over, and nothing needs to.
    route.recipe_json = serde_json::to_string(&valid_start_request()).expect("serialize");
    route.publication_ready_at_ms =
        now_ms.saturating_add(plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS);
    route.lease_expires_at_ms = now_ms.saturating_add(1);

    assert!(
        matches!(
            classify_durable_route(Some(route.clone()), "node-other", now_ms),
            DurableRouteResolution::OwnerTransition(_)
        ),
        "the publication fence is the other producer of this classification"
    );
    assert!(
        matches!(
            classify_owner_loss(&route, now_ms),
            OwnerLoss::Transitioning(_)
        ),
        "a live lease is a live owner whatever its recipe says"
    );
    assert!(
        matches!(
            classify_owner_loss(&route, route.lease_expires_at_ms),
            OwnerLoss::Unrecoverable(_)
        ),
        "and once that lease stops being renewed, the recipe decides"
    );
}

#[test]
fn only_durable_prepared_authority_bypasses_the_control_publication_fence() {
    let now_ms = 5_000_000;
    let mut route = media_route("session-prepared-media");
    route.publication_ready_at_ms = MEDIA_SESSION_PUBLICATION_BLOCKED;
    route.lease_expires_at_ms = now_ms + 30_000;
    route.response_json = serde_json::json!({
        "plan_notes": [PREPARED_SUCCESSOR_PLAN_NOTE]
    })
    .to_string();
    assert!(prepared_successor_route(&route));
    let staged = plurx_core::domain::MediaSessionStagedGeneration {
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
            user_id: route
                .principal
                .local_user_id()
                .expect("local test principal"),
        },
        playback_id: route.playback_id.clone(),
        staged_incarnation_id: route.incarnation_id.clone(),
        expected_predecessor_incarnation_id: "predecessor".to_owned(),
        deadline_ms: now_ms + 20_000,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };

    let control = classify_durable_route(Some(route.clone()), &route.owner_node_id, now_ms);
    assert!(matches!(
        control,
        DurableRouteResolution::OwnerTransition(_)
    ));
    assert!(matches!(
        classify_prepared_media_route(control, true, &route.owner_node_id, now_ms),
        DurableRouteResolution::ActiveLocal(_)
    ));
    assert!(matches!(
        classify_prepared_media_route(
            DurableRouteResolution::OwnerTransition(route.clone()),
            false,
            &route.owner_node_id,
            now_ms
        ),
        DurableRouteResolution::OwnerTransition(_)
    ));

    let mut expired = staged;
    expired.deadline_ms = now_ms;
    assert!(matches!(
        classify_prepared_media_route(
            DurableRouteResolution::OwnerTransition(route),
            expired.deadline_ms > now_ms,
            "another-node",
            now_ms
        ),
        DurableRouteResolution::OwnerTransition(_)
    ));
}

fn eligible_recipe(route: &MediaSessionRoute) -> RemoteStartRequest {
    serde_json::from_str(&route.recipe_json).expect("the eligible fixture parses")
}

/// §7.3 requires a candidate to prove the source snapshot rather than
/// assume a replicated row implies a mount. A session whose file row could
/// not be read at creation records an impossible `0/0` snapshot, so it is
/// refused too.
#[test]
fn only_an_unchanged_source_admits_a_takeover() {
    let envelope = valid_start_request();
    let (size, mtime) = (envelope.source_size, envelope.source_mtime);
    assert!(takeover_source_matches(&envelope, size, mtime));
    assert!(!takeover_source_matches(&envelope, size + 1, mtime));
    assert!(!takeover_source_matches(&envelope, size, mtime + 1));

    let unread = RemoteStartRequest {
        source_size: 0,
        source_mtime: 0,
        ..envelope
    };
    assert!(
        !takeover_source_matches(&unread, size, mtime),
        "a session started against a file the placement never read is not recoverable"
    );
}

#[test]
fn takeover_claim_reconciliation_distinguishes_only_exact_authority() {
    let now_ms = 10_000;
    let mut original = media_route("session-claim");
    original.incarnation_id = "00000000-0000-4000-8000-0000000000c1".to_owned();
    original.owner_node_id = "node-old".to_owned();
    original.lease_expires_at_ms = now_ms - 1;
    original.updated_at_ms = now_ms - 2;
    let claim = MediaSessionTakeover {
        incarnation_id: original.incarnation_id.clone(),
        expected_owner_node_id: original.owner_node_id.clone(),
        expected_owner_epoch: original.owner_epoch,
        next_owner_node_id: "node-new".to_owned(),
        now_ms,
        lease_expires_at_ms: now_ms + LEASE_TTL_MS,
    };

    assert_eq!(
        takeover_claim_verdict(&original, &claim, Some(original.clone()), now_ms),
        TakeoverClaimVerdict::Pending,
        "the unchanged predecessor remains ambiguous after a lost reply"
    );

    let mut winner = original.clone();
    winner.owner_node_id = claim.next_owner_node_id.clone();
    winner.owner_epoch += 1;
    winner.discontinuity_sequence += 1;
    winner.lease_expires_at_ms = claim.lease_expires_at_ms;
    winner.updated_at_ms = claim.now_ms;
    assert!(matches!(
        takeover_claim_verdict(&original, &claim, Some(winner.clone()), now_ms),
        TakeoverClaimVerdict::Won(route) if *route == winner
    ));

    let mut renewed_winner = winner.clone();
    renewed_winner.lease_expires_at_ms += LEASE_TTL_MS;
    renewed_winner.updated_at_ms += 1;
    assert!(
        matches!(
            takeover_claim_verdict(
                &original,
                &claim,
                Some(renewed_winner.clone()),
                now_ms,
            ),
            TakeoverClaimVerdict::Won(route) if *route == renewed_winner
        ),
        "the exact target epoch remains ours after a legitimate renewal"
    );

    let mut renewed_predecessor = original.clone();
    renewed_predecessor.lease_expires_at_ms = now_ms + LEASE_TTL_MS;
    assert_eq!(
        takeover_claim_verdict(&original, &claim, Some(renewed_predecessor), now_ms,),
        TakeoverClaimVerdict::Lost,
        "a predecessor renewal makes the immutable expired-row CAS impossible"
    );

    let mut terminal = original.clone();
    terminal.state = "ended".to_owned();
    assert_eq!(
        takeover_claim_verdict(&original, &claim, Some(terminal), now_ms),
        TakeoverClaimVerdict::Lost
    );
    let mut competitor = winner.clone();
    competitor.owner_node_id = "node-other".to_owned();
    assert_eq!(
        takeover_claim_verdict(&original, &claim, Some(competitor), now_ms),
        TakeoverClaimVerdict::Lost
    );
    assert_eq!(
        takeover_claim_verdict(&original, &claim, None, now_ms),
        TakeoverClaimVerdict::Lost
    );
    assert_eq!(
        takeover_claim_verdict(&original, &claim, Some(winner), claim.lease_expires_at_ms,),
        TakeoverClaimVerdict::Lost,
        "a late commit has no authority beyond its immutable lease"
    );
}

#[tokio::test(start_paused = true)]
async fn takeover_reconciliation_retains_one_worker_across_pending_until_publication() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let io = ScriptedTakeoverIo::new(Arc::clone(&events));
    let (pending, winner) = scripted_takeover(Arc::clone(&events));
    let source = pending.original.clone();
    io.replays
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend([ScriptedOutcome::Ready(None), ScriptedOutcome::Ready(None)]);
    io.reads
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend([
            ScriptedOutcome::Ready(Some(source)),
            ScriptedOutcome::Ready(Some(winner)),
        ]);
    io.pins
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(true);
    io.adoptions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(true);
    io.renewals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(ScriptedOutcome::Ready(true));

    reconcile_pending_takeover(&io, pending, None)
        .await
        .expect("scripted takeover publishes");
    assert_eq!(
        io.events(),
        vec![
            "generation:10",
            "replay",
            "generation:11",
            "read",
            "generation:12",
            "replay",
            "generation:13",
            "read",
            "pin",
            "worker:worker-a:retain",
            "renew:provisional-scripted",
            "adopt:adoption-a",
            "worker:worker-a:adopted:session-scripted-takeover",
            "seed",
            "worker:worker-a:publish",
            "cache:13:true",
        ],
        "Pending observations must not dispose the worker, and the winning read's cache generation follows seed/publication"
    );
}

fn scripted_clock_takeover(
    events: Arc<StdMutex<Vec<String>>>,
    clock: &TakeoverClockAdmission,
) -> (
    PendingTakeoverSettlement<ProbeTakeoverWorker, ProbeTakeoverAdoption>,
    MediaSessionRoute,
) {
    let (mut pending, mut winner) = scripted_takeover(events);
    pending.claim = clock.claim(&pending.original, "node-new");
    pending.monotonic_expiry = clock.monotonic_expiry;
    winner.lease_expires_at_ms = pending.claim.lease_expires_at_ms;
    winner.updated_at_ms = pending.claim.now_ms;
    (pending, winner)
}

#[tokio::test(start_paused = true)]
async fn takeover_clock_guard_refuses_only_unsubmitted_claim_and_stops_exact_worker() {
    for (offset, expected) in [
        (None, ClockRefusal::Unknown),
        (Some((1_500_000, 601_000)), ClockRefusal::Offset),
        (Some((2_500_000, 1_000)), ClockRefusal::Offset),
    ] {
        assert_eq!(
            acquire_takeover_clock(&clock_fixture(offset), "node-old").err(),
            Some(expected)
        );
    }
    for clock in [
        Arc::new(ClusterClockGuard::new(false)),
        clock_fixture(Some((1_999_000, 1_000))),
    ] {
        assert!(acquire_takeover_clock(&clock, "node-old").is_ok());
    }
    for recovered in [false, true] {
        let guard = clock_fixture(Some((0, 1_000)));
        let clock = acquire_takeover_clock(&guard, "node-old").expect("original admission");
        let original_now = clock.ticket.now_ms();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, _) = scripted_clock_takeover(Arc::clone(&events), &clock);
        tokio::task::yield_now().await;
        guard.roster_failed();
        if recovered {
            let round = guard.roster(&[]);
            assert!(guard.publish(round, std::collections::BTreeMap::new()));
        }
        let error = settle_initial_takeover_claim(&io, pending, || clock.ticket.revalidate())
            .await
            .expect_err("an old proof cannot be renewed by recovered evidence");
        assert!(error.contains("GenerationChanged"));
        assert_eq!(clock.ticket.now_ms(), original_now);
        assert_eq!(
            io.events(),
            vec!["worker:worker-a:stop:media-session takeover clock refused before submission"],
            "no claim, replay, pin, publication or unrelated worker teardown"
        );
        assert!(guard.prometheus().contains(
            "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"generation_changed\"} 1\n"
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn takeover_clock_guard_preserves_original_24s_lease_and_publication_runway() {
    for preparation in [Duration::from_secs(8), Duration::from_secs(12)] {
        let guard = Arc::new(ClusterClockGuard::new(false));
        let clock = acquire_takeover_clock(&guard, "node-old").expect("standalone admission");
        let original_now = clock.ticket.now_ms();
        let original_monotonic_expiry = clock.monotonic_expiry;
        tokio::time::advance(preparation).await;
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, winner) = scripted_clock_takeover(Arc::clone(&events), &clock);
        assert_eq!(pending.claim.now_ms, original_now);
        assert_eq!(pending.claim.lease_expires_at_ms - original_now, 24_000);
        assert_eq!(pending.monotonic_expiry, original_monotonic_expiry);
        assert_eq!(
            pending.monotonic_expiry - tokio::time::Instant::now(),
            Duration::from_secs(24) - preparation
        );
        io.claims
            .lock()
            .expect("scripted claim")
            .push_back(ScriptedOutcome::Ready(Some(winner)));
        io.allow_publication();
        let result =
            settle_initial_takeover_claim(&io, pending, || clock.ticket.revalidate()).await;
        let published = io.events().iter().any(|event| event.ends_with(":publish"));
        if preparation == Duration::from_secs(8) {
            result.expect("eight-second preparation still has sufficient original runway");
            assert!(published);
            assert!(io
                .events()
                .iter()
                .any(|event| event == "renew:provisional-scripted"));
        } else {
            assert!(
                result.is_err(),
                "insufficient runway never extends the original lease"
            );
            assert!(!published);
            assert!(io
                .events()
                .iter()
                .any(|event| event.contains("stop-retain:")));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn takeover_clock_guard_keeps_submitted_ambiguous_reconciliation_ungated() {
    for mode in ["error", "timeout"] {
        let guard = clock_fixture(Some((0, 1_000)));
        let clock = acquire_takeover_clock(&guard, "node-old").expect("original admission");
        let events = Arc::new(StdMutex::new(Vec::new()));
        let mut io = ScriptedTakeoverIo::new(Arc::clone(&events));
        io.fail_clock_on_initial_claim = Some(Arc::clone(&guard));
        let (pending, winner) = scripted_clock_takeover(Arc::clone(&events), &clock);
        io.claims
            .lock()
            .expect("scripted claim")
            .push_back(if mode == "error" {
                ScriptedOutcome::Error("reply lost after submission and clock failure")
            } else {
                ScriptedOutcome::Never
            });
        io.replays
            .lock()
            .expect("scripted replay")
            .push_back(ScriptedOutcome::Ready(Some(winner)));
        io.allow_publication();
        settle_initial_takeover_claim(&io, pending, || clock.ticket.revalidate())
            .await
            .expect("submitted fixed claim reconciles despite later clock failure");
        assert_eq!(guard.acquire().err(), Some(ClockRefusal::Unknown));
        let events = io.events();
        assert_eq!(events.first().map(String::as_str), Some("claim"));
        assert!(events.iter().any(|event| event == "replay"), "{mode}");
        assert!(events
            .iter()
            .any(|event| event == "renew:provisional-scripted"));
        assert!(events.iter().any(|event| event.ends_with(":publish")));
        assert!(!events.iter().any(|event| event.contains("drop-teardown")));
        assert!(guard.prometheus().contains(
            "plurx_cluster_clock_refusals_total{decision=\"takeover\",cause=\"generation_changed\"} 0\n"
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn ambiguous_initial_claim_error_and_timeout_reconcile_to_one_worker() {
    for mode in ["error", "timeout"] {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, winner) = scripted_takeover(Arc::clone(&events));
        io.claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(if mode == "error" {
                ScriptedOutcome::Error("scripted initial claim error")
            } else {
                ScriptedOutcome::Never
            });
        io.replays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Ready(Some(winner)));
        io.allow_publication();

        settle_initial_takeover_claim(&io, pending, || Ok(()))
            .await
            .expect("ambiguous initial claim must reconcile");
        let events = io.events();
        assert_eq!(events.first().map(String::as_str), Some("claim"));
        assert!(
            events.iter().any(|event| event == "replay"),
            "{mode}: {events:?}"
        );
        assert!(events.iter().any(|event| event.ends_with(":publish")));
        assert!(!events.iter().any(|event| event.contains("drop-teardown")));
    }
}

#[tokio::test(start_paused = true)]
async fn ambiguous_replay_error_and_timeout_fall_through_to_exact_read() {
    for mode in ["error", "timeout"] {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, winner) = scripted_takeover(Arc::clone(&events));
        io.replays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(if mode == "error" {
                ScriptedOutcome::Error("scripted replay error")
            } else {
                ScriptedOutcome::Never
            });
        io.reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Ready(Some(winner)));
        io.allow_publication();

        reconcile_pending_takeover(&io, pending, None)
            .await
            .expect("ambiguous replay must reconcile through exact read");
        let events = io.events();
        assert!(events.iter().any(|event| event == "replay"));
        assert!(
            events.iter().any(|event| event == "read"),
            "{mode}: {events:?}"
        );
        assert!(events.iter().any(|event| event.ends_with(":publish")));
    }
}

#[tokio::test(start_paused = true)]
async fn ambiguous_exact_read_error_and_timeout_return_to_fixed_replay() {
    for mode in ["error", "timeout"] {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, winner) = scripted_takeover(Arc::clone(&events));
        io.replays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend([
                ScriptedOutcome::Ready(None),
                ScriptedOutcome::Ready(Some(winner)),
            ]);
        io.reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(if mode == "error" {
                ScriptedOutcome::Error("scripted exact read error")
            } else {
                ScriptedOutcome::Never
            });
        io.allow_publication();

        reconcile_pending_takeover(&io, pending, None)
            .await
            .expect("ambiguous exact read must return to fixed replay");
        let events = io.events();
        assert_eq!(events.iter().filter(|event| *event == "replay").count(), 2);
        assert!(
            events.iter().any(|event| event == "read"),
            "{mode}: {events:?}"
        );
        assert!(events.iter().any(|event| event.ends_with(":publish")));
    }
}

#[tokio::test]
async fn settlement_supervisor_cancellation_drops_one_owned_worker() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let io = Arc::new(ScriptedTakeoverIo::new(Arc::clone(&events)));
    let (pending, _) = scripted_takeover(Arc::clone(&events));
    let pause = ScriptedPause::new();
    io.replays
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(ScriptedOutcome::Paused(Arc::clone(&pause)));
    let task = tokio::spawn({
        let io = Arc::clone(&io);
        async move { reconcile_pending_takeover(io.as_ref(), pending, None).await }
    });
    pause.entered.notified().await;

    task.abort();
    assert!(task
        .await
        .expect_err("settlement supervisor must cancel")
        .is_cancelled());
    assert_eq!(
        io.events(),
        vec!["generation:10", "replay", "worker:worker-a:drop-teardown",],
        "the reconciliation future owns exactly one teardown capability while suspended"
    );
}

#[tokio::test]
async fn settlement_supervisor_panic_drops_one_owned_worker() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let io = Arc::new(ScriptedTakeoverIo::new(Arc::clone(&events)));
    let (pending, _) = scripted_takeover(Arc::clone(&events));
    io.replays
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(ScriptedOutcome::Panic("scripted replay panic"));
    let task = tokio::spawn({
        let io = Arc::clone(&io);
        async move { reconcile_pending_takeover(io.as_ref(), pending, None).await }
    });

    assert!(task
        .await
        .expect_err("settlement supervisor must propagate panic")
        .is_panic());
    assert_eq!(
        io.events(),
        vec!["generation:10", "replay", "worker:worker-a:drop-teardown",],
        "unwinding the supervisor cannot detach or duplicate worker ownership"
    );
}

#[tokio::test(start_paused = true)]
async fn creation_timeout_aborts_and_joins_child_before_worker_stop() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let pause = ScriptedPause::new();
    let handle: tokio::task::JoinHandle<Result<StartInfo, String>> = tokio::spawn({
        let events = Arc::clone(&events);
        let pause = Arc::clone(&pause);
        async move {
            let _drop = ScriptedChildDrop(events);
            pause.wait().await;
            std::future::pending().await
        }
    });
    pause.entered.notified().await;
    let worker = ProbeTakeoverWorker {
        id: "creation-worker",
        events: Arc::clone(&events),
        stopped: None,
        settled: false,
    };

    let result = TakeoverCreationOwner::new(handle, worker)
        .finish(tokio::time::Instant::now() + Duration::from_secs(1))
        .await;
    assert_eq!(
        result.err().as_deref(),
        Some("media-session takeover creation timed out")
    );
    assert_eq!(
        *events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![
            "creation-child:drop",
            "worker:creation-worker:stop:media-session takeover creation timed out",
        ],
        "worker teardown cannot begin until abort has joined the creation child"
    );
}

#[tokio::test]
async fn creation_owner_drop_aborts_and_joins_child_before_worker_stop() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let pause = ScriptedPause::new();
    let handle: tokio::task::JoinHandle<Result<StartInfo, String>> = tokio::spawn({
        let events = Arc::clone(&events);
        let pause = Arc::clone(&pause);
        async move {
            let _drop = ScriptedChildDrop(events);
            pause.wait().await;
            std::future::pending().await
        }
    });
    pause.entered.notified().await;
    let stopped = Arc::new(tokio::sync::Notify::new());
    let worker = ProbeTakeoverWorker {
        id: "creation-worker",
        events: Arc::clone(&events),
        stopped: Some(Arc::clone(&stopped)),
        settled: false,
    };

    drop(TakeoverCreationOwner::new(handle, worker));
    tokio::time::timeout(Duration::from_secs(1), stopped.notified())
        .await
        .expect("detached creation-owner settlement must finish");
    assert_eq!(
        *events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![
            "creation-child:drop",
            "worker:creation-worker:stop:media-session takeover creation owner dropped",
        ],
        "supervisor cancellation transfers child join and worker teardown to one detached owner"
    );
}

#[tokio::test]
async fn creation_panic_joins_child_before_worker_stop() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let handle: tokio::task::JoinHandle<Result<StartInfo, String>> = tokio::spawn({
        let events = Arc::clone(&events);
        async move {
            let _drop = ScriptedChildDrop(events);
            panic!("scripted creation panic");
        }
    });
    let worker = ProbeTakeoverWorker {
        id: "creation-worker",
        events: Arc::clone(&events),
        stopped: None,
        settled: false,
    };

    let error = match TakeoverCreationOwner::new(handle, worker)
        .finish(tokio::time::Instant::now() + Duration::from_secs(30))
        .await
    {
        Ok(_) => panic!("creation panic must fail settlement"),
        Err(error) => error,
    };
    assert!(error.starts_with("media-session takeover creation task failed:"));
    assert_eq!(
        *events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![
            "creation-child:drop",
            "worker:creation-worker:stop:media-session takeover creation task failed",
        ],
        "JoinError is observed only after child unwind, then the worker stops"
    );
}

#[tokio::test(start_paused = true)]
async fn cache_generation_rejection_cannot_unpublish_the_owned_worker() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let io = ScriptedTakeoverIo::new(Arc::clone(&events));
    let (pending, winner) = scripted_takeover(Arc::clone(&events));
    io.allow_publication();
    io.cache_results
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(false);

    reconcile_pending_takeover(&io, pending, Some(winner))
        .await
        .expect("cache generation rejection leaves Store ownership authoritative");
    let events = io.events();
    let publish = events
        .iter()
        .position(|event| event.ends_with(":publish"))
        .expect("worker publication");
    let rejected_cache = events
        .iter()
        .position(|event| event == "cache:9:false")
        .expect("scripted generation rejection");
    assert!(publish < rejected_cache, "{events:?}");
    assert!(!events.iter().any(|event| event.contains("drop-teardown")));
}

#[tokio::test(start_paused = true)]
async fn takeover_reconciliation_stops_exactly_once_on_definitive_loss() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let io = ScriptedTakeoverIo::new(Arc::clone(&events));
    let (pending, _) = scripted_takeover(Arc::clone(&events));
    let mut terminal = pending.original.clone();
    terminal.state = "ended".to_owned();
    io.replays
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(ScriptedOutcome::Ready(None));
    io.reads
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(ScriptedOutcome::Ready(Some(terminal)));

    reconcile_pending_takeover(&io, pending, None)
        .await
        .expect("definitive loss is a settled non-error verdict");
    assert_eq!(
        io.events(),
        vec![
            "generation:10",
            "replay",
            "generation:11",
            "read",
            "worker:worker-a:stop:media-session takeover lost",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn takeover_bootstrap_renewal_loss_never_publishes_the_durable_identity() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let io = ScriptedTakeoverIo::new(Arc::clone(&events));
    let (pending, winner) = scripted_takeover(Arc::clone(&events));
    io.pins
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(true);
    io.renewals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(ScriptedOutcome::Ready(false));

    reconcile_pending_takeover(&io, pending, Some(winner))
        .await
        .expect("a lost bootstrap renewal is a settled loss");
    let events = io.events();
    assert!(events.iter().any(|event| event == "pin"));
    assert!(events
        .iter()
        .any(|event| event == "renew:provisional-scripted"));
    assert!(events.iter().any(|event| {
        event == "worker:worker-a:stop:media-session takeover bootstrap renewal lost"
    }));
    assert!(
        !events.iter().any(|event| {
            event.starts_with("adopt:")
                || event.contains(":adopted:")
                || event == "seed"
                || event.ends_with(":publish")
                || event.starts_with("cache:")
        }),
        "the durable capability must remain unreachable unless its fresh lease commits: {events:?}"
    );
    assert!(!events.iter().any(|event| event.contains("drop-teardown")));
}

#[tokio::test(start_paused = true)]
async fn bootstrap_renewal_error_and_timeout_retain_without_adoption() {
    for mode in ["error", "timeout"] {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, winner) = scripted_takeover(Arc::clone(&events));
        io.pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(true);
        io.renewals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(if mode == "error" {
                ScriptedOutcome::Error("scripted bootstrap renewal error")
            } else {
                ScriptedOutcome::Never
            });

        assert!(
            reconcile_pending_takeover(&io, pending, Some(winner))
                .await
                .is_err(),
            "{mode} must remain an ambiguous failure"
        );
        let events = io.events();
        assert!(events
            .iter()
            .any(|event| event == "renew:provisional-scripted"));
        assert!(
            events.iter().any(|event| {
                event.starts_with(
                    "worker:worker-a:stop-retain:media-session takeover bootstrap renewal",
                )
            }),
            "{mode}: {events:?}"
        );
        assert!(
            !events.iter().any(|event| {
                event.starts_with("adopt:")
                    || event.contains(":adopted:")
                    || event == "seed"
                    || event.ends_with(":publish")
                    || event.starts_with("cache:")
            }),
            "{mode}: an ambiguous renewal cannot expose the durable identity: {events:?}"
        );
        assert!(!events.iter().any(|event| event.contains("drop-teardown")));
    }
}

#[tokio::test(start_paused = true)]
async fn takeover_publication_failures_stop_and_retain_the_known_winner() {
    for failure in ["runway", "pin", "adoption"] {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (mut pending, mut winner) = scripted_takeover(Arc::clone(&events));
        if failure == "runway" {
            let expires_at_ms = unix_ms().saturating_add(5_000);
            pending.claim.lease_expires_at_ms = expires_at_ms;
            pending.monotonic_expiry = tokio::time::Instant::now() + Duration::from_secs(5);
            winner.lease_expires_at_ms = expires_at_ms;
        } else {
            io.pins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(failure != "pin");
            if failure == "adoption" {
                io.renewals
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_back(ScriptedOutcome::Ready(true));
                io.adoptions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_back(false);
            }
        }
        let expected_expiry = pending.claim.lease_expires_at_ms;

        assert!(
            reconcile_pending_takeover(&io, pending, Some(winner))
                .await
                .is_err(),
            "{failure} failure must not publish"
        );
        let events = io.events();
        assert!(events.iter().any(|event| {
            event.starts_with("worker:worker-a:stop-retain:")
                && (failure == "adoption" || event.ends_with(&expected_expiry.to_string()))
        }), "{failure}: known-winner cleanup must retain settlement through its current exact lease: {events:?}");
        assert!(
            !events.iter().any(|event| event.ends_with(":publish")
                || event == "seed"
                || event.starts_with("cache:")),
            "{failure}: publication side effects are forbidden after failure: {events:?}"
        );
        if failure == "adoption" {
            assert!(events.iter().any(|event| event == "pin"));
            assert!(events
                .iter()
                .any(|event| event == "renew:provisional-scripted"));
            assert!(events.iter().any(|event| event == "adopt:adoption-a"));
            assert!(!events.iter().any(|event| event.contains(":adopted:")));
        }
        assert!(!events.iter().any(|event| event.contains("drop-teardown")));
    }
}

/// A settling takeover or replacement cannot report a frontier while its
/// process-local worker is between generations. Left in the renewal batch,
/// that absence reads as lost ownership; left out of `live`, stale
/// settlement deletes the durable route needed by the activation CAS.
#[test]
fn a_settlement_protected_route_is_live_but_not_renewable_or_stale() {
    let routes = vec![
        owned_lease("session-live"),
        owned_lease("session-settling"),
        owned_lease("session-gone"),
    ];
    let renewable = vec!["session-live".to_owned()];

    let settling = SessionSettlementGuard::begin("session-settling");
    let live = lease_tick_live(renewable.clone());
    assert!(live.contains("session-settling"), "{live:?}");
    assert!(live.contains("session-live"), "{live:?}");
    assert!(!live.contains("session-gone"), "{live:?}");
    let ids = lease_tick_active(&routes, &live, unix_ms())
        .iter()
        .map(|route| route.session_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec!["session-live"],
        "only a route with a reportable frontier may be renewed or fenced"
    );
    let mut stale_in_flight = HashSet::new();
    let mut backoff = HashMap::new();
    let stale = take_stale_settlement_candidates(
        &routes,
        &live,
        &mut stale_in_flight,
        &mut backoff,
        tokio::time::Instant::now(),
        unix_ms(),
    );
    assert_eq!(
        stale
            .iter()
            .map(|(route, _)| route.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["session-gone"],
        "the protected predecessor must retain the pointer needed by successor activation"
    );

    // A second attempt for the same route is ordinary; the loser's guard
    // must not un-protect the winner.
    let overlapping = SessionSettlementGuard::begin("session-settling");
    drop(overlapping);
    let live = lease_tick_live(renewable);
    assert!(
        live.contains("session-settling"),
        "an overlapping attempt's guard must not release the winner's protection"
    );
    assert!(
        lease_tick_active(&routes, &live, unix_ms())
            .iter()
            .all(|route| route.session_id != "session-settling"),
        "the settling route is still excluded while any holder remains"
    );

    // Adoption completes: the worker is now registered under the durable
    // id, so it is renewable on its own and the guard is released.
    drop(settling);
    let live = lease_tick_live(vec![
        "session-live".to_owned(),
        "session-settling".to_owned(),
    ]);
    let ids = lease_tick_active(&routes, &live, unix_ms())
        .iter()
        .map(|route| route.session_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec!["session-live", "session-settling"],
        "once the last guard drops the route renews normally"
    );

    let no_worker = lease_tick_live(Vec::new());
    let mut stale_in_flight = HashSet::new();
    let mut backoff = HashMap::new();
    let stale = take_stale_settlement_candidates(
        &[owned_lease("session-settling")],
        &no_worker,
        &mut stale_in_flight,
        &mut backoff,
        tokio::time::Instant::now(),
        unix_ms(),
    );
    assert_eq!(
        stale.len(),
        1,
        "after the transition resolves, an actually orphaned route settles normally"
    );
}

#[test]
fn a_seed_published_after_inventory_is_not_false_lease_loss() {
    let incarnation = "incarnation-fresh";
    let session = "session-fresh";
    let inventory = HashSet::new();
    let live = HashSet::from([session.to_owned()]);
    let fresh = HashSet::from([incarnation.to_owned()]);
    assert!(
        !known_generation_missing_from_inventory(incarnation, session, &inventory, &live, &fresh,),
        "a claim published after this tick's Store snapshot belongs to the next tick"
    );
    assert!(
        known_generation_missing_from_inventory(
            incarnation,
            session,
            &inventory,
            &live,
            &HashSet::new(),
        ),
        "the exemption is exact and lasts only while the seed is fresh"
    );
}

#[test]
fn service_vod_only_policy_survives_worker_and_durable_recipe_without_native_identity_change() {
    let native = valid_start_request();
    let native_value = serde_json::to_value(&native).expect("native envelope");
    assert!(native_value["request"].get("vod_only").is_none());
    assert!(native_value["request"].get("passive_vod").is_none());
    let legacy: RemoteStartRequest = serde_json::from_value(native_value).expect("legacy recipe");
    assert!(!legacy.request.vod_only);

    assert_eq!(
        native.request.durable_intent_fingerprint(&native.principal),
        legacy.request.durable_intent_fingerprint(&legacy.principal)
    );
    let mut service = native.clone();
    service.request.vod_only = true;
    service.request.passive_vod = true;
    assert_ne!(
        native.request.durable_intent_fingerprint(&native.principal),
        service
            .request
            .durable_intent_fingerprint(&service.principal)
    );
    let encoded = serde_json::to_vec(&service).expect("worker and durable envelope");
    let mut recovered: RemoteStartRequest =
        serde_json::from_slice(&encoded).expect("policy round trip");
    assert!(recovered.request.vod_only);
    assert!(recovered.request.passive_vod);
    let mut invalid = recovered.clone();
    invalid.request.vod_only = false;
    assert!(!invalid.is_valid());
    assert!(recovered.is_valid());
    recovered.request.presentation = crate::transcode::Presentation::Live;
    assert!(!recovered.is_valid());
    assert!(
        !takeover_recipe_is_valid(&recovered),
        "takeover cannot turn VOD-only into rolling"
    );
}

#[test]
fn remote_start_contract_rejects_unfenced_or_noncanonical_inputs() {
    let request = valid_start_request();
    assert!(request.is_valid());
    assert!(worker_session_request_is_valid(&request.request));

    let mut incompatible = request.clone();
    incompatible.protocol_version = crate::media_pool::PROTOCOL_VERSION.saturating_sub(1);
    assert!(!incompatible.is_valid());

    let mut removed_presentation = request.clone();
    removed_presentation.request.presentation = crate::transcode::Presentation::Live;
    assert!(!removed_presentation.is_valid());
    assert!(takeover_recipe_is_valid(&removed_presentation));
    assert!(!takeover_recipe_is_valid(&request));

    let mut mismatched = request.clone();
    mismatched.request.request_id = Some("00000000-0000-4000-8000-0000000000ff".to_owned());
    assert!(!mismatched.is_valid());

    let mut unbound_reopen = request.clone();
    unbound_reopen.request.reopen_reason = Some(ReopenReason::Stall);
    assert!(!unbound_reopen.is_valid());

    let mut traversal = request.clone();
    traversal.request.playback_id = "player\r\nforged".to_owned();
    assert!(!traversal.is_valid());

    let mut blank_playback = request.clone();
    blank_playback.request.playback_id = "   ".to_owned();
    assert!(!worker_session_request_is_valid(&blank_playback.request));
    assert!(!blank_playback.is_valid());

    let mut too_late = request.clone();
    too_late.request.start_seconds = MAX_MEDIA_MILLIS as f64 / 1_000.0 + 0.001;
    assert!(!worker_session_request_is_valid(&too_late.request));
    assert!(!too_late.is_valid());

    let mut audio_out_of_range = request.clone();
    audio_out_of_range.request.audio_index = Some(1_025);
    assert!(!worker_session_request_is_valid(
        &audio_out_of_range.request
    ));
    assert!(!audio_out_of_range.is_valid());

    let mut subtitle_out_of_range = request.clone();
    subtitle_out_of_range.request.subtitle_burn = Some(-1);
    assert!(!worker_session_request_is_valid(
        &subtitle_out_of_range.request
    ));
    assert!(!subtitle_out_of_range.is_valid());

    let mut json = serde_json::to_value(request).expect("serialize start request");
    json.get_mut("request")
        .and_then(serde_json::Value::as_object_mut)
        .expect("request object")
        .insert("future_unfenced_field".to_owned(), serde_json::json!(true));
    assert!(serde_json::from_value::<RemoteStartRequest>(json).is_err());
}

#[test]
fn processed_p81_owner_marker_is_optional_and_bound_to_pq_transcode() {
    let ordinary = valid_start_response();
    let json = serde_json::to_value(&ordinary).expect("ordinary response");
    assert!(json.get("processed_dv_profile").is_none());
    let decoded: RemoteStartResponse = serde_json::from_value(json).expect("old owner response");
    assert!(decoded.processed_dv_profile.is_none());
    let mut processed = ordinary;
    processed.grade = OutputGrade::Hdr10;
    processed.processed_dv_profile = Some(8);
    assert!(processed.is_valid());
    let header = processed
        .processed_dv_header()
        .expect("processed owner sidecar");
    assert_eq!(processed.processed_dv_profile_from_header(&header), Some(8));
    let body = serde_json::to_value(&processed).expect("processed body");
    assert!(
        body.get("processed_dv_profile").is_none(),
        "legacy strict body remains unchanged"
    );
    let wrong_session = header.replace(
        &processed.session_id,
        "00000000-0000-4000-8000-0000000000b2",
    );
    assert!(processed
        .processed_dv_profile_from_header(&wrong_session)
        .is_none());
    processed.processed_dv_profile = Some(7);
    assert!(!processed.is_valid());
    processed.processed_dv_profile = Some(8);
    processed.grade = OutputGrade::Sdr;
    assert!(!processed.is_valid());
}

#[test]
fn remote_start_response_is_bound_to_the_session_capability_and_reports_presentation() {
    let response = valid_start_response();
    assert!(response.is_valid());

    let mut recovery_presentation = response.clone();
    recovery_presentation.vod = false;
    assert!(recovery_presentation.is_valid());

    let mut wrong_path = response.clone();
    wrong_path.playlist_url = "/api/v1/hls/somebody-else/index.m3u8".to_owned();
    assert!(!wrong_path.is_valid());

    let mut mismatched_height = response.clone();
    mismatched_height.kind = SessionKind::Transcode { height: 1080 };
    assert!(!mismatched_height.is_valid());

    let mut nonfinite = response;
    nonfinite.start_seconds = f64::NAN;
    assert!(!nonfinite.is_valid());

    let mut too_late = valid_start_response();
    too_late.media_origin_seconds = MAX_MEDIA_MILLIS as f64 / 1_000.0 + 0.001;
    assert!(!too_late.is_valid());

    let mut too_long = valid_start_response();
    too_long.duration_ms = Some(MAX_MEDIA_MILLIS + 1);
    assert!(!too_long.is_valid());

    let mut unknown_copy = valid_start_response();
    unknown_copy.kind = SessionKind::Copy {
        convert_dolby_vision: false,
        dual_layer_as_copied: false,
        aac: false,
        preserve_dolby_vision: false,
    };
    unknown_copy.target_height = 0;
    assert!(unknown_copy.is_valid());
    unknown_copy.target_height = crate::transcode::MAX_HEIGHT + 1;
    assert!(unknown_copy.is_valid());
    unknown_copy.target_height = -1;
    assert!(!unknown_copy.is_valid());

    let mut rolling = valid_start_response();
    rolling.control_lease_timeout_ms = crate::playback_control::ROLLING_LEASE_TIMEOUT_MS;
    assert!(
        rolling.is_valid(),
        "the rolling registry's real TTL is valid"
    );
    rolling.control_lease_timeout_ms = 42_000;
    assert!(
        !rolling.is_valid(),
        "a worker cannot invent a lease lifetime the owning registry does not enforce"
    );
}

#[test]
fn control_admission_bounds_retries_and_uuid_spray_before_store_work() {
    let started = Instant::now();
    let mut admission = ControlAdmission {
        window_started: started,
        admitted: 0,
        sessions: HashMap::new(),
    };
    for _ in 0..CONTROL_RATE_PER_SESSION {
        assert_eq!(admission.admit(started, "one-session"), Ok(()));
    }
    assert!(admission.admit(started, "one-session").is_err());
    assert_eq!(
        admission.admit(started + CONTROL_RATE_WINDOW, "one-session"),
        Ok(()),
        "the bounded retry budget reopens after its fixed window"
    );

    let next_window = started + CONTROL_RATE_WINDOW * 2;
    let mut spray = ControlAdmission {
        window_started: next_window,
        admitted: 0,
        sessions: HashMap::new(),
    };
    for index in 0..CONTROL_RATE_GLOBAL {
        assert_eq!(
            spray.admit(next_window, &format!("random-capability-{index}")),
            Ok(())
        );
    }
    assert!(spray.admit(next_window, "one-too-many").is_err());
    assert!(
        spray.sessions.len() <= MAX_CONTROL_RATE_ENTRIES,
        "the admission map itself must stay bounded"
    );
}

#[test]
fn quality_reservation_burst_keeps_manual_control_budget_independent() {
    let started = Instant::now();
    let mut quality = ControlAdmission {
        window_started: started,
        ..Default::default()
    };
    let mut manual = ControlAdmission {
        window_started: started,
        ..Default::default()
    };
    for _ in 0..QUALITY_RATE_PER_SESSION {
        assert_eq!(
            quality.admit_with_limit(started, "film", QUALITY_RATE_PER_SESSION),
            Ok(())
        );
    }
    assert!(quality
        .admit_with_limit(started, "film", QUALITY_RATE_PER_SESSION)
        .is_err());
    for _ in 0..CONTROL_RATE_PER_SESSION {
        assert_eq!(manual.admit(started, "film"), Ok(()));
    }
    assert!(manual.admit(started, "film").is_err());
    assert_eq!(
        quality.admit_with_limit(
            started + CONTROL_RATE_WINDOW,
            "film",
            QUALITY_RATE_PER_SESSION
        ),
        Ok(())
    );
    let mut spray = ControlAdmission {
        window_started: started,
        ..Default::default()
    };
    for index in 0..CONTROL_RATE_GLOBAL {
        assert_eq!(
            spray.admit_with_limit(started, &index.to_string(), QUALITY_RATE_PER_SESSION),
            Ok(())
        );
    }
    assert!(spray
        .admit_with_limit(started, "extra", QUALITY_RATE_PER_SESSION)
        .is_err());
    assert!(spray.sessions.len() <= MAX_CONTROL_RATE_ENTRIES);
}

fn owner_answer(status: u16, body: &[u8]) -> PeerResponse {
    PeerResponse {
        dv_processing_header: None,
        clock_timing: None,
        status: reqwest::StatusCode::from_u16(status).expect("status"),
        body: body.to_vec(),
    }
}

fn quality_discovery() -> crate::http::hls::QualityControlRequest {
    crate::http::hls::QualityControlRequest {
        version: 1,
        generation: uuid::Uuid::new_v4().to_string(),
        control_epoch: 1,
        operation: crate::http::hls::QualityControlOperation::Discover,
        identity: None,
    }
}

/// An owner's refusal or deferral must keep its meaning across the relay:
/// collapsing 409/410/429 into 503 turns "stop" into "retry", and calling
/// an owner's route miss "unsupported" disables cancellation for a
/// session that merely raced its own end.
#[test]
fn quality_control_relay_keeps_owner_refusals_and_route_misses_distinct() {
    let request = quality_discovery();
    let gone = serde_json::to_vec(&crate::playback_control::ControlErrorBody {
        terminal_reason: None,
        code: "session_gone".into(),
        message: "no media session holds this capability".into(),
        generation: None,
        control_epoch: None,
        retry_after_ms: None,
        invalid_field: None,
    })
    .expect("error body");
    let Ok(QualityControlRelayOutcome::Refused(miss)) =
        classify_quality_control_relay(owner_answer(404, &gone), None, &request)
    else {
        panic!("an owner's own route miss is not 'unsupported'");
    };
    assert_eq!(miss.status(), StatusCode::NOT_FOUND);
    for status in [404, 405] {
        assert!(
            matches!(
                classify_quality_control_relay(owner_answer(status, b""), None, &request),
                Ok(QualityControlRelayOutcome::Unsupported)
            ),
            "an owner without the endpoint ({status}) is unsupported"
        );
    }
    for status in [409, 410] {
        let Ok(QualityControlRelayOutcome::Refused(refusal)) =
            classify_quality_control_relay(owner_answer(status, b""), None, &request)
        else {
            panic!("owner {status} must pass through");
        };
        assert_eq!(refusal.status().as_u16(), status);
        assert!(refusal.headers().get(header::RETRY_AFTER).is_none());
    }
    let limited = serde_json::to_vec(&crate::playback_control::ControlErrorBody {
        terminal_reason: None,
        code: "control_rate_limited".into(),
        message: "the quality control budget is exhausted".into(),
        generation: None,
        control_epoch: None,
        retry_after_ms: Some(2_500),
        invalid_field: None,
    })
    .expect("error body");
    let Ok(QualityControlRelayOutcome::Refused(deferred)) =
        classify_quality_control_relay(owner_answer(429, &limited), None, &request)
    else {
        panic!("owner 429 must pass through");
    };
    assert_eq!(deferred.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        deferred
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("3"),
        "the owner's own delay reaches the client"
    );
    let Ok(QualityControlRelayOutcome::Refused(unavailable)) =
        classify_quality_control_relay(owner_answer(503, b""), Some(7), &request)
    else {
        panic!("owner 503 must pass through");
    };
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        unavailable
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("7")
    );
    assert!(matches!(
        classify_quality_control_relay(owner_answer(500, b""), None, &request),
        Err(PeerTransportError::InvalidResponse)
    ));
}

/// The schedule relay keeps the owner's status and its retry hint, and a
/// deferral without one still tells the client when to come back.
#[test]
fn quality_schedule_relay_passes_owner_status_and_retry_after() {
    let deferred = relayed_owner_response(429, Some(4), Vec::new()).expect("relay");
    assert_eq!(deferred.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        deferred
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("4")
    );
    let unavailable = relayed_owner_response(503, None, Vec::new()).expect("relay");
    assert_eq!(
        unavailable
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
    let refused = relayed_owner_response(409, None, Vec::new()).expect("relay");
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert!(refused.headers().get(header::RETRY_AFTER).is_none());
}

#[test]
fn relay_resource_names_are_single_safe_path_components() {
    for value in ["seg00001.ts", "init.mp4", "subtitle-2.vtt"] {
        assert!(valid_resource_name(value), "{value}");
    }
    for value in ["", ".", "..", "../secret", "nested/segment.ts", "x\r\ny"] {
        assert!(!valid_resource_name(value), "{value}");
    }
}

#[test]
fn relay_headers_and_renewal_batches_stay_bounded() {
    let legacy_abort: RemoteAbortRequest = serde_json::from_str(&format!(
        r#"{{"incarnation_id":"{}","session_id":"{}"}}"#,
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    ))
    .expect("new owner accepts legacy start-abort envelope");
    assert!(legacy_abort.reason.is_none());
    assert!(legacy_abort.is_valid());
    for terminal in [
        crate::vodserve::Terminal::Deleted,
        crate::vodserve::Terminal::Superseded,
        crate::vodserve::Terminal::AdminStop,
        crate::vodserve::Terminal::Revoked,
        crate::vodserve::Terminal::Replaced,
    ] {
        let terminal_abort = RemoteAbortRequest {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            expected_owner_epoch: 1,
            reason: Some(terminal.control_reason().to_owned()),
        };
        assert!(terminal_abort.is_valid(), "{terminal:?}");
        assert!(serde_json::to_string(&terminal_abort)
            .expect("terminal-abort JSON")
            .contains(terminal.control_reason()));
    }
    let release_abort = RemoteAbortRequest {
        incarnation_id: uuid::Uuid::new_v4().to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        expected_owner_epoch: 1,
        reason: Some(
            crate::vodserve::Terminal::Deleted
                .control_reason()
                .to_owned(),
        ),
    };
    assert!(release_abort.is_valid());

    let headers = RelayHeaders {
        range: Some("bytes=0-99".to_owned()),
        if_range: Some("\"generation\"".to_owned()),
        if_none_match: Some("\"generation\"".to_owned()),
        if_modified_since: Some("Sun, 23 Aug 2026 08:00:00 GMT".to_owned()),
    };
    assert!(headers.is_valid());
    assert!(!RelayHeaders {
        range: Some("bytes=0-1\r\nx-forged: true".to_owned()),
        ..RelayHeaders::default()
    }
    .is_valid());
    assert!(!RelayHeaders {
        if_none_match: Some("x".repeat(1_025)),
        ..RelayHeaders::default()
    }
    .is_valid());
    assert!(!RelayHeaders {
        if_range: Some("x".repeat(1_025)),
        ..RelayHeaders::default()
    }
    .is_valid());

    let mut public = axum::http::HeaderMap::new();
    public.insert(
        axum::http::header::RANGE,
        "bytes=10-19".parse().expect("Range"),
    );
    public.insert(
        axum::http::header::IF_RANGE,
        "\"generation\"".parse().expect("If-Range"),
    );
    let forwarded = RelayHeaders::from_http(&public);
    assert_eq!(forwarded.range.as_deref(), Some("bytes=10-19"));
    assert_eq!(forwarded.if_range.as_deref(), Some("\"generation\""));
    let downgraded = forwarded.for_unversioned_peer();
    assert!(downgraded.range.is_none());
    assert!(downgraded.if_range.is_none());
    let encoded = serde_json::to_value(&downgraded).expect("encode old-peer headers");
    let object = encoded.as_object().expect("relay header object");
    assert!(
        !object.contains_key("if_range"),
        "a new node must not send the unknown field to an old strict peer"
    );
    assert!(
        !object.contains_key("range"),
        "the old peer must fall back to a complete representation"
    );
    let decoded: RelayHeaders = serde_json::from_str(
        r#"{"range":"bytes=0-9","if_none_match":null,"if_modified_since":null}"#,
    )
    .expect("new owner accepts the old relay JSON shape");
    assert_eq!(decoded.range.as_deref(), Some("bytes=0-9"));
    assert!(decoded.if_range.is_none());
    let legacy_range_only = decoded.for_unversioned_peer();
    assert!(
        legacy_range_only.range.is_none(),
        "an unversioned Range-only envelope has no validator proof and must become full 200"
    );

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyAbortRequest {
        incarnation_id: String,
        session_id: String,
    }
    let reason_body = serde_json::to_vec(&release_abort).expect("new abort body");
    assert!(
        serde_json::from_slice::<LegacyAbortRequest>(&reason_body).is_err(),
        "the old strict endpoint rejects the reason-bearing envelope"
    );
    let fallback = remote_abort_legacy_retry_body(&release_abort, reqwest::StatusCode::BAD_REQUEST)
        .expect("legacy abort fallback")
        .expect("a rejected reason request retries once");
    let decoded = serde_json::from_slice::<LegacyAbortRequest>(&fallback)
        .expect("old endpoint accepts fallback body");
    assert_eq!(decoded.incarnation_id, release_abort.incarnation_id);
    assert_eq!(decoded.session_id, release_abort.session_id);
    assert!(
        remote_abort_legacy_retry_body(&release_abort, reqwest::StatusCode::NO_CONTENT)
            .expect("new-peer response")
            .is_none()
    );

    let routes = vec![(); LEASE_RENEWAL_BATCH * 2 + 1];
    assert_eq!(
        renewal_chunks(&routes)
            .map(|chunk| chunk.len())
            .collect::<Vec<_>>(),
        vec![LEASE_RENEWAL_BATCH, LEASE_RENEWAL_BATCH, 1]
    );
    let admitted_owner = vec![(); LEASE_RENEWAL_BATCH * LEASE_RENEWAL_FANOUT];
    assert_eq!(
        renewal_chunks(&admitted_owner).count(),
        LEASE_RENEWAL_FANOUT
    );
    assert!(
        REMOTE_ACTIVATION_CONFIRMATION_WINDOW
            > START_DEADLINE
                + OWNER_ASSIGNMENT_DEADLINE
                + Duration::from_millis(LEASE_TTL_MS as u64),
        "the worker fallback must outlive ingress plus the fixed activation lease"
    );
    assert!(
        Duration::from_millis(TAKEOVER_CLAIM_LEASE_TTL_MS as u64)
            > TAKEOVER_PUBLICATION_MIN_RUNWAY + TAKEOVER_RECONCILIATION_STORE_DEADLINE,
        "the one-shot claim must outlive publication with a full first-renewal runway"
    );
}

#[test]
fn stale_settlement_retries_retain_a_bounded_backoff_slot() {
    assert_eq!(
        (1..=6)
            .map(stale_settlement_retry_delay)
            .collect::<Vec<_>>(),
        vec![
            Duration::from_secs(30),
            Duration::from_secs(60),
            Duration::from_secs(120),
            Duration::from_secs(240),
            Duration::from_secs(300),
            Duration::from_secs(300),
        ]
    );

    let now = tokio::time::Instant::now();
    let mut backoff = (0..MAX_STALE_SETTLEMENTS_PER_TICK - 1)
        .map(|index| {
            (
                format!("deferred-{index}"),
                StaleSettlementBackoff {
                    failures: 1,
                    next_attempt: now + Duration::from_secs(30),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(stale_settlement_capacity(1, &backoff), 0);

    backoff.get_mut("deferred-0").expect("fixture").next_attempt = now;
    assert_eq!(
        stale_settlement_capacity(1, &backoff),
        0,
        "a due retry retains its slot until that exact retry is selected"
    );

    let mut settling = HashSet::from(["in-flight".to_owned()]);
    let candidates = take_stale_settlement_candidates(
        &[owned_lease("fresh-first"), owned_lease("deferred-0")],
        &HashSet::new(),
        &mut settling,
        &mut backoff,
        now,
        unix_ms(),
    );
    assert_eq!(
        candidates
            .iter()
            .map(|(route, failures)| (route.session_id.as_str(), *failures))
            .collect::<Vec<_>>(),
        vec![("deferred-0", 1)],
        "a due retry reclaims its own slot before a fresh earlier route"
    );
    assert!(!settling.contains("fresh-first"));
    assert_eq!(
        settling.len() + backoff.len(),
        MAX_STALE_SETTLEMENTS_PER_TICK
    );
}

#[test]
fn stale_inventory_route_is_not_settled_after_its_lease_runway_is_spent() {
    let inventory_now_ms = unix_ms();
    let mut route = owned_lease("expired-after-inventory");
    route.lease_expires_at_ms = inventory_now_ms.saturating_add(1_000);
    let candidate_now_ms = inventory_now_ms.saturating_add(1_001);
    let mut settling = HashSet::new();
    let mut backoff = HashMap::new();
    let candidates = take_stale_settlement_candidates(
        &[route],
        &HashSet::new(),
        &mut settling,
        &mut backoff,
        tokio::time::Instant::now(),
        candidate_now_ms,
    );
    assert!(
        candidates.is_empty(),
        "a pre-expiry inventory row must remain takeover-eligible once candidate evaluation crosses expiry"
    );
    assert!(settling.is_empty());
}

/// C-07 M5 (PLEX-FACADE-PAGING §5.4). The three ways a cached route
/// lookup is answered are three separate series, and every map-lock and
/// sweep site the lookups pass through is recorded exactly once per pass.
#[tokio::test]
async fn a_cache_hit_and_a_store_read_are_counted_separately() {
    use plurx_core::cluster::membership::MembershipManager;
    use plurx_core::store::SqliteStore;

    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let coordinator = MediaSessionCoordinator::new(MembershipManager::unavailable(), store);
    let metrics = coordinator.route_cache_metrics();
    let session_id = "00000000-0000-4000-8000-0000000000d1";
    let other_id = "00000000-0000-4000-8000-0000000000d2";

    assert!(coordinator.route(session_id).await.expect("miss").is_none());
    assert_eq!(
        metrics.lookups(RouteLookup::Store),
        1,
        "a cold lookup reads the Store"
    );
    assert_eq!(
        metrics.lookups(RouteLookup::CacheHit),
        0,
        "a cold lookup is not a hit"
    );
    for _ in 0..3 {
        assert!(coordinator.route(session_id).await.expect("hit").is_none());
    }
    assert_eq!(
        metrics.lookups(RouteLookup::CacheHit),
        3,
        "each cached answer is one hit"
    );
    assert_eq!(
        metrics.lookups(RouteLookup::Store),
        1,
        "a cached answer must not also count as a Store read"
    );
    assert!(coordinator.route(other_id).await.expect("miss").is_none());
    assert_eq!(metrics.lookups(RouteLookup::Store), 2);
    assert_eq!(metrics.lookups(RouteLookup::SingleFlightHit), 0);

    let exposition = metrics.prometheus();
    for line in [
        "plurx_media_session_route_lookups_total{result=\"cache_hit\"} 3\n",
        "plurx_media_session_route_lookups_total{result=\"single_flight_hit\"} 0\n",
        "plurx_media_session_route_lookups_total{result=\"store\"} 2\n",
        // Two misses take the lookup lock twice each (before and after
        // the single-flight shard), three hits once each.
        "plurx_media_session_route_lock_seconds_count{site=\"lookup\"} 7\n",
        // Each Store answer is published once.
        "plurx_media_session_route_lock_seconds_count{site=\"queried_insert\"} 2\n",
        "plurx_media_session_route_lock_seconds_count{site=\"insert\"} 0\n",
        "plurx_media_session_route_lock_seconds_count{site=\"generation_insert\"} 0\n",
        // Two sweeps, over an empty map and then over the first answer.
        "plurx_media_session_route_prune_entries_count 2\n",
        "plurx_media_session_route_prune_entries_sum 1\n",
        "plurx_media_session_route_cache_entries 2\n",
    ] {
        assert!(
            exposition.contains(line),
            "missing {line:?} in\n{exposition}"
        );
    }

    let route = media_route("00000000-0000-4000-8000-0000000000d3");
    coordinator.cache_route(route).await;
    let exposition = metrics.prometheus();
    for line in [
        "plurx_media_session_route_lock_seconds_count{site=\"insert\"} 1\n",
        "plurx_media_session_route_prune_entries_count 3\n",
        "plurx_media_session_route_prune_entries_sum 3\n",
        "plurx_media_session_route_cache_entries 3\n",
    ] {
        assert!(
            exposition.contains(line),
            "missing {line:?} in\n{exposition}"
        );
    }
}

/// C-07 M5 (PLEX-FACADE-PAGING §3.5 item 2). The prune histogram counts
/// the entries each sweep WALKS, which is the O(n) cost under the map
/// lock, not the entries it leaves (the gauge already says that). Time is
/// paused so entries expire on demand, and each of the three sweep sites
/// runs over expired entries: counting after `retain` would record 0
/// there. It also covers the gauge update when a lookup removes an
/// expired entry. No Store call is made, so paused time cannot fire a
/// lookup deadline.
#[tokio::test(start_paused = true)]
async fn each_route_sweep_counts_the_entries_it_walked_and_an_expired_lookup_lowers_the_gauge() {
    use plurx_core::cluster::membership::MembershipManager;
    use plurx_core::store::SqliteStore;

    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let coordinator = MediaSessionCoordinator::new(MembershipManager::unavailable(), store);
    let metrics = coordinator.route_cache_metrics();
    let value = |name: &str| -> u64 {
        let exposition = metrics.prometheus();
        let prefix = format!("{name} ");
        exposition
            .lines()
            .find_map(|line| line.strip_prefix(prefix.as_str()))
            .unwrap_or_else(|| panic!("{name} missing in\n{exposition}"))
            .parse()
            .expect("integer sample")
    };
    let sum = "plurx_media_session_route_prune_entries_sum";
    let entries = "plurx_media_session_route_cache_entries";
    let generation = |session_id: &str| {
        coordinator.route_generations[route_hash(session_id) % coordinator.route_generations.len()]
            .load(std::sync::atomic::Ordering::Acquire)
    };
    let id = |n: u8| format!("00000000-0000-4000-8000-0000000000f{n}");

    coordinator.cache_route(media_route(&id(1))).await;
    coordinator.cache_route(media_route(&id(2))).await;
    assert_eq!(value(entries), 2);
    tokio::time::advance(ROUTE_CACHE_TTL).await;

    // A lookup that finds its entry expired removes it and lowers the gauge.
    assert!(coordinator.cached_route(&id(1)).await.is_none());
    assert_eq!(value(entries), 1, "an expired lookup must lower the gauge");

    // `cache_route_result` sweeps one expired entry and keeps none of it.
    let before = value(sum);
    coordinator.cache_route(media_route(&id(3))).await;
    assert_eq!(
        value(sum) - before,
        1,
        "insert sweep walked one expired entry"
    );
    assert_eq!(value(entries), 1);

    // `cache_queried_route_result` sweeps two expired entries.
    coordinator.cache_route(media_route(&id(4))).await;
    assert_eq!(value(entries), 2);
    tokio::time::advance(ROUTE_CACHE_TTL).await;
    let before = value(sum);
    assert!(coordinator
        .cache_queried_route_result(&id(5), None, generation(&id(5)))
        .await
        .is_none());
    assert_eq!(
        value(sum) - before,
        2,
        "queried-insert sweep walked two expired entries"
    );
    assert_eq!(value(entries), 1);

    // `cache_route_if_generation` sweeps two expired entries.
    coordinator.cache_route(media_route(&id(6))).await;
    assert_eq!(value(entries), 2);
    tokio::time::advance(ROUTE_CACHE_TTL).await;
    let before = value(sum);
    assert!(
        coordinator
            .cache_route_if_generation(media_route(&id(7)), generation(&id(7)))
            .await
    );
    assert_eq!(
        value(sum) - before,
        2,
        "generation-insert sweep walked two expired entries"
    );
    assert_eq!(value(entries), 1);
}

/// C-07 M5 (PLEX-FACADE-PAGING §2.4, §5.4). The measurement's reading of
/// "store reads per session" depends on this: while one Store read for a
/// capability is outstanding, every other lookup of that capability
/// waits on its `route_queries` shard and is then answered by the cache
/// the first read filled. The gate holds the first read open until every
/// lookup has run as far as it can, so this does not pass by timing.
#[tokio::test]
async fn concurrent_lookups_of_one_session_produce_one_store_read() {
    use plurx_core::cluster::membership::MembershipManager;
    use plurx_core::store::SqliteStore;

    const LOOKUPS: u64 = 8;
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let coordinator = MediaSessionCoordinator::new(MembershipManager::unavailable(), store);
    let metrics = coordinator.route_cache_metrics();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *coordinator
        .route_store_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
    let session_id = "00000000-0000-4000-8000-0000000000d4";
    let lookups = (0..LOOKUPS)
        .map(|_| {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move { coordinator.route(session_id).await })
        })
        .collect::<Vec<_>>();
    // A current-thread runtime: yielding lets every spawned lookup run
    // until it parks, either at the gate or behind its query shard.
    for _ in 0..256 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        metrics.lookups(RouteLookup::Store),
        1,
        "while one Store read is outstanding, the other lookups of the same capability must wait behind its single-flight shard instead of reading the Store"
    );
    gate.close();
    for lookup in lookups {
        assert!(lookup
            .await
            .expect("lookup task")
            .expect("lookup")
            .is_none());
    }
    assert_eq!(metrics.lookups(RouteLookup::Store), 1);
    assert_eq!(
        metrics.lookups(RouteLookup::SingleFlightHit),
        LOOKUPS - 1,
        "every waiter is answered by the cache the one read filled"
    );
    assert_eq!(metrics.lookups(RouteLookup::CacheHit), 0);
}

/// The route-cache exposition has a fixed series count whatever traffic
/// it has seen (no id, no path, no node in a label), and each lock
/// bucket's `le` label is the bound it is counted against.
#[test]
fn route_cache_exposition_is_bounded_and_its_labels_match_their_bounds() {
    for (bound_nanos, label) in ROUTE_LOCK_BUCKETS {
        let seconds = label.parse::<f64>().expect("numeric le label");
        assert_eq!((seconds * 1e9).round() as u64, bound_nanos, "{label}");
    }
    assert_eq!(
        ROUTE_PRUNE_BUCKETS.last().copied(),
        Some(MAX_ROUTE_CACHE_ENTRIES as u64),
        "the top prune bucket is the cache's ceiling"
    );
    let metrics = RouteCacheMetrics::default();
    let empty = metrics.prometheus();
    metrics.lock_waited(RouteLockSite::Insert, Duration::from_micros(50));
    metrics.lock_waited(RouteLockSite::Insert, Duration::from_secs(1));
    metrics.pruned(MAX_ROUTE_CACHE_ENTRIES);
    metrics.lookup(RouteLookup::SingleFlightHit);
    metrics.cache_entries(17);
    let exposition = metrics.prometheus();
    let series = |text: &str| text.lines().filter(|line| !line.starts_with('#')).count();
    // 3 lookup results; 4 sites x (5 bounds + Inf + sum + count); 7 prune
    // bounds + Inf + sum + count; 1 gauge.
    assert_eq!(series(&empty), 3 + 4 * 8 + 10 + 1);
    assert_eq!(series(&exposition), series(&empty));
    for line in [
        "plurx_media_session_route_lock_seconds_bucket{site=\"insert\",le=\"0.00001\"} 0\n",
        "plurx_media_session_route_lock_seconds_bucket{site=\"insert\",le=\"0.0001\"} 1\n",
        "plurx_media_session_route_lock_seconds_bucket{site=\"insert\",le=\"0.1\"} 1\n",
        "plurx_media_session_route_lock_seconds_bucket{site=\"insert\",le=\"+Inf\"} 2\n",
        "plurx_media_session_route_lock_seconds_sum{site=\"insert\"} 1.000050000\n",
        "plurx_media_session_route_lock_seconds_count{site=\"lookup\"} 0\n",
        "plurx_media_session_route_prune_entries_bucket{le=\"2048\"} 0\n",
        "plurx_media_session_route_prune_entries_bucket{le=\"4096\"} 1\n",
        "plurx_media_session_route_prune_entries_sum 4096\n",
        "plurx_media_session_route_lookups_total{result=\"single_flight_hit\"} 1\n",
        "plurx_media_session_route_cache_entries 17\n",
    ] {
        assert!(
            exposition.contains(line),
            "missing {line:?} in\n{exposition}"
        );
    }
}

#[tokio::test]
async fn route_misses_are_single_flight_and_activation_supersedes_them() {
    use plurx_core::cluster::membership::MembershipManager;
    use plurx_core::store::SqliteStore;

    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let coordinator = MediaSessionCoordinator::new(MembershipManager::unavailable(), store);
    let session_id = "00000000-0000-4000-8000-0000000000c1";
    assert!(coordinator
        .route(session_id)
        .await
        .expect("first miss")
        .is_none());
    assert!(coordinator
        .route(session_id)
        .await
        .expect("negative-cache hit")
        .is_none());
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::Store),
        1,
        "a repeated random capability must cause only one Store read"
    );

    let mut route = media_route(session_id);
    route.incarnation_id = "00000000-0000-4000-8000-0000000000c2".to_owned();
    let generation_shard = route_hash(session_id) % coordinator.route_generations.len();
    let stale_miss_generation =
        coordinator.route_generations[generation_shard].load(std::sync::atomic::Ordering::Acquire);
    coordinator.cache_route(route.clone()).await;
    coordinator.routes.lock().await.remove(session_id);
    assert!(
        coordinator
            .cache_queried_route_result(session_id, None, stale_miss_generation)
            .await
            .is_none(),
        "a stale in-flight miss must remain suppressed after cache eviction"
    );
    assert!(
        !coordinator.routes.lock().await.contains_key(session_id),
        "the stale miss must not be published after activation"
    );
    coordinator.cache_route(route.clone()).await;
    assert_eq!(
        coordinator
            .route(session_id)
            .await
            .expect("activated route")
            .map(|route| route.incarnation_id),
        Some("00000000-0000-4000-8000-0000000000c2".to_owned()),
        "activation must replace an earlier cached miss immediately"
    );

    let stale_active = route.clone();
    let stale_active_generation =
        coordinator.route_generations[generation_shard].load(std::sync::atomic::Ordering::Acquire);
    route.state = "ended".to_owned();
    route.lease_expires_at_ms = unix_ms();
    coordinator.cache_route(route.clone()).await;
    coordinator.routes.lock().await.remove(session_id);
    assert!(
        coordinator
            .cache_queried_route_result(session_id, Some(stale_active), stale_active_generation,)
            .await
            .is_none(),
        "an active read begun before fencing must stay suppressed after eviction"
    );
    assert!(
        !coordinator.routes.lock().await.contains_key(session_id),
        "the stale active route must not be republished after fencing"
    );
    let terminal_owner = route.owner_node_id.clone();
    coordinator.cache_route(route).await;
    assert!(
        coordinator
            .route(session_id)
            .await
            .expect("terminal route")
            .is_none(),
        "terminal routes must be negative cache entries, never authorizers"
    );
    assert!(matches!(
        coordinator
            .route_resolution_before(
                session_id,
                &terminal_owner,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("typed terminal route"),
        DurableRouteResolution::Terminal(_)
    ));

    let normalized_id = "00000000-0000-4000-8000-0000000000c4";
    let active_returned_from_end = media_route(normalized_id);
    coordinator
        .cache_terminal_route(active_returned_from_end)
        .await;
    assert!(coordinator
        .route(normalized_id)
        .await
        .expect("normalized terminal cache")
        .is_none());
    assert!(matches!(
        coordinator
            .route_resolution_before(
                normalized_id,
                "node-c",
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("normalized terminal resolution"),
        DurableRouteResolution::Terminal(_)
    ));

    let unproved_id = "00000000-0000-4000-8000-0000000000c5";
    assert!(coordinator
        .complete_release_with_route(media_route(unproved_id))
        .await
        .is_none());
    assert!(
        coordinator
            .route(unproved_id)
            .await
            .expect("unproved route lookup")
            .is_none(),
        "a nonterminal End result cannot mint a durable proof or cache authority"
    );
}

#[tokio::test(start_paused = true)]
async fn release_reconciliation_elects_once_and_retries_with_one_fence() {
    use plurx_core::cluster::membership::MembershipManager;
    use plurx_core::store::SqliteStore;

    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let coordinator = MediaSessionCoordinator::new(MembershipManager::unavailable(), store);
    let session_id = "00000000-0000-4000-8000-0000000000d1";

    let winner = match coordinator.begin_release_reconciliation(session_id).await {
        ReleaseAdmission::Won(settlement) => settlement,
        ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("first release wins"),
    };
    let follower = match coordinator.begin_release_reconciliation(session_id).await {
        ReleaseAdmission::Joined(settlement) => settlement,
        ReleaseAdmission::Won(_) | ReleaseAdmission::Full => {
            panic!("a duplicate DELETE must join the existing settlement")
        }
    };
    assert!(Arc::ptr_eq(&winner, &follower));
    assert!(coordinator.route(session_id).await.is_err());

    coordinator
        .defer_release_reconciliation(session_id, &winner)
        .await;
    assert!(coordinator
        .take_release_reconciliation_candidates()
        .await
        .is_empty());
    tokio::time::advance(RELEASE_RECONCILIATION_RETRY_BACKOFF).await;
    let candidates = coordinator.take_release_reconciliation_candidates().await;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].0, session_id);
    assert!(Arc::ptr_eq(&candidates[0].1, &winner));
    assert!(
        coordinator
            .take_release_reconciliation_candidates()
            .await
            .is_empty(),
        "an in-flight retry cannot be selected twice"
    );

    coordinator.complete_release_absent(session_id).await;
    assert!(matches!(
        coordinator
            .route_resolution_before(session_id, "local", Instant::now() + Duration::from_secs(1),)
            .await
            .expect("definitive release result"),
        DurableRouteResolution::Absent
    ));
    assert!(matches!(
        coordinator.begin_release_reconciliation(session_id).await,
        ReleaseAdmission::Joined(_)
    ));
    coordinator
        .complete_release_settlement(session_id, &winner, StatusCode::NO_CONTENT)
        .await;
    assert_eq!(winner.wait().await, StatusCode::NO_CONTENT);
    assert_eq!(follower.wait().await, StatusCode::NO_CONTENT);
    let second = match coordinator.begin_release_reconciliation(session_id).await {
        ReleaseAdmission::Won(settlement) => settlement,
        ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("second release wins"),
    };
    coordinator.complete_release_absent(session_id).await;
    coordinator
        .complete_release_settlement(session_id, &second, StatusCode::NO_CONTENT)
        .await;
}

#[test]
fn expired_active_route_is_retryable_owner_transition_not_terminal() {
    let session_id = "00000000-0000-4000-8000-0000000000c3";
    let mut route = media_route(session_id);
    route.lease_expires_at_ms = 9_999;
    assert!(matches!(
        classify_durable_route(Some(route.clone()), &route.owner_node_id, 10_000),
        DurableRouteResolution::OwnerTransition(exact)
            if exact.incarnation_id == route.incarnation_id
    ));

    route.state = "ended".to_owned();
    assert!(matches!(
        classify_durable_route(Some(route), "test-node", 10_000),
        DurableRouteResolution::Terminal(_)
    ));
}

#[tokio::test]
async fn relay_forwards_only_media_headers_without_buffering_the_body() {
    let app = Router::new().route(
        "/media",
        get(|| async {
            // Consume an earlier request/preparation budget before the
            // peer publishes headers. Body streaming must start a new,
            // bounded lifecycle after this point.
            tokio::time::sleep(Duration::from_millis(25)).await;
            let chunks =
                stream::once(async { Ok::<Bytes, Infallible>(Bytes::from_static(b"first")) })
                    .chain(stream::pending::<Result<Bytes, Infallible>>());
            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, "video/mp2t")
                .header(header::CACHE_CONTROL, "private, max-age=1")
                .header(header::CONTENT_RANGE, "bytes 0-4/10")
                .header(header::CONTENT_DISPOSITION, "inline; filename=segment.ts")
                .header(header::SET_COOKIE, "peer-secret=must-not-leak")
                .header(header::CONNECTION, "close")
                .header("x-peer-internal", "must-not-leak")
                .body(Body::from_stream(chunks))
                .expect("fixture response")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind relay fixture");
    let address = listener.local_addr().expect("relay fixture address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve relay fixture");
    });
    let preparation_deadline = tokio::time::Instant::now() + Duration::from_millis(5);
    let peer = reqwest::Client::new()
        .get(format!("http://{address}/media"))
        .send()
        .await
        .expect("request peer stream");
    assert!(
        tokio::time::Instant::now() >= preparation_deadline,
        "the fixture consumed the pre-header preparation budget"
    );

    let relayed =
        relay_response_with_limits(peer, Duration::from_millis(250), Duration::from_millis(75))
            .expect("build streamed relay");
    assert_eq!(relayed.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        relayed.headers().get(header::CONTENT_TYPE),
        Some(&"video/mp2t".parse().expect("content type"))
    );
    assert!(relayed.headers().get(header::CONTENT_RANGE).is_some());
    assert!(relayed.headers().get(header::CONTENT_DISPOSITION).is_some());
    assert!(relayed.headers().get(header::SET_COOKIE).is_none());
    assert!(relayed.headers().get(header::CONNECTION).is_none());
    assert!(relayed.headers().get("x-peer-internal").is_none());

    let mut stream = relayed.into_body().into_data_stream();
    assert_eq!(
        stream
            .next()
            .await
            .expect("first relayed chunk")
            .expect("first relayed bytes"),
        Bytes::from_static(b"first"),
        "relay_response forwards the first peer chunk without buffering"
    );
    let error = tokio::time::timeout(Duration::from_millis(150), stream.next())
        .await
        .expect("the body adapter must settle by its no-progress deadline")
        .expect("deadline body error")
        .expect_err("a peer stalled after one chunk must fail the relayed body");
    assert!(error.to_string().contains("no-progress deadline"));
    server.abort();
}

#[tokio::test]
async fn relay_total_lifetime_fires_while_body_is_unpolled_and_backpressured() {
    let app = Router::new().route(
        "/media",
        get(|| async {
            let chunks = stream::iter(
                (0..32).map(|_| Ok::<Bytes, Infallible>(Bytes::from_static(b"chunk"))),
            )
            .chain(stream::pending::<Result<Bytes, Infallible>>());
            Response::builder()
                .body(Body::from_stream(chunks))
                .expect("relay backpressure response")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind relay backpressure fixture");
    let address = listener.local_addr().expect("relay fixture address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve relay backpressure fixture");
    });
    let peer = reqwest::Client::new()
        .get(format!("http://{address}/media"))
        .send()
        .await
        .expect("request peer stream");
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
    let upstream_pulls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let relayed = relay_response_with_limits_observed(
        peer,
        Duration::from_millis(75),
        Duration::from_secs(5),
        Some(settled_tx),
        Some(Arc::clone(&upstream_pulls)),
        None,
        None,
    )
    .expect("build independently pumped relay");

    // Do not poll the body. The bounded channel fills, but the pump's
    // absolute timer remains independently scheduled and must settle.
    tokio::time::timeout(Duration::from_millis(250), settled_rx)
        .await
        .expect("unpolled relay pump obeys total lifetime")
        .expect("relay pump completion signal");
    let pulls = upstream_pulls.load(std::sync::atomic::Ordering::Acquire);
    assert!(pulls > 0, "the fixture must exercise the upstream reader");
    assert!(
        pulls <= RELAY_BODY_CHANNEL_CAPACITY,
        "the relay must reserve channel capacity before each upstream pull: {pulls}"
    );
    let mut body = relayed.into_body().into_data_stream();
    let mut chunks = 0_usize;
    let error = loop {
        match body.next().await {
            Some(Ok(_)) => chunks += 1,
            Some(Err(error)) => break error,
            None => panic!("total deadline must remain visible after buffered chunks"),
        }
    };
    assert!(chunks <= RELAY_BODY_CHANNEL_CAPACITY);
    assert!(error.to_string().contains("total lifetime"));
    server.abort();
}

#[tokio::test]
async fn bounded_relay_rejects_declared_and_chunked_oversize_bodies() {
    let app = Router::new()
        .route(
            "/declared",
            get(|| async {
                Response::builder()
                    .header(header::CONTENT_LENGTH, "6")
                    .body(Body::from("123456"))
                    .expect("declared response")
            }),
        )
        .route(
            "/chunked",
            get(|| async {
                Body::from_stream(stream::iter([
                    Ok::<Bytes, Infallible>(Bytes::from_static(b"123")),
                    Ok::<Bytes, Infallible>(Bytes::from_static(b"456")),
                ]))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind bounded relay fixture");
    let address = listener.local_addr().expect("bounded relay address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve bounded relay fixture");
    });
    let client = reqwest::Client::new();
    let declared = client
        .get(format!("http://{address}/declared"))
        .send()
        .await
        .expect("declared peer response");
    assert!(matches!(
        relay_response_with_limits_observed(
            declared,
            Duration::from_secs(1),
            Duration::from_secs(1),
            None,
            None,
            None,
            Some(5),
        ),
        Err(PeerTransportError::InvalidResponse)
    ));

    let chunked = client
        .get(format!("http://{address}/chunked"))
        .send()
        .await
        .expect("chunked peer response");
    let response = relay_response_with_limits_observed(
        chunked,
        Duration::from_secs(1),
        Duration::from_secs(1),
        None,
        None,
        None,
        Some(5),
    )
    .expect("build bounded chunked relay");
    // HTTP framing may coalesce the source chunks before reqwest exposes
    // them. Either way, a body whose running total crosses the ceiling
    // must terminate with the relay's bounded error.
    let error = axum::body::to_bytes(response.into_body(), 16)
        .await
        .expect_err("chunked body crosses the byte ceiling");
    assert!(error.to_string().contains("byte ceiling"));
    server.abort();
}

#[tokio::test]
async fn dropping_relay_body_cancels_its_network_pump() {
    let app = Router::new().route(
        "/media",
        get(|| async {
            Response::builder()
                .body(Body::from_stream(stream::pending::<
                    Result<Bytes, Infallible>,
                >()))
                .expect("relay cancellation response")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind relay cancellation fixture");
    let address = listener.local_addr().expect("relay fixture address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve relay cancellation fixture");
    });
    let peer = reqwest::Client::new()
        .get(format!("http://{address}/media"))
        .send()
        .await
        .expect("request peer stream");
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
    let relayed = relay_response_with_limits_observed(
        peer,
        Duration::from_secs(30),
        Duration::from_secs(30),
        Some(settled_tx),
        None,
        None,
        None,
    )
    .expect("build cancellable relay");
    drop(relayed);
    tokio::time::timeout(Duration::from_millis(250), settled_rx)
        .await
        .expect("body Drop cancels the sole network owner")
        .expect("relay pump completion signal");
    server.abort();
}

#[tokio::test]
async fn terminal_control_relay_accepts_and_replays_the_exact_ended_response() {
    let mut request = terminal_relay_request();
    request.control.selection.quality = crate::playback_control::QualitySelection::Auto {
        height: None,
        candidate_id: Some(plurx_core::playback::candidate::CandidateId([0x12; 16])),
    };
    assert!(request.is_valid());
    let mut expected = terminal_relay_response(&request);
    expected.effective_selection.candidate_id =
        Some(plurx_core::playback::candidate::CandidateId([0x12; 16]));
    let body = serde_json::to_vec(&expected).expect("terminal relay response");

    for disposition in ["accepted", "replayed"] {
        let response = validated_control_relay_response(
            PeerResponse {
                dv_processing_header: None,
                clock_timing: None,
                status: reqwest::StatusCode::OK,
                body: body.clone(),
            },
            &request,
        )
        .unwrap_or_else(|error| panic!("{disposition} terminal relay response: {error:?}"));
        assert_eq!(response.status(), StatusCode::OK);
        let response_body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("bounded relayed terminal body");
        let decoded =
            serde_json::from_slice::<crate::playback_control::ControlResponseV1>(&response_body)
                .expect("decode relayed terminal body");
        assert_eq!(decoded, expected, "{disposition} relay changed the ack");
    }
}

#[tokio::test]
async fn processing_owner_header_preserves_old_control_body_and_refuses_wrong_incarnation() {
    let request = terminal_relay_request();
    let mut expected = terminal_relay_response(&request);
    expected.effective_selection.dynamic_range = Some("hdr10".into());
    let body = serde_json::to_vec(&expected).expect("legacy control body");
    for generation in [&request.generation, &request.session_id] {
        let header = serde_json::json!({"generation":generation,"hdr10_enhanced":true,"fel_contributed":false,
            "applied_operations":["RpuColorConversion","TargetMapping"]}).to_string();
        let response = validated_control_relay_response(
            PeerResponse {
                dv_processing_header: Some(header),
                clock_timing: None,
                status: reqwest::StatusCode::OK,
                body: body.clone(),
            },
            &request,
        )
        .expect("ordinary-compatible relay");
        assert_eq!(
            response.headers().contains_key("x-plurx-dv-processing"),
            generation == &request.generation
        );
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .expect("relay body");
        let parsed: crate::playback_control::ControlResponseV1 =
            serde_json::from_slice(&bytes).expect("old strict body still decodes");
        assert_eq!(parsed, expected);
    }
}
