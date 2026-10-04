//! Real selected-store fixtures for B-to-Source playback qualification.
//! These helpers install only the production pre-serving factories.
use super::shared_source_playback::{
    real_source_start_fixture_with, RealSourceStartFixture, SourceFixtureMode,
};
use axum::{body::Body, http::Request};
use plurx_core::{
    cluster::migration::{select_daemon_store, SelectedStore},
    config::{Config, SharingEgressConfig, SharingNetworkConfig},
    sharing::{Assignment, Endpoint, ImportOutcome, MutationOutcome, NewImport, SecretDomain},
    store::keys,
};
use serde_json::{json, Value};
use std::{net::IpAddr, sync::Arc};
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) struct RealReceiverFixture {
    pub state: crate::state::AppState,
    pub source: RealSourceStartFixture,
    pub original_login: String,
    pub viewer_id: i64,
    pub import_id: Uuid,
    selected: SelectedStore,
    directory: tempfile::TempDir,
}

pub(crate) fn real_receiver_fixture(
    address: IpAddr,
    mode: SourceFixtureMode,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = RealReceiverFixture> + Send>> {
    Box::pin(build_receiver_fixture(address, mode))
}

async fn build_receiver_fixture(address: IpAddr, mode: SourceFixtureMode) -> RealReceiverFixture {
    let directory = crate::test_tempdir().expect("real B directory");
    let mut config = Config::default();
    config.storage.data_dir = directory.path().join("database");
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("B Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("B API port");
    config.cluster.raft_bind = raft.local_addr().expect("B Raft address");
    config.cluster.api_bind = api.local_addr().expect("B API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    let mut selected = Box::pin(select_daemon_store(&config))
        .await
        .expect("actual selected B voter");
    selected
        .store
        .put_setting(keys::SHARING_ENABLED, "true")
        .await
        .expect("saved B choice");
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("B production pre-serving factory"));
    let identity = selected
        .store
        .sharing_identity(crate::state::clock_ms())
        .await
        .expect("actual B sharing identity");
    let source = real_source_start_fixture_with(mode, Some(identity.server_id)).await;
    let mut state = super::source_actor_test_state();
    state.store = Arc::clone(&selected.store);
    state.node_id = selected.identity.node_id.clone();
    state.membership = selected.membership_manager();
    state.catalogue = selected.catalogue_reader();
    state.replication = selected.replication_monitor();
    state.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&selected.credential_key),
        config.storage.data_dir.clone(),
        SharingNetworkConfig {
            bind: (address, 0).into(),
            egress: SharingEgressConfig::LocalAddress { address },
        },
    ));
    state.transcode = Arc::new(crate::transcode::TranscodeManager::new(
        Arc::clone(&state.store),
        directory.path().join("workers"),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    ));
    let password = "actual-receiver-fixture-password";
    let password_hash = plurx_core::auth::hash_password(password).expect("real password hash");
    let viewer = state
        .store
        .create_user("actual-receiver-viewer", &password_hash, false)
        .await
        .expect("actual B viewer");
    let response = super::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username":viewer.username,"password":password,"device":"real-source-fixture"})
                        .to_string(),
                ))
                .expect("actual login request"),
        )
        .await
        .expect("actual B login router");
    assert!(response.status().is_success(), "actual login succeeds");
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("bounded actual login response");
    let login: Value = serde_json::from_slice(&bytes).expect("actual login JSON");
    assert_eq!(login["user"]["id"].as_i64(), Some(viewer.id));
    let original_login = login["token"]
        .as_str()
        .expect("actual original token")
        .to_owned();
    RealReceiverFixture {
        state,
        source,
        original_login,
        viewer_id: viewer.id,
        import_id: Uuid::new_v4(),
        selected,
        directory,
    }
}

