//! Actual signed member forwarding in the existing disposable CGNAT fixture.
//! Both B nodes share a filesystem; no remote-mount qualification is claimed.
use super::*;
use axum::http::StatusCode;
use std::time::Duration;

struct ServerOwner {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<anyhow::Result<crate::HttpDrain>>,
}
impl ServerOwner {
    fn serve(observation: Arc<crate::StartupObservationHttp>, router: axum::Router) -> Self {
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
    fn serve_tls(
        listener: plurx_core::sharing_tls::SharingTlsListener,
        router: axum::Router,
    ) -> Self {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(crate::serve_http(
            listener,
            router,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        Self {
            stop: Some(stop),
            task,
        }
    }
    async fn finish(mut self) {
        let _ = self.stop.take().expect("owned stop").send(());
        let drained = tokio::time::timeout(Duration::from_secs(15), &mut self.task)
            .await
            .expect("finite actual driver drain")
            .expect("server join")
            .expect("server result");
        assert_eq!(drained, crate::HttpDrain::Complete);
    }
}
impl Drop for ServerOwner {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}
struct ClientOwner(tokio::task::JoinHandle<Result<(), hyper::Error>>);
impl Drop for ClientOwner {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated Linux CGNAT namespace and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_receiver_nonowner_signed_http_resource_and_end_require_actual_driver_closure() {
    let address: IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit disposable namespace")
        .parse()
        .expect("fixture IP");
    assert!(plurx_core::sharing::is_tailnet_address(address));
    Box::pin(tokio::time::timeout(
        Duration::from_secs(330),
        actual_nonowner_b(address),
    ))
    .await
    .expect("finite actual forwarding fixture");
}
async fn actual_nonowner_b(address: IpAddr) {
    use plurx_core::{
        cluster::membership::ClusterRole,
        sharing_tls::{LiveNodeTls, SharingTlsListener},
    };
    let worker_listener = tokio::net::TcpListener::bind((address, 0))
        .await
        .expect("B worker bind");
    let worker_address = worker_listener.local_addr().expect("worker address");
    drop(worker_listener); // Preserve the chosen origin for the startup socket owner.
    let fixture = real_receiver_fixture_at(address, SourceFixtureMode::Copy, worker_address).await;
    let mut worker_membership =
        super::super::shared_source_playback::SourceFixtureMembershipOwner::start(
            fixture.state.membership.clone(),
        );
    let tls = Arc::new(
        LiveNodeTls::open(
            &fixture.directory().join("forward-source-tls"),
            crate::state::clock_ms() / 1000,
        )
        .expect("actual Source TLS"),
    );
    let (pin, _) = tls.status().expect("actual Source pin");
    let source_listener = tokio::net::TcpListener::bind((address, 0))
        .await
        .expect("Source bind");
    let endpoint = Endpoint {
        ipv4: match address {
            IpAddr::V4(ip) => ip,
            _ => panic!("IPv4 fixture"),
        },
        ipv6: None,
        ts_fqdn: "source.fixture.ts.net".into(),
        port: source_listener.local_addr().expect("Source address").port(),
        spki_sha256: pin,
    };
    // Diagnostics record actual HTTP arrival/status only; they confer no
    // admission, actor ownership, or physical closure evidence.
    let source_starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let source_start_status = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_starts = Arc::clone(&source_starts);
    let observed_status = Arc::clone(&source_start_status);
    let source_app = super::super::sharing::peer_router((*fixture.source.state).clone()).layer(
        axum::middleware::from_fn(
            move |request: Request<Body>, next: axum::middleware::Next| {
                let starts = Arc::clone(&observed_starts);
                let status = Arc::clone(&observed_status);
                async move {
                    let is_start = request.method() == axum::http::Method::POST
                        && request.uri().path().ends_with("/sessions");
                    if is_start {
                        starts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let response = next.run(request).await;
                    if is_start {
                        status.store(
                            usize::from(response.status().as_u16()),
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                    response
                }
            },
        ),
    );
    let source_server =
        ServerOwner::serve_tls(SharingTlsListener::new(source_listener, tls), source_app);
    fixture.pair(endpoint).await;
    let worker_server = ServerOwner::serve(
        Arc::clone(
            fixture
                .startup_clock
                .as_ref()
                .expect("actual worker startup owner"),
        ),
        fixture_router(fixture.state.clone()),
    );
    let second_directory = crate::test_tempdir().expect("isolated second B member");
    let second_listener = tokio::net::TcpListener::bind((address, 0))
        .await
        .expect("B ingress bind");
    let second_address = second_listener.local_addr().expect("ingress address");
    let token = fixture
        .state
        .membership
        .issue_token_for_role(Duration::from_secs(120), ClusterRole::Voter)
        .await
        .expect("actual coordinator token");
    let mut config = Config::default();
    config.storage.data_dir = second_directory.path().join("database");
    config.server.bind = second_address;
    config.cluster.artwork_url = format!("http://{second_address}");
    config.cluster.join_url = format!("http://{worker_address}");
    config.cluster.join_token_file = second_directory.path().join("join.token");
    std::fs::write(&config.cluster.join_token_file, token.token).expect("local one-use token");
    let raft = std::net::TcpListener::bind((address, 0)).expect("second Raft bind");
    let api = std::net::TcpListener::bind((address, 0)).expect("second API bind");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = address.to_string();
    drop((raft, api));
    drop(second_listener);
    let observation = Arc::new(crate::StartupObservationHttp::new(second_address));
    let mut selected = Box::pin(select_daemon_store_observing(
        &config,
        Some(observation.as_ref()),
    ))
    .await
    .expect("actual second admitted B voter");
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("actual startup factory"));
    let mut ingress = super::super::source_actor_test_state();
    ingress.store = Arc::clone(&selected.store);
    ingress.node_id = selected.identity.node_id.clone();
    ingress.membership = selected.membership_manager();
    ingress.clock_observer = observation
        .observer()
        .await
        .expect("actual ingress clock observer");
    ingress.catalogue = selected.catalogue_reader();
    ingress.replication = selected.replication_monitor();
    ingress.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&selected.credential_key),
        config.storage.data_dir.clone(),
        SharingNetworkConfig {
            bind: (address, 0).into(),
            egress: SharingEgressConfig::LocalAddress { address },
            ..SharingNetworkConfig::default()
        },
    ));
    ingress
        .membership
        .set_ingress_custody_boot(Some(ingress.sharing.accepted_drivers.boot_id()));
    ingress
        .membership
        .publish_ingress_custody_boot()
        .await
        .expect("actual ingress boot");
    fixture
        .state
        .membership
        .heartbeat()
        .await
        .expect("worker roster");
    ingress
        .membership
        .heartbeat()
        .await
        .expect("ingress roster");
    assert_ne!(fixture.state.node_id, ingress.node_id);
    assert_ne!(
        fixture.state.sharing.accepted_drivers.boot_id(),
        ingress.sharing.accepted_drivers.boot_id()
    );
    // This ingress does no producer work; actual assigned B playback remains
    // owned by the serving worker and all data traverses signed member HTTP.
    let mut ingress_membership =
        super::super::shared_source_playback::SourceFixtureMembershipOwner::start(
            ingress.membership.clone(),
        );
    let ingress_server =
        ServerOwner::serve(Arc::clone(&observation), fixture_router(ingress.clone()));
    let (status, _, bytes) = b_request(
        worker_address,
        false,
        "GET",
        &format!(
            "/api/v1/shared/imports/{}/items/{}",
            fixture.import_id,
            fixture.source.reference.item_id.as_str()
        ),
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let details: Value = serde_json::from_slice(&bytes).expect("actual details");
    let base = details["files"][0]["file_base"]
        .as_str()
        .expect("signed alias");
    let original: Value = serde_json::from_slice(&fixture.source.request).expect("recipe");
    let (status, _, bytes) = b_request(
        worker_address,
        false,
        "POST",
        &format!("{base}/hls/sessions"),
        &fixture.original_login,
        serde_json::to_vec(&original["session"]).expect("canonical session"),
    )
    .await;
    if status != StatusCode::OK {
        eprintln!(
            "actual Source HTTP diagnostic starts={}, last_status={}",
            source_starts.load(std::sync::atomic::Ordering::Relaxed),
            source_start_status.load(std::sync::atomic::Ordering::Relaxed)
        );
        eprintln!(
            "actual B diagnostic claim-stage counts: {}",
            receiver_claim_observation(
                &fixture,
                original["session"]["request_id"]
                    .as_str()
                    .expect("actual B request UUID")
            )
            .await
        );
    }
    assert_eq!(
        status,
        StatusCode::OK,
        "actual worker Start response: {}",
        String::from_utf8_lossy(&bytes)
    );
    let start: Value = serde_json::from_slice(&bytes).expect("actual B Start");
    let session = start["session_id"].as_str().expect("B session");
    let session_uuid = Uuid::parse_str(session).expect("B UUID");
    assert!(fixture
        .state
        .sharing
        .receiver_starts
        .by_session(session_uuid)
        .is_some());
    assert!(ingress
        .sharing
        .receiver_starts
        .by_session(session_uuid)
        .is_none());
    let playlist = start["playlist_url"]
        .as_str()
        .expect("actual playlist")
        .to_owned();
    let socket = tokio::net::TcpStream::connect(second_address)
        .await
        .expect("actual H2 ingress socket");
    let (mut sender, driver) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .handshake::<_, Body>(hyper_util::rt::TokioIo::new(socket))
            .await
            .expect("actual H2 handshake");
    let mut driver = ClientOwner(tokio::spawn(driver));
    let mut request = Request::builder()
        .method("GET")
        .uri(format!("http://{second_address}{playlist}"))
        .header(
            "authorization",
            format!("Bearer {}", fixture.original_login),
        )
        .body(Body::empty())
        .expect("resource");
    let response = sender
        .send_request(request)
        .await
        .expect("forwarded protected resource");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(Body::new(response.into_body()), 1024 * 1024)
        .await
        .expect("protected bytes");
    assert!(bytes.starts_with(b"#EXTM3U"));
    assert!(
        ingress
            .sharing
            .receiver_starts
            .by_session(session_uuid)
            .is_none(),
        "SQL routing cannot adopt the actual actor"
    );
    {
        let actual = fixture
            .state
            .sharing
            .receiver_starts
            .pause_actual_actor_for_fixture(session_uuid)
            .expect("pause exact actual owner without replacing its physical invocation");
        assert!(fixture
            .state
            .sharing
            .receiver_starts
            .by_session(session_uuid)
            .is_none());
        let denied = b_request(
            second_address,
            false,
            "GET",
            &playlist,
            &fixture.original_login,
            Vec::new(),
        )
        .await;
        assert_eq!(
            denied.0,
            StatusCode::SERVICE_UNAVAILABLE,
            "current SQL/proof cannot recreate an absent actual owner"
        );
        assert!(ingress
            .sharing
            .receiver_starts
            .by_session(session_uuid)
            .is_none());
        drop(actual); // restore precisely the original retained Arc, never SQL.
        assert!(fixture
            .state
            .sharing
            .receiver_starts
            .by_session(session_uuid)
            .is_some());
    }
    // A real metadata fault changes only the advertised owner epoch. Restore
    // the injected fault before cleanup; this is not a production takeover.
    let route = fixture
        .state
        .store
        .media_session_route(session)
        .await
        .expect("actual route")
        .expect("route exists");
    let db = fixture
        .selected
        .local_client()
        .expect("actual B voter client");
    db.execute(
        "UPDATE media_sessions SET owner_epoch=owner_epoch+1 WHERE session_id=$1",
        hiqlite::params!(session),
    )
    .await
    .expect("inject changed metadata fence");
    let denied = b_request(
        second_address,
        false,
        "GET",
        &playlist,
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    db.execute(
        "UPDATE media_sessions SET owner_epoch=$1 WHERE session_id=$2",
        hiqlite::params!(route.owner_epoch, session),
    )
    .await
    .expect("restore exact injected metadata");
    assert_eq!(
        denied.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "fresh SQL epoch cannot authenticate old actual owner"
    );
    request = Request::builder()
        .method("DELETE")
        .uri(format!("http://{second_address}/api/v1/hls/{session}"))
        .header(
            "authorization",
            format!("Bearer {}", fixture.original_login),
        )
        .body(Body::empty())
        .expect("same-driver End");
    let response = sender
        .send_request(request)
        .await
        .expect("bounded same-driver End");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    axum::body::to_bytes(Body::new(response.into_body()), 128 * 1024)
        .await
        .expect("closing body");
    drop(sender);
    driver.0.abort();
    let _ = (&mut driver.0).await;
    // Actual owner close/ACK and Source retirement can outlive the local
    // socket join. Retry only this exact End, under one finite cleanup budget.
    tokio::time::timeout(Duration::from_secs(9), async {
        loop {
            let (status, _, bytes) = b_request(
                second_address,
                false,
                "DELETE",
                &format!("/api/v1/hls/{session}"),
                &fixture.original_login,
                Vec::new(),
            )
            .await;
            if status == StatusCode::NO_CONTENT {
                assert!(bytes.is_empty());
                break;
            }
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "only unresolved exact cleanup may precede actual confirmation"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("actual confirmed End after driver closure under original bounded retry budget");
    assert!(
        ingress
            .sharing
            .receiver_starts
            .by_session(session_uuid)
            .is_none(),
        "End metadata never creates an actor"
    );
    ingress_server.finish().await;
    worker_server.finish().await;
    source_server.finish().await;
    ingress_membership.finish().await;
    worker_membership.finish().await;
    selected.shutdown().await.expect("second voter shutdown");
    fixture.shutdown().await;
}
