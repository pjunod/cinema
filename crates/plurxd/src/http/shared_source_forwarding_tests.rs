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
    Box::pin(tokio::time::timeout(
        Duration::from_secs(120),
        actual_nonowner_forwarding(),
    ))
    .await
    .expect("finite real-member forwarding fixture");
}
async fn actual_nonowner_forwarding() {
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
    drop(worker_listener); // The startup owner binds this exact reserved origin.
    let worker = real_source_start_fixture_at(worker_address).await;
    let worker_router = super::super::router((*worker.state).clone())
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
        .membership
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
    drop(ingress_listener);
    let observation = Arc::new(crate::StartupObservationHttp::new(ingress_address));
    let mut selected = Box::pin(select_daemon_store_observing(
        &config,
        Some(observation.as_ref()),
    ))
    .await
    .expect("actual joined second voter");
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("actual candidate startup"));
    let mut ingress = crate::http::source_actor_test_state();
    ingress.store = Arc::clone(&selected.store);
    ingress.membership = selected.membership_manager();
    ingress.clock_observer = observation
        .observer()
        .await
        .expect("actual ingress clock observer");
    ingress.node_id = selected.identity.node_id.clone();
    ingress.catalogue = selected.catalogue_reader();
    ingress.replication = selected.replication_monitor();
    ingress.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&selected.credential_key),
        config.storage.data_dir.clone(),
        config.sharing.clone(),
    ));
    ingress
        .membership
        .set_ingress_custody_boot(Some(ingress.sharing.accepted_drivers.boot_id()));
    ingress
        .membership
        .publish_ingress_custody_boot()
        .await
        .expect("actual ingress registry boot");
    worker
        .state
        .membership
        .heartbeat()
        .await
        .expect("worker current membership");
    ingress
        .membership
        .heartbeat()
        .await
        .expect("ingress current membership");
    assert_ne!(worker.state.node_id, ingress.node_id);
    assert_ne!(
        worker.state.sharing.accepted_drivers.boot_id(),
        ingress.sharing.accepted_drivers.boot_id()
    );
    // A real serving admission fence makes this ingress ineligible to produce.
    // Its public peer ingress and cleanup can still route to the serving worker.
    let _fence = ingress
        .serving
        .begin_restart_preparation_until(
            u64::try_from(crate::state::clock_ms()).expect("positive clock") + 120_000,
        )
        .await
        .expect("actual non-serving ingress fence");
    assert!(!ingress.serving.accepting_new_media().await);
    let ingress_server = FixtureServer::spawn(
        Arc::clone(&observation),
        super::super::sharing::peer_router(ingress.clone()).merge(
            axum::Router::new()
                .route(
                    crate::http::internal_clock::PATH,
                    axum::routing::get(crate::http::internal_clock::observation_snapshot),
                )
                .with_state(crate::http::internal_clock::ObservationContext {
                    membership: ingress.membership.clone(),
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(axum::body::Body::new(response.into_body()), 4 * 1024 * 1024)
        .await
        .expect("Start envelope");
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
    recipe["resource"] = json!("index.m3u8");
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
    assert_eq!(response.status(), StatusCode::OK);
    let playlist = axum::body::to_bytes(axum::body::Body::new(response.into_body()), 1024 * 1024)
        .await
        .expect("protected playlist bytes");
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