impl RealReceiverFixture {
    /// Pair from the actual Source invitation/grant using B's own sealed keys.
    /// The caller supplies its actual bound TLS endpoint, never a Source URL.
    pub(crate) async fn pair(&self, endpoint: Endpoint) {
        let invitation = self
            .source
            .invitation_for(endpoint)
            .expect("genuine invitation");
        let credential = self.source.headers["authorization"]
            .to_str()
            .expect("actual credential header")
            .strip_prefix("CinemaShare ")
            .expect("actual credential scheme");
        let secret = plurx_core::secrets::Secret::from_cleartext(credential);
        let hash = plurx_core::sharing::secret_hash(SecretDomain::Grant, &secret);
        let status = self
            .source
            .state
            .store
            .sharing_grant_status(&hash)
            .await
            .expect("actual Source grant status")
            .expect("actual approved grant");
        let identity = self
            .state
            .store
            .sharing_identity(crate::state::clock_ms())
            .await
            .expect("actual B identity");
        assert_eq!(status.grant.recipient_server_id, identity.server_id);
        assert_eq!(status.grant.id, self.source.grant);
        assert_eq!(status.invitation_id, invitation.id);
        let now = crate::state::clock_ms();
        let clear = crate::sharing::ImportCredential::encode(
            &secret,
            invitation.id,
            now,
            &status.recipient_name,
            invitation.expires_at_ms,
            Some(status.grant.pending_expires_at_ms),
        )
        .expect("real ImportCredential");
        let credential = self
            .selected
            .credential_key
            .seal_sharing(
                plurx_core::secrets::SharingSecretPurpose::Credential,
                identity.server_id,
                self.import_id,
                clear.expose(),
            )
            .expect("actual B Credential envelope");
        let invitation_secret = invitation.encode().expect("genuine invitation blob");
        let claim_secret = self
            .selected
            .credential_key
            .seal_sharing(
                plurx_core::secrets::SharingSecretPurpose::Claim,
                identity.server_id,
                self.import_id,
                invitation_secret.expose(),
            )
            .expect("actual B Claim envelope");
        assert_eq!(
            self.state
                .store
                .create_share_import(NewImport {
                    id: self.import_id,
                    source: invitation.identity,
                    source_name: invitation.name,
                    claim_id: status.claim_id,
                    credential,
                    claim_secret,
                    endpoints: invitation.endpoints,
                    now_ms: now,
                })
                .await
                .expect("actual B import"),
            ImportOutcome::Created
        );
        assert_eq!(
            self.state
                .store
                .settle_current_share_claim(
                    self.import_id,
                    status.claim_id,
                    1,
                    status.grant.id,
                    true,
                    now,
                )
                .await
                .expect("actual claim settlement"),
            MutationOutcome::Applied
        );
        assert_eq!(
            self.state
                .store
                .assign_share_viewers(
                    self.import_id,
                    1,
                    vec![Assignment {
                        library_id: self.source.reference.library_id.clone(),
                        user_id: self.viewer_id,
                    }],
                    now,
                )
                .await
                .expect("actual viewer assignment"),
            MutationOutcome::Applied
        );
    }

    pub(crate) fn directory(&self) -> &std::path::Path {
        self.directory.path()
    }

    pub(crate) async fn shutdown(self) {
        self.source.shutdown().await;
        self.selected
            .shutdown()
            .await
            .expect("actual B voter shutdown");
    }
}

#[tokio::test]
async fn sharing_receiver_fixture_uses_actual_factories_login_and_genuine_source_claim() {
    // This is a bootstrap/pairing proof. The endpoint is validated metadata;
    // the separate CGNAT fixture must exercise the actual pinned TLS transport.
    let fixture = real_receiver_fixture(
        "100.127.89.2".parse().expect("fixture address"),
        SourceFixtureMode::Copy,
    )
    .await;
    let tls = plurx_core::sharing_tls::LiveNodeTls::open(
        &fixture.directory().join("tls"),
        crate::state::clock_ms() / 1000,
    )
    .expect("actual node TLS");
    let (pin, _) = tls.status().expect("actual SPKI");
    fixture
        .pair(Endpoint {
            ipv4: "100.127.89.2".parse().expect("CGNAT IPv4"),
            ipv6: None,
            ts_fqdn: "source.fixture.ts.net".into(),
            port: 32443,
            spki_sha256: pin,
        })
        .await;
    let import = fixture
        .state
        .store
        .sharing_import(fixture.import_id)
        .await
        .expect("actual import read")
        .expect("actual import");
    assert_eq!(import.summary.remote_grant_id, Some(fixture.source.grant));
    assert_eq!(import.summary.assignment_generation, 2);
    assert!(!fixture.original_login.is_empty());
    assert!(fixture
        .state
        .store
        .authorize_share_viewer(
            fixture.import_id,
            fixture.source.reference.library_id.clone(),
            fixture.viewer_id,
        )
        .await
        .expect("actual viewer authority")
        .is_some());
    // Construct the exact candidate/production merge: legacy production paths
    // must remain accepted by this disposable outer router.
    let _app = fixture_router(fixture.state.clone());
    let recipe: Value =
        serde_json::from_slice(&fixture.source.request).expect("actual Source recipe");
    let observation = receiver_claim_observation(
        &fixture,
        recipe["session"]["request_id"]
            .as_str()
            .expect("actual request UUID"),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(&observation).expect("bounded counts"),
        json!({"claims":0,"assigned":0,"sessions":0,"upstream":0,"source_bound":0,"published":0,"resolved":0,"delivery":0})
    );
    fixture.shutdown().await;
}

