//! Real member authentication and accepted-driver forwarding. Both nodes share
//! the test filesystem; this qualifies a non-serving ingress, not remote mounts.
use super::*;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct FixtureServer {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<anyhow::Result<crate::HttpDrain>>,
}
impl FixtureServer {
    fn spawn(observation: Arc<crate::StartupObservationHttp>, router: axum::Router) -> Self {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            observation
                .serve_normal(router, async move {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            stop: Some(stop),
            task,
        }
    }
    async fn finish(mut self) {
        let _ = self.stop.take().expect("owned accept stop").send(());
        let drained = tokio::time::timeout(Duration::from_secs(10), &mut self.task)
            .await
            .expect("actual accept/driver owners drain")
            .expect("server join")
            .expect("server result");
        assert_eq!(drained, crate::HttpDrain::Complete);
    }
}
impl Drop for FixtureServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}
struct ClientDriver(tokio::task::JoinHandle<Result<(), hyper::Error>>);
impl Drop for ClientDriver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_nonowner_http_cold_probe_and_same_driver_end_require_actual_closure() {
    let fixture_body_budget =
        crate::transcode::TranscodeManager::source_fixture_body_budget().await;
    Box::pin(tokio::time::timeout(
        Duration::from_secs(120),
        actual_nonowner_forwarding(fixture_body_budget, false, SourceFixtureMode::Copy),
    ))
    .await
    .expect("finite real-member forwarding fixture");
}
/// S6/S7 across the Source cluster (ingress is not the owner, the incident
/// topology): a clocked Start is answered `startup_timeout` with
/// `Retry-After` on the viewer's clock by the ingress while the owner cannot
/// publish, and the same request then rejoins the owner's one entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_nonowner_clocked_start_answers_not_yet_and_rejoins_the_owner_entry() {
    let fixture_body_budget =
        crate::transcode::TranscodeManager::source_fixture_body_budget().await;
    Box::pin(tokio::time::timeout(
        Duration::from_secs(120),
        actual_nonowner_forwarding(fixture_body_budget, true, SourceFixtureMode::Copy),
    ))
    .await
    .expect("finite real-member forwarding fixture");
}
/// The production shape across the Source cluster: a web player's range
/// caps through a receiver that affirms bounded ranges, Dolby Vision relay
/// and processed upgrades, arriving at an ingress that is not the owner. The
/// owner runs its growing copy; the receiver's Status after Start, the
/// playlist and the End all answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_nonowner_range_copy_start_status_playlist_and_end() {
    let fixture_body_budget =
        crate::transcode::TranscodeManager::source_fixture_body_budget().await;
    Box::pin(tokio::time::timeout(
        Duration::from_secs(120),
        actual_nonowner_forwarding(fixture_body_budget, false, SourceFixtureMode::RangeCopy),
    ))
    .await
    .expect("finite real-member range forwarding fixture");
}
/// The same with a title that carries text subtitles, started with native
/// subtitles and a selected track, as the web player starts every such title.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_nonowner_range_native_copy_start_status_playlist_and_end() {
    let fixture_body_budget =
        crate::transcode::TranscodeManager::source_fixture_body_budget().await;
    Box::pin(tokio::time::timeout(
        Duration::from_secs(120),
        actual_nonowner_forwarding(
            fixture_body_budget,
            false,
            SourceFixtureMode::RangeNativeCopy,
        ),
    ))
    .await
    .expect("finite real-member native range forwarding fixture");
}
async fn actual_nonowner_forwarding(
    fixture_body_budget: tokio::sync::OwnedSemaphorePermit,
    clocked: bool,
    mode: SourceFixtureMode,
) {
    let range = matches!(
        mode,
        SourceFixtureMode::RangeCopy | SourceFixtureMode::RangeNativeCopy
    );
    use plurx_core::{
        cluster::{membership::ClusterRole, migration::select_daemon_store_observing},
        config::Config,
    };
    // Reserve the real advertised endpoints before either startup; no SQL
    // membership/boot/key advertisements substitute for actual selected nodes.
    let worker_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("worker bind");
    let worker_address = worker_listener.local_addr().expect("worker address");
    let mut worker = real_source_start_fixture_at(mode, worker_listener, fixture_body_budget).await;
    // Exercise both actual node and sharing routes with the strict sharing
    // fallback once; the ordinary web-shell fallback is irrelevant here.
    let worker_router = super::super::router((*worker.state).clone())
        .reset_fallback()
        .merge(super::super::sharing::peer_router((*worker.state).clone()));
    let worker_server = FixtureServer::spawn(
        Arc::clone(
            worker
                .startup_clock
                .as_ref()
                .expect("actual worker startup owner"),
        ),
        worker_router,
    );
    let ingress_directory = crate::test_tempdir().expect("isolated second member data");
    let ingress_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("ingress bind");
    let ingress_address = ingress_listener.local_addr().expect("ingress address");
    let token = worker
        .state
        .cluster
        .test_membership()
        .issue_token_for_role(Duration::from_secs(120), ClusterRole::Voter)
        .await
        .expect("actual coordinator-issued admission token");
    let mut config = Config::default();
    config.storage.data_dir = ingress_directory.path().join("database");
    config.server.bind = ingress_address;
    config.cluster.artwork_url = format!("http://{ingress_address}");
    config.cluster.join_url = format!("http://{worker_address}");
    config.cluster.join_token_file = ingress_directory.path().join("join.token");
    std::fs::write(&config.cluster.join_token_file, token.token)
        .expect("local one-use fixture token");
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("second Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("second API port");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    let observation = Arc::new(
        crate::StartupObservationHttp::from_listener(ingress_listener)
            .expect("owned ingress observation listener"),
    );
    let mut selected = Box::pin(select_daemon_store_observing(
        &config,
        Some(&crate::cluster::StartupClockAdapter(observation.as_ref())),
    ))
    .await
    .expect("actual joined second voter");
    assert!(
        !config.cluster.join_token_file.exists(),
        "actual identity-bound Source ingress join token must be consumed by successful startup finalization before sharing admission"
    );

    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("actual candidate startup"));
    let mut ingress = crate::http::source_actor_test_state();
    ingress.store = Arc::clone(&selected.store);
    ingress.replace_membership(selected.membership_manager());
    ingress.clock_observer = observation
        .observer()
        .await
        .expect("actual ingress clock observer");
    ingress.node_id = selected.identity.node_id.clone();
    ingress.catalogue = selected.catalogue_reader();
    ingress.replace_replication(selected.replication_monitor());
    ingress.media_sessions = crate::media_sessions::MediaSessionCoordinator::new(
        ingress.cluster.test_membership().clone(),
        Arc::clone(&ingress.store),
    );
    ingress.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&selected.credential_key),
        config.storage.data_dir.clone(),
        config.sharing.clone(),
    ));
    ingress
        .cluster
        .test_membership()
        .set_ingress_custody_boot(Some(ingress.sharing.accepted_drivers.boot_id()));
    ingress
        .cluster
        .test_membership()
        .publish_ingress_custody_boot()
        .await
        .expect("actual ingress registry boot");
    worker
        .state
        .cluster
        .test_membership()
        .heartbeat()
        .await
        .expect("worker current membership");
    ingress
        .cluster
        .test_membership()
        .heartbeat()
        .await
        .expect("ingress current membership");
    // Follow actual Raft progress changes while the production sampler and
    // heartbeat publish bounded readiness. Never manufacture a progress row.
    let mut progress = selected
        .local_client()
        .expect("actual ingress local client")
        .local_db_raft_metrics()
        .expect("actual applied progress watch");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let peers = ingress
                .cluster
                .test_membership()
                .cluster_access()
                .directory(None)
                .media_observation(true)
                .await
                .expect("actual bounded-ready media peer observation");
            if peers.iter().any(|peer| {
                peer.node_id == worker.state.node_id && peer.reachable && peer.http_base.is_some()
            }) {
                break;
            }
            assert!(
                progress.wait_for_change().await,
                "actual applied progress watch closed before Source placement readiness"
            );
        }
    })
    .await
    .expect("Source worker production passive/quorum placement readiness");
    worker.keep_fixture_membership_stable().await;
    assert_ne!(worker.state.node_id, ingress.node_id);
    assert_ne!(
        worker.state.sharing.accepted_drivers.boot_id(),
        ingress.sharing.accepted_drivers.boot_id()
    );
    // A real serving admission fence makes this ingress ineligible to produce.
    // Its public peer ingress and cleanup can still route to the serving worker.
    let _fence = ingress
        .cluster
        .test_fence()
        .begin_restart_preparation_until(
            u64::try_from(crate::state::clock_ms()).expect("positive clock") + 120_000,
        )
        .await
        .expect("actual non-serving ingress fence");
    assert!(!ingress.cluster.test_fence().accepting_new_media().await);
    let ingress_server = FixtureServer::spawn(
        Arc::clone(&observation),
        super::super::sharing::peer_router(ingress.clone()).merge(
            axum::Router::new()
                .route(
                    crate::http::internal_clock::PATH,
                    axum::routing::get(crate::http::internal_clock::observation_snapshot),
                )
                .with_state(crate::http::internal_clock::ObservationContext {
                    clock: ingress.cluster.operations().clock(),
                    node_id: ingress.node_id.clone(),
                    observer: ingress.clock_observer.clone(),
                }),
        ),
    );
    let socket = tokio::net::TcpStream::connect(ingress_address)
        .await
        .expect("actual outer H2 socket");
    let (mut sender, driver) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
            .await
            .expect("actual H2 handshake");
    let mut driver = ClientDriver(tokio::spawn(driver));
    let path = format!(
        "http://{ingress_address}/sharing/v1/items/{}/files/{}/sessions",
        worker.reference.item_id.as_str(),
        worker.reference.file_id.as_str()
    );
    // Both real voters and the worker's placement are established above.
    // Keep the worker roster stable through the cold Start and rejoin.
    worker.keep_fixture_membership_stable().await;
    if clocked {
        // The owner cannot publish while its software capacity is held.
        let hold = worker
            .state
            .transcode
            .test_hold_source_software_capacity()
            .expect("owner capacity held");
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(&path)
            .body(axum::body::Body::from(worker.request.clone()))
            .expect("clocked Start request");
        *request.headers_mut() = worker.headers.clone();
        request.headers_mut().insert(
            super::super::hls::STARTUP_REMAINING_HEADER,
            axum::http::HeaderValue::from_static("1500"),
        );
        let began = Instant::now();
        let response = sender
            .send_request(request)
            .await
            .expect("forwarded clocked Start");
        let status = response.status();
        let retry_after = response.headers().get("retry-after").cloned();
        let bytes = axum::body::to_bytes(axum::body::Body::new(response.into_body()), 128 * 1024)
            .await
            .expect("not-yet body");
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let reply: Value = serde_json::from_slice(&bytes).expect("typed not-yet");
        assert_eq!(reply["code"], "startup_timeout");
        assert_eq!(
            retry_after.as_ref().and_then(|v| v.to_str().ok()),
            Some("1")
        );
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "the ingress answers on the viewer's clock, not the owner's 305 s"
        );
        drop(hold);
    }
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(&path)
        .body(axum::body::Body::from(worker.request.clone()))
        .expect("Start request");
    let mut request = request;
    *request.headers_mut() = worker.headers.clone();
    let response = sender
        .send_request(request)
        .await
        .expect("actual forwarded Start");
    let start_status = response.status();
    let bytes = axum::body::to_bytes(axum::body::Body::new(response.into_body()), 4 * 1024 * 1024)
        .await
        .expect("Start envelope");
    assert_eq!(
        start_status,
        StatusCode::OK,
        "actual forwarded Source Start response: {}",
        String::from_utf8_lossy(&bytes)
    );
    let decoded = super::super::decode_source_start_response(&bytes, &worker.reference)
        .expect("strict Source envelope");
    let mut recipe: Value = serde_json::from_slice(&worker.request).expect("canonical recipe");
    let request_id = recipe["session"]["request_id"]
        .as_str()
        .expect("request ID")
        .to_owned();
    recipe["incarnation_id"] = json!(decoded.incarnation_id());
    recipe["session_id"] = json!(decoded.response().session_id);
    recipe["control_epoch"] = json!(
        decoded
            .response()
            .control
            .as_ref()
            .expect("control")
            .control_epoch
    );
    let entry = worker
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("actual registry")
        .first()
        .cloned()
        .expect("worker-owned invocation");
    let owned = entry
        .wait(Instant::now() + Duration::from_secs(1))
        .await
        .expect("actual retained worker");
    assert_eq!(owned.assignment.owner_node_id(), worker.state.node_id);
    assert_eq!(
        worker
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("actual registry")
            .len(),
        1,
        "the retry rejoined the owner's one entry (clocked={clocked})"
    );
    assert!(
        ingress
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("ingress registry")
            .is_empty(),
        "routing metadata cannot adopt an actor"
    );
    let operation = format!("{path}/{request_id}");
    // The session's own playlist: the media playlist for an SDR copy, the
    // master for native subtitles or an HDR or Dolby Vision range.
    let resource = decoded
        .response()
        .playlist_url
        .strip_prefix(&format!("/api/v1/hls/{}/", decoded.response().session_id))
        .expect("session playlist")
        .to_owned();
    if range {
        // The growing copy is a bounded view, presentable only to a client
        // and receiver that both affirmed ranges, as these did.
        assert!(!decoded.response().vod);
        assert!(
            super::receiver_presentation::response_presentable(decoded.response(), true, true),
            "a receiver that affirmed ranges accepts the Start"
        );
        // A receiver confirms every Start with a Status before it answers its
        // viewer; it must read the same published session back.
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("{operation}/status"))
            .body(axum::body::Body::from(
                serde_json::to_vec(&recipe).expect("status body"),
            ))
            .expect("status request");
        *request.headers_mut() = worker.headers.clone();
        let response = sender
            .send_request(request)
            .await
            .expect("forwarded status");
        let status = response.status();
        let bytes =
            axum::body::to_bytes(axum::body::Body::new(response.into_body()), 4 * 1024 * 1024)
                .await
                .expect("status envelope");
        assert_eq!(
            status,
            StatusCode::OK,
            "forwarded Source Status after Start: {}",
            String::from_utf8_lossy(&bytes)
        );
        let confirmed = super::super::decode_source_start_response(&bytes, &worker.reference)
            .expect("strict Source status envelope");
        assert_eq!(confirmed.incarnation_id(), decoded.incarnation_id());
        assert_eq!(
            confirmed.response().session_id,
            decoded.response().session_id
        );
    }
    recipe["resource"] = json!(resource);
    let mut request = axum::http::Request::builder()
        .method("POST")
        .uri(format!("{operation}/resources"))
        .body(axum::body::Body::from(
            serde_json::to_vec(&recipe).expect("resource body"),
        ))
        .expect("resource request");
    *request.headers_mut() = worker.headers.clone();
    let response = sender
        .send_request(request)
        .await
        .expect("forwarded resource");
    let resource_status = response.status();
    let playlist = axum::body::to_bytes(axum::body::Body::new(response.into_body()), 1024 * 1024)
        .await
        .expect("protected playlist bytes");
    assert_eq!(
        resource_status,
        StatusCode::OK,
        "forwarded playlist: {}",
        String::from_utf8_lossy(&playlist)
    );
    assert!(playlist.starts_with(b"#EXTM3U"));
    recipe.as_object_mut().expect("object").remove("resource");
    let known_body = serde_json::to_vec(&recipe).expect("exact cleanup body");
    let mut request = axum::http::Request::builder()
        .method("POST")
        .uri(format!("{operation}/end"))
        .body(axum::body::Body::from(known_body.clone()))
        .expect("same-driver End");
    *request.headers_mut() = worker.headers.clone();
    let response = sender
        .send_request(request)
        .await
        .expect("bounded same-driver End");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    axum::body::to_bytes(axum::body::Body::new(response.into_body()), 128 * 1024)
        .await
        .expect("closing body");
    drop(sender);
    driver.0.abort();
    let _ = (&mut driver.0).await;
    // Actual socket closure and exact owner settlement, not row absence, allow
    // the stable retry to produce the final End receipt.
    // Same-driver End can retain an unresolved result while the detached
    // owner retries a close/ACK race. Only its actual settlement permits retry.
    tokio::time::timeout(Duration::from_secs(20), async {
        while owned.actor.settlement_status() != Some(Ok(())) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual retained owner settles after driver closure");
    let end = entry.end(Arc::clone(&worker.state));
    end.wait(Instant::now() + Duration::from_secs(15))
        .await
        .expect("actual owner closure and durable ACK");
    let response = tests::actual_resource_request(
        ingress_address,
        false,
        &format!("{operation}/end"),
        worker.headers.clone(),
        known_body,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
        .await
        .expect("confirmed receipt");
    let receipt: Value = serde_json::from_slice(&bytes).expect("actual exact End receipt");
    assert_eq!(receipt["incarnation_id"], json!(decoded.incarnation_id()));
    assert_eq!(receipt["reference"], json!(worker.reference));
    assert_eq!(receipt["settled"], json!(true));
    ingress_server.finish().await;
    worker_server.finish().await;
    selected
        .shutdown()
        .await
        .expect("second actual voter shutdown");
    worker.shutdown().await;
}

#[test]
fn sharing_source_forwarding_carries_the_dolby_vision_affirmation() {
    use super::receiver_presentation::{internal_range, normalize_wire, select_persisted_recipe};
    let raw = serde_json::to_vec(&json!({"session":{"caps":{"transports":["hls","hls-bounded-range-v1"],"video":[{"codec":"hevc","dv_profiles":[8]}]}}}))
        .expect("raw");
    // The ingress applies the affirmation to the canonical body every
    // forwarded owner plans from: the affirmed recipe keeps its Dolby Vision
    // caps, the unaffirmed one has none, and neither carries a token an owner
    // of any version would refuse.
    for ready in [false, true] {
        for dolby_vision in [false, true] {
            let effective = normalize_wire(&raw, ready, dolby_vision).expect("effective");
            assert_eq!(internal_range(&effective).expect("closed tokens"), ready);
            let value: serde_json::Value = serde_json::from_slice(&effective).expect("json");
            assert_eq!(
                value["session"]["caps"]["video"][0]
                    .get("dv_profiles")
                    .is_some(),
                dolby_vision
            );
            // A later operation re-posts the raw public recipe; the retained
            // route's fingerprint selects the recipe it was planned with.
            let stored = super::ingress::fingerprint_recipe(&effective, ready);
            assert_eq!(
                select_persisted_recipe(&raw, &stored, false).expect("retained route"),
                effective
            );
        }
    }
}