// Diagnostic observation only. Row absence never proves no physical owner.
async fn receiver_claim_observation(fixture: &RealReceiverFixture, request: &str) -> String {
    let client = fixture
        .selected
        .local_client()
        .expect("actual B selected voter client");
    let mut rows = client.query_consistent(
        "SELECT json_object('claims',COUNT(*),'assigned',COALESCE(SUM(owner_node_id IS NOT NULL),0),'sessions',(SELECT COUNT(*) FROM media_sessions m WHERE m.incarnation_id IN (SELECT incarnation_id FROM media_session_requests WHERE request_id=$1 AND user_id=$2)),'upstream',(SELECT COUNT(*) FROM sharing_relay_upstream b WHERE b.incarnation_id IN (SELECT incarnation_id FROM media_session_requests WHERE request_id=$1 AND user_id=$2)),'source_bound',(SELECT COUNT(*) FROM sharing_relay_upstream b WHERE b.source_session_id IS NOT NULL AND b.source_incarnation_id IS NOT NULL AND b.capability_envelope IS NOT NULL AND b.incarnation_id IN (SELECT incarnation_id FROM media_session_requests WHERE request_id=$1 AND user_id=$2)),'published',(SELECT COUNT(*) FROM media_sessions m WHERE m.publication_ready_at_ms=0 AND m.incarnation_id IN (SELECT incarnation_id FROM media_session_requests WHERE request_id=$1 AND user_id=$2)),'resolved',COALESCE(SUM(state='resolved'),0),'delivery',(SELECT COUNT(*) FROM sharing_delivery_grants d WHERE d.incarnation_id IN (SELECT incarnation_id FROM media_session_requests WHERE request_id=$1 AND user_id=$2))) AS payload FROM media_session_requests WHERE request_id=$1 AND user_id=$2",
        hiqlite::params!(request.to_owned(), fixture.viewer_id),
    ).await.expect("actual B read-only claim-stage observation");
    assert_eq!(rows.len(), 1);
    rows[0].get("payload")
}

fn fixture_router(state: crate::state::AppState) -> axum::Router {
    axum::Router::new()
        .without_v07_checks()
        .nest(
            "/api/v1",
            super::shared_receiver_ingress::candidate_router().with_state(state.clone()),
        )
        .merge(super::router(state).without_v07_checks())
}

#[tokio::test]
#[ignore = "requires an isolated Linux CGNAT namespace and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_receiver_real_pinned_source_h1_b_h1_h2_start_resources_and_confirmed_end() {
    let address: IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit disposable CGNAT namespace")
        .parse()
        .expect("fixture IP");
    assert!(plurx_core::sharing::is_tailnet_address(address));
    for h2 in [false, true] {
        tokio::time::timeout(
            std::time::Duration::from_secs(330),
            Box::pin(actual_pinned_playback(address, h2)),
        )
        .await
        .expect("bounded real playback fixture");
    }
}

async fn actual_pinned_playback(address: IpAddr, h2: bool) {
    use axum::http::StatusCode;
    use plurx_core::sharing_tls::{LiveNodeTls, SharingTlsListener};
    let fixture = real_receiver_fixture(address, SourceFixtureMode::Copy).await;
    let tls = Arc::new(
        LiveNodeTls::open(
            &fixture.directory().join("source-runtime-tls"),
            crate::state::clock_ms() / 1000,
        )
        .expect("actual Source runtime TLS"),
    );
    let (pin, _) = tls.status().expect("actual Source runtime SPKI");
    let source_listener = tokio::net::TcpListener::bind((address, 0))
        .await
        .expect("actual Source CGNAT listener");
    let endpoint = Endpoint {
        ipv4: match address {
            IpAddr::V4(ip) => ip,
            _ => panic!("IPv4 CGNAT fixture"),
        },
        ipv6: None,
        ts_fqdn: "source.fixture.ts.net".into(),
        port: source_listener.local_addr().expect("Source bind").port(),
        spki_sha256: pin,
    };
    // Actual HTTP-arrival diagnostics only, never no-admission/settlement proof.
    let source_starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let source_start_status = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_starts = source_starts.clone();
    let observed_status = source_start_status.clone();
    let source_app = super::sharing::peer_router((*fixture.source.state).clone()).layer(
        axum::middleware::from_fn(
            move |request: Request<Body>, next: axum::middleware::Next| {
                let starts = observed_starts.clone();
                let status = observed_status.clone();
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
    let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
    let source_task = tokio::spawn(crate::serve_http(
        SharingTlsListener::new(source_listener, tls),
        source_app,
        async move {
            let _ = source_stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    fixture.pair(endpoint).await;
    // The live Start remains uninstalled in production. Only this disposable
    // fixture installs the typed candidate on the real B serving stack.
    let app = fixture_router(fixture.state.clone());
    let b_listener = tokio::net::TcpListener::bind((address, 0))
        .await
        .expect("actual B CGNAT listener");
    let b_address = b_listener.local_addr().expect("B bind");
    let (b_stop, b_stopped) = tokio::sync::oneshot::channel();
    let b_task = tokio::spawn(crate::serve_http(
        b_listener,
        app,
        async move {
            let _ = b_stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let detail_path = format!(
        "/api/v1/shared/imports/{}/items/{}",
        fixture.import_id,
        fixture.source.reference.item_id.as_str(),
    );
    let (status, _, bytes) = b_request(
        b_address,
        h2,
        "GET",
        &detail_path,
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "actual authenticated details; H2={h2}"
    );
    let details: Value = serde_json::from_slice(&bytes).expect("actual B detail JSON");
    let base = details["files"][0]["file_base"]
        .as_str()
        .expect("actual signed B file alias");
    assert!(base.starts_with(&format!(
        "/api/v1/shared/imports/{}/files/",
        fixture.import_id
    )));
    let original: Value =
        serde_json::from_slice(&fixture.source.request).expect("Source complete recipe fixture");
    let session =
        serde_json::to_vec(&original["session"]).expect("complete ordinary CreateSession body");
    let summary = fixture
        .state
        .store
        .sharing_import(fixture.import_id)
        .await
        .expect("current import read")
        .expect("current import")
        .summary;
    let key = super::shared_artwork::receiver_key(&fixture.state)
        .await
        .expect("actual B locator key read")
        .expect("actual B locator key");
    let reference = key
        .verify(
            base.rsplit('/').next().expect("actual locator"),
            fixture.import_id,
            summary.lifecycle_generation,
        )
        .expect("actual advertised locator verification");
    let login_hash = plurx_core::auth::hash_token(&fixture.original_login);
    let intent = plurx_core::sharing_receiver_sessions::ReceiverSessionIntent {
        scope: plurx_core::store::sharing_catalogue::ReceiverCatalogueScope {
            import_id: summary.id,
            source_server_id: summary.source_server_id,
            catalogue_epoch: summary.catalogue_epoch,
            lifecycle_generation: summary.lifecycle_generation,
            assignment_generation: summary.assignment_generation,
            endpoint_generation: summary.endpoint_generation,
            claim_id: summary.claim_id,
            remote_grant_id: summary.remote_grant_id.expect("actual remote grant"),
            libraries: vec![reference.item.library_id.clone()],
        },
        user_id: fixture.viewer_id,
        login_hash: login_hash.clone(),
        recipe: plurx_core::sharing_receiver_sessions::RemoteSourceRecipe {
            kind: plurx_core::sharing_receiver_sessions::ReceiverProducerKind::RemoteSource,
            version: 1,
            reference: reference.item,
            lifecycle_generation: reference.lifecycle_generation,
            file_id: reference.file_id,
            file_revision: reference.revision,
            source_request_id: Uuid::new_v4(),
            parent_login_hash: login_hash,
            request_json: original["session"].to_string(),
        },
        source_position_ms: 0,
    };
    // This is only an actual read-only B metadata check, never Source admission.
    assert!(
        fixture
            .state
            .store
            .prepare_receiver_session_authority(intent)
            .await
            .expect("actual B metadata authority read")
            .is_some(),
        "current original-login full-reference B metadata authority; H2={h2}"
    );
    let (status, _, bytes) = b_request(
        b_address,
        h2,
        "POST",
        &format!("{base}/hls/sessions"),
        &fixture.original_login,
        session,
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
        "actual B Start through real Source actor; H2={h2}; code={}",
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| value.get("code").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| "no_typed_error_code".into())
    );
    let start: Value = serde_json::from_slice(&bytes).expect("complete B Start DTO");
    let session = start["session_id"].as_str().expect("actual B session ID");
    assert_eq!(
        Uuid::parse_str(session).expect("B UUID").get_version_num(),
        4
    );
    let prefix = format!("/api/v1/hls/{session}/");
    let mut playlist_path = start["playlist_url"]
        .as_str()
        .expect("actual B playlist")
        .to_owned();
    assert!(playlist_path.starts_with(&prefix));
    assert_eq!(start["control"]["url"], format!("{prefix}control"));
    let (status, headers, bytes) = b_request(
        b_address,
        h2,
        "GET",
        &playlist_path,
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "real playlist relay; H2={h2}");
    assert!(headers["content-type"]
        .to_str()
        .expect("playlist MIME")
        .contains("mpegurl"));
    let mut playlist = String::from_utf8(bytes.to_vec()).expect("actual HLS UTF-8");
    assert!(playlist.starts_with("#EXTM3U"));
    if playlist.contains("#EXT-X-STREAM-INF:") {
        let relative = playlist
            .lines()
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .expect("actual master variant");
        plurx_core::sharing_resources::SharingHlsResource::parse(relative)
            .expect("closed variant grammar");
        playlist_path = format!("{prefix}{relative}");
        let (status, _, bytes) = b_request(
            b_address,
            h2,
            "GET",
            &playlist_path,
            &fixture.original_login,
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "actual variant; H2={h2}");
        playlist = String::from_utf8(bytes.to_vec()).expect("actual variant UTF-8");
    }
    let init = playlist
        .lines()
        .find_map(|line| {
            line.strip_prefix("#EXT-X-MAP:URI=\"")
                .and_then(|rest| rest.split('"').next())
        })
        .expect("actual init map");
    let segment = playlist
        .lines()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .expect("actual video segment");
    for (resource, marker) in [(init, b"ftyp".as_slice()), (segment, b"moof".as_slice())] {
        plurx_core::sharing_resources::SharingHlsResource::parse(resource)
            .expect("closed actual media resource");
        let (status, headers, bytes) = b_request(
            b_address,
            h2,
            "GET",
            &format!("{prefix}{resource}"),
            &fixture.original_login,
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "actual media relay; H2={h2}");
        assert_eq!(headers["content-type"], "video/mp4");
        assert!(
            bytes.windows(marker.len()).any(|window| window == marker),
            "real FFmpeg media box"
        );
        assert_eq!(
            headers["content-length"]
                .to_str()
                .expect("actual length")
                .parse::<usize>()
                .expect("decimal length"),
            bytes.len()
        );
    }
    let end_path = format!("/api/v1/hls/{session}");
    for _ in 0..2 {
        let (status, _, bytes) = b_request(
            b_address,
            h2,
            "DELETE",
            &end_path,
            &fixture.original_login,
            Vec::new(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "actual physically confirmed End/retry; H2={h2}"
        );
        assert!(bytes.is_empty());
    }
    let _ = b_stop.send(());
    b_task
        .await
        .expect("actual B server joined")
        .expect("B server result");
    let _ = source_stop.send(());
    source_task
        .await
        .expect("actual Source server joined")
        .expect("Source server result");
    fixture.shutdown().await;
}

async fn b_request(
    address: std::net::SocketAddr,
    h2: bool,
    method: &str,
    path: &str,
    original_login: &str,
    body: Vec<u8>,
) -> (
    axum::http::StatusCode,
    axum::http::HeaderMap,
    axum::body::Bytes,
) {
    use http_body_util::BodyExt;
    let socket = tokio::net::TcpStream::connect(address)
        .await
        .expect("actual B socket");
    let request = Request::builder()
        .method(method)
        .uri(format!("http://{address}{path}"))
        .header("authorization", format!("Bearer {original_login}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("actual B request");
    let (response, driver) = if h2 {
        let (mut sender, driver) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .handshake::<_, Body>(hyper_util::rt::TokioIo::new(socket))
                .await
                .expect("actual B H2");
        let driver = tokio::spawn(driver);
        let response = sender
            .send_request(request)
            .await
            .expect("actual B H2 response");
        drop(sender);
        (response, driver)
    } else {
        let (mut sender, driver) =
            hyper::client::conn::http1::handshake::<_, Body>(hyper_util::rt::TokioIo::new(socket))
                .await
                .expect("actual B H1");
        let driver = tokio::spawn(driver);
        let response = sender
            .send_request(request)
            .await
            .expect("actual B H1 response");
        drop(sender);
        (response, driver)
    };
    let (parts, body) = response.into_parts();
    let bytes = http_body_util::Limited::new(body, 4 * 1024 * 1024)
        .collect()
        .await
        .expect("bounded actual B body")
        .to_bytes();
    driver.abort();
    let _ = driver.await;
    (parts.status, parts.headers, bytes)
}
