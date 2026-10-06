//! Real selected-store fixtures for B-to-Source playback qualification.
//! These helpers install only the production pre-serving factories.
use super::shared_source_playback::{
    real_source_start_fixture_with, RealSourceStartFixture, SourceFixtureMode,
};
use axum::{body::Body, http::Request, response::IntoResponse};
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
    Box::pin(build_receiver_fixture(address, mode, None))
}

fn real_receiver_fixture_at(
    address: IpAddr,
    mode: SourceFixtureMode,
    advertised_http: std::net::SocketAddr,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = RealReceiverFixture> + Send>> {
    Box::pin(build_receiver_fixture(address, mode, Some(advertised_http)))
}
async fn build_receiver_fixture(
    address: IpAddr,
    mode: SourceFixtureMode,
    advertised_http: Option<std::net::SocketAddr>,
) -> RealReceiverFixture {
    let directory = crate::test_tempdir().expect("real B directory");
    let mut config = Config::default();
    config.storage.data_dir = directory.path().join("database");
    if let Some(address) = advertised_http {
        config.server.bind = address;
        config.cluster.artwork_url = format!("http://{address}");
    }
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
            ..SharingNetworkConfig::default()
        },
    ));
    state
        .membership
        .set_ingress_custody_boot(Some(state.sharing.accepted_drivers.boot_id()));
    state
        .membership
        .publish_ingress_custody_boot()
        .await
        .expect("actual B fixture registry boot");
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

// The unchanged public B router: Start, relay, status, control and End.
fn fixture_router(state: crate::state::AppState) -> axum::Router {
    super::router(state)
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
            Box::pin(actual_pinned_playback(
                address,
                h2,
                SourceFixtureMode::Copy,
                None,
            )),
        )
        .await
        .expect("bounded real playback fixture");
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(330),
        Box::pin(actual_pinned_playback(
            address,
            false,
            SourceFixtureMode::Copy,
            Some(false),
        )),
    )
    .await
    .expect("retained TLS End refusal reaches approved replacement");
    tokio::time::timeout(
        std::time::Duration::from_secs(330),
        Box::pin(actual_pinned_playback(
            address,
            false,
            SourceFixtureMode::Copy,
            Some(true),
        )),
    )
    .await
    .expect("retained End stall leaves approved replacement useful budget");
}

#[tokio::test]
#[ignore = "requires an isolated Linux CGNAT namespace and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_receiver_real_pinned_source_encoded_and_native_lanes_through_b() {
    let address: IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit disposable CGNAT namespace")
        .parse()
        .expect("fixture IP");
    assert!(plurx_core::sharing::is_tailnet_address(address));
    for mode in [SourceFixtureMode::Encoded, SourceFixtureMode::NativeCopy] {
        for h2 in [false, true] {
            tokio::time::timeout(
                std::time::Duration::from_secs(330),
                Box::pin(actual_pinned_playback(address, h2, mode, None)),
            )
            .await
            .expect("bounded real playback fixture");
        }
    }
}

/// The current-rendition control selection a client derives from its own
/// original Start ask, the same projection the Source freezes.
fn original_selection(session: &Value) -> crate::playback_control::ClientSelection {
    use crate::playback_control as pc;
    let height = session["height"].as_i64();
    let quality = if session["quality_auto"]
        .as_bool()
        .unwrap_or(height.is_none())
    {
        pc::QualitySelection::Auto {
            height,
            candidate_id: None,
        }
    } else if session["copy"].as_bool() == Some(true) {
        pc::QualitySelection::Original
    } else {
        pc::QualitySelection::Manual {
            height: height.expect("manual fixture height"),
        }
    };
    let subtitle = match (
        session["native_subtitles"].as_bool(),
        session["subtitle"].as_i64(),
    ) {
        (Some(true), Some(track)) => pc::SubtitleSelection {
            mode: pc::SubtitleMode::Native,
            track: Some(track),
        },
        _ => pc::SubtitleSelection {
            mode: pc::SubtitleMode::Off,
            track: None,
        },
    };
    pc::ClientSelection {
        quality,
        audio_track: session["audio"].as_i64(),
        subtitle,
        audio_offset_ms: session["audio_offset_ms"].as_i64().unwrap_or(0),
        codec: pc::CodecPolicy::Auto,
        dynamic_range: pc::DynamicRangePolicy::Auto,
    }
}

async fn actual_pinned_playback(
    address: IpAddr,
    h2: bool,
    mode: SourceFixtureMode,
    endpoint_failure: Option<bool>,
) {
    use axum::http::StatusCode;
    use plurx_core::sharing_tls::{LiveNodeTls, SharingTlsListener};
    let fixture = real_receiver_fixture(address, mode).await;
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
    let stalled_end_release = tokio_util::sync::CancellationToken::new();
    let observed_stall_release = stalled_end_release.clone();
    let end_refusal = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let refused_end_bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_refusal = end_refusal.clone();
    let observed_end_bodies = refused_end_bodies.clone();
    let observed_starts = source_starts.clone();
    let observed_status = source_start_status.clone();
    let source_app = super::sharing::peer_router((*fixture.source.state).clone()).layer(
        axum::middleware::from_fn(
            move |request: Request<Body>, next: axum::middleware::Next| {
                let release = observed_stall_release.clone();
                let refusal = observed_refusal.clone();
                let end_bodies = observed_end_bodies.clone();
                let starts = observed_starts.clone();
                let status = observed_status.clone();
                async move {
                    if refusal.load(std::sync::atomic::Ordering::Relaxed)
                        && request.uri().path().ends_with("/end")
                    {
                        let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
                            .await
                            .expect("bounded immutable End ask");
                        end_bodies.lock().expect("End observations").push(bytes);
                        if endpoint_failure == Some(true) {
                            // The actual accepted request owns this stall. The
                            // fixture releases it after replacement settlement
                            // and joins its server; no detached timer/poller.
                            release.cancelled().await;
                        }
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
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
        SharingTlsListener::new(source_listener, tls.clone()),
        source_app,
        async move {
            let _ = source_stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    fixture.pair(endpoint.clone()).await;
    let replacement_end_bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let replacement = if endpoint_failure.is_some() {
        let listener = tokio::net::TcpListener::bind((address, 0))
            .await
            .expect("replacement Source listener");
        let mut approved = endpoint.clone();
        approved.port = listener.local_addr().expect("replacement bind").port();
        let replacement_tls = Arc::new(
            LiveNodeTls::open(
                &fixture.directory().join("source-replacement-tls"),
                crate::state::clock_ms() / 1000,
            )
            .expect("approved replacement runtime TLS"),
        );
        approved.spki_sha256 = replacement_tls.status().expect("replacement SPKI").0;
        assert_ne!(approved.spki_sha256, endpoint.spki_sha256);
        let observed = replacement_end_bodies.clone();
        let app = super::sharing::peer_router((*fixture.source.state).clone()).layer(
            axum::middleware::from_fn(
                move |request: Request<Body>, next: axum::middleware::Next| {
                    let observed = observed.clone();
                    async move {
                        if request.uri().path().ends_with("/end") {
                            let (parts, body) = request.into_parts();
                            let bytes = axum::body::to_bytes(body, 64 * 1024)
                                .await
                                .expect("replacement immutable End ask");
                            observed
                                .lock()
                                .expect("replacement End observations")
                                .push(bytes.clone());
                            next.run(Request::from_parts(parts, Body::from(bytes)))
                                .await
                        } else {
                            next.run(request).await
                        }
                    }
                },
            ),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(crate::serve_http(
            SharingTlsListener::new(listener, replacement_tls),
            app,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        Some((approved, stop, task))
    } else {
        None
    };
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
    assert_eq!(
        details["delivery_status"], "available",
        "a fresh detail with a signed locator reports launch capability; H2={h2}"
    );
    let base = details["files"][0]["file_base"]
        .as_str()
        .expect("actual signed B file alias");
    assert!(base.starts_with(&format!(
        "/api/v1/shared/imports/{}/files/",
        fixture.import_id
    )));
    // Pre-session assets through B before any session exists: the actual
    // embedded caption as WebVTT (native modes carry two SubRip tracks), a
    // chapter the lavfi source does not have, and the typed progressive
    // closure. None of these creates a session on either side.
    if matches!(
        mode,
        SourceFixtureMode::NativeCopy | SourceFixtureMode::NativeEncoded
    ) {
        let (status, headers, bytes) = b_request(
            b_address,
            h2,
            "GET",
            &format!("{base}/subs/0.vtt"),
            &fixture.original_login,
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "actual pre-session WebVTT; H2={h2}");
        assert_eq!(headers["content-type"], "text/vtt; charset=utf-8");
        assert_eq!(headers["cache-control"], "no-store");
        assert!(String::from_utf8_lossy(&bytes).contains("Actual HTTP Source caption"));
    }
    let (status, _, bytes) = b_request(
        b_address,
        h2,
        "GET",
        &format!("{base}/chapters/0/thumb"),
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no chapters in the source; H2={h2}"
    );
    assert!(String::from_utf8_lossy(&bytes).contains("sharing_asset_not_found"));
    let (status, _, _) = b_request(
        b_address,
        h2,
        "GET",
        &format!("{base}/stream.mp4"),
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "typed progressive closure; H2={h2}"
    );
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
    // The same MIME types the Local HLS routes answer for fMP4 objects.
    for (resource, marker, mime) in [
        (init, b"ftyp".as_slice(), "video/mp4"),
        (segment, b"moof".as_slice(), "video/iso.segment"),
    ] {
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
        assert_eq!(headers["content-type"], mime);
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
    // Shared status and current-rendition control through B's own tuple.
    let generation = start["control"]["generation"]
        .as_str()
        .expect("B control generation")
        .to_owned();
    let epoch = start["control"]["control_epoch"]
        .as_u64()
        .expect("B control epoch");
    let (status, headers, bytes) = b_request(
        b_address,
        h2,
        "GET",
        &format!("{prefix}status"),
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "actual Shared status; H2={h2}");
    assert_eq!(headers["cache-control"], "no-store");
    let observed: Value = serde_json::from_slice(&bytes).expect("Shared status JSON");
    assert_eq!(observed["subject"], "shared");
    assert_eq!(observed["session_id"], session);
    assert_eq!(observed["incarnation_id"], generation.as_str());
    assert_eq!(observed["control_epoch"], epoch);
    assert_eq!(
        observed["reference"]["item"]["import_id"],
        fixture.import_id.to_string()
    );
    assert_eq!(
        observed["reference"]["file_id"],
        fixture.source.reference.file_id.as_str()
    );
    for forbidden in ["id", "file_id", "producer_failed"] {
        assert!(observed["status"].get(forbidden).is_none(), "{forbidden}");
    }
    assert!(
        serde_json::from_value::<crate::sharing_client::SharedVodStatus>(
            observed["status"].clone()
        )
        .expect("closed Shared metrics")
        .is_valid()
    );
    let control = |generation: &str, epoch: u64, sequence: u64| {
        use crate::playback_control as pc;
        serde_json::to_vec(&pc::ControlRequestV1 {
            intent: None,
            protocol: pc::PROTOCOL_V1.to_owned(),
            generation: generation.to_owned(),
            control_epoch: epoch,
            client_instance_id: "6f1c2d1e-7f9a-4b8e-9d3c-2a1b0c9d8e7f".to_owned(),
            sequence,
            demand: pc::PlaybackDemand::Active,
            position_ms: 0,
            buffered_from_ms: Some(0),
            buffered_through_ms: 1_000,
            playback_rate: 1.0,
            render_state: pc::RenderState::Rendering,
            seek_target_ms: None,
            observed_download_bps: None,
            selection: original_selection(&original["session"]),
            capabilities: Some(pc::DynamicCapabilities {
                presentation_target: None,
                decoder_caps: None,
                platform: pc::ClientPlatform::Web,
                max_height: 1080,
                codecs: vec![pc::CodecPolicy::H264],
                dynamic_ranges: vec![pc::DynamicRangePolicy::Sdr],
                dual_player_preparation: false,
            }),
            observation: None,
            acknowledgement: None,
            supported_actions: Some(vec![
                "hold".to_owned(),
                "retry_resource".to_owned(),
                pc::PREPARE_REPLACEMENT_ACTION.to_owned(),
            ]),
        })
        .expect("ordinary v1 control")
    };
    let control_path = format!("{prefix}control");
    for attempt in 0..2 {
        let (status, _, bytes) = b_request(
            b_address,
            h2,
            "POST",
            &control_path,
            &fixture.original_login,
            control(&generation, epoch, 1),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "accepted control or exact replay {attempt}; H2={h2}: {}",
            String::from_utf8_lossy(&bytes)
        );
        let accepted: crate::playback_control::ControlResponseV1 =
            serde_json::from_slice(&bytes).expect("ordinary v1 control response");
        assert_eq!(accepted.accepted_sequence, 1);
        assert_eq!(accepted.generation, generation);
        assert_eq!(accepted.control_epoch, epoch);
        assert_eq!(accepted.delivery.owner_epoch, epoch);
        assert_eq!(
            accepted.delivery.owner_node_hash,
            crate::playback_control::node_hash(&fixture.state.node_id)
        );
        assert!(!String::from_utf8_lossy(&bytes).contains("/api/v1/hls/"));
    }
    for (body, code) in [
        (
            control(&Uuid::new_v4().to_string(), epoch, 2),
            "stale_control",
        ),
        (control(&generation, epoch + 1, 2), "owner_changed"),
    ] {
        let (status, _, bytes) = b_request(
            b_address,
            h2,
            "POST",
            &control_path,
            &fixture.original_login,
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{code}; H2={h2}");
        let refusal: Value = serde_json::from_slice(&bytes).expect("control error body");
        assert_eq!(refusal["code"], code);
        assert_eq!(refusal["generation"], generation.as_str());
    }
    if let Some((approved, _, _)) = &replacement {
        let current = fixture
            .source
            .state
            .store
            .sharing_endpoint_manifest()
            .await
            .expect("manifest snapshot");
        assert_eq!(
            fixture
                .source
                .state
                .store
                .set_sharing_endpoint_manifest(
                    current.map_or(0, |manifest| manifest.revision),
                    vec![endpoint.clone(), approved.clone()],
                )
                .await
                .expect("Source approves replacement"),
            plurx_core::sharing::MutationOutcome::Applied
        );
        let import = fixture
            .state
            .store
            .sharing_import(fixture.import_id)
            .await
            .expect("import")
            .expect("paired import");
        fixture
            .state
            .sharing
            .refresh_active_import(&fixture.state, import)
            .await
            .expect("authenticated endpoint refresh");
        end_refusal.store(true, std::sync::atomic::Ordering::Relaxed);
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
    stalled_end_release.cancel();
    if let Some((_, stop, task)) = replacement {
        {
            let old = refused_end_bodies
                .lock()
                .expect("retained End observations");
            let new = replacement_end_bodies
                .lock()
                .expect("replacement End observations");
            assert_eq!(old.len(), 1, "retained TLS succeeded but End route refused");
            assert_eq!(
                *old, *new,
                "fallback preserves immutable End identity bytes"
            );
        }
        let _ = stop.send(());
        task.await
            .expect("replacement joined")
            .expect("replacement server result");
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

#[tokio::test]
#[ignore = "requires an isolated Linux CGNAT namespace and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_receiver_real_pinned_quality_reopen_preserves_position_and_releases_slot() {
    let address: IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit disposable CGNAT namespace")
        .parse()
        .expect("fixture IP");
    assert!(plurx_core::sharing::is_tailnet_address(address));
    tokio::time::timeout(
        std::time::Duration::from_secs(330),
        Box::pin(actual_pinned_reopen(address)),
    )
    .await
    .expect("bounded real reopen fixture");
}

/// P0 directed change through the real pinned B: a changed selection is
/// accepted with `preparation: none`, the client reopens with a fresh Start at
/// its position, B publishes that successor before it supersedes the
/// predecessor, and the predecessor retires through its single owner with a
/// confirmed Source End that frees its Source slot.
async fn actual_pinned_reopen(address: IpAddr) {
    use axum::http::StatusCode;
    let pair = PinnedPair::start(address).await;
    let (fixture, b_address, login, base, session) = (
        &pair.fixture,
        pair.b_address,
        pair.login.clone(),
        pair.base.clone(),
        pair.session.clone(),
    );
    let (status, _, bytes) = b_request(
        b_address,
        false,
        "POST",
        &format!("{base}/hls/sessions"),
        &login,
        serde_json::to_vec(&session).expect("CreateSession"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "first B Start: {}",
        String::from_utf8_lossy(&bytes)
    );
    let first: Value = serde_json::from_slice(&bytes).expect("first Start");
    let first_id = first["session_id"].as_str().expect("B session").to_owned();
    let first_generation = first["control"]["generation"]
        .as_str()
        .expect("B generation")
        .to_owned();
    let first_epoch = first["control"]["control_epoch"].as_u64().expect("B epoch");
    assert_eq!(fixture.source.active_source_sessions().await, 1);
    let control = |generation: &str,
                   epoch: u64,
                   sequence: u64,
                   selection: crate::playback_control::ClientSelection| {
        fixture_control(generation, epoch, sequence, selection, false, None)
    };
    let first_control = format!("/api/v1/hls/{first_id}/control");
    let current = original_selection(&session);
    let mut directed = current.clone();
    directed.quality = crate::playback_control::QualitySelection::Manual { height: 144 };
    for (sequence, selection) in [(1, current.clone()), (2, directed.clone())] {
        if sequence == 2 {
            // The per-session control budget between two new sequences.
            tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        }
        let (status, _, bytes) = b_request(
            b_address,
            false,
            "POST",
            &first_control,
            &login,
            control(&first_generation, first_epoch, sequence, selection),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "control {sequence}: {}",
            String::from_utf8_lossy(&bytes)
        );
        let answer: crate::playback_control::ControlResponseV1 =
            serde_json::from_slice(&bytes).expect("control answer");
        assert_eq!(answer.accepted_sequence, sequence);
        assert_eq!(
            answer.delivery.preparation.as_deref(),
            Some("none"),
            "a shared session declines every preparation; the client reopens"
        );
        assert_eq!(answer.action, crate::playback_control::ControlAction::None);
        assert_eq!(
            answer.effective_selection.height, 180,
            "the current rendition is never replaced in place"
        );
    }
    // The client's one reopen: a fresh Start of the same file with the new
    // selection, at the position sampled when the decline arrived, and no
    // lineage fields.
    let mut reopen = session.clone();
    reopen["request_id"] = Uuid::new_v4().to_string().into();
    reopen["start"] = serde_json::json!(1.0);
    reopen["copy"] = serde_json::json!(false);
    reopen["height"] = serde_json::json!(144);
    reopen["quality_auto"] = serde_json::json!(false);
    let (status, _, bytes) = b_request(
        b_address,
        false,
        "POST",
        &format!("{base}/hls/sessions"),
        &login,
        serde_json::to_vec(&reopen).expect("reopen CreateSession"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "reopen B Start beside its predecessor: {}",
        String::from_utf8_lossy(&bytes)
    );
    let second: Value = serde_json::from_slice(&bytes).expect("reopen Start");
    let second_id = second["session_id"].as_str().expect("B session").to_owned();
    assert_ne!(second_id, first_id);
    assert_eq!(
        second["vod"], true,
        "a whole-title timeline the client seeks into"
    );
    assert_eq!(second["media_origin_ms"], 0);
    let second_generation = second["control"]["generation"]
        .as_str()
        .expect("B generation")
        .to_owned();
    let successor_route = fixture
        .state
        .store
        .media_session_route_by_incarnation(&second_generation)
        .await
        .expect("successor route read")
        .expect("successor route");
    let retained: Value =
        serde_json::from_str(&successor_route.recipe_json).expect("successor recipe");
    let request: Value = serde_json::from_str(
        retained["request_json"]
            .as_str()
            .expect("retained complete request"),
    )
    .expect("retained request JSON");
    assert_eq!(
        request["start"], 1.0,
        "the Source plans from the sampled position"
    );
    assert_eq!(request["playback_id"], session["playback_id"]);
    assert_ne!(
        successor_route.playback_id, session["playback_id"],
        "each B session is its own Store playback"
    );
    // Published first, then superseded: the predecessor retires through its
    // single owner (no DELETE was sent) and its Source slot is released.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let predecessor = fixture
            .state
            .store
            .media_session_route_by_incarnation(&first_generation)
            .await
            .expect("predecessor route read");
        if predecessor
            .as_ref()
            .is_some_and(|route| route.state == "ended")
            && fixture.source.active_source_sessions().await == 1
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the superseded predecessor retires and frees its Source slot"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    // Its End is the confirmed physical one: an exact DELETE replays 204.
    let (status, _, _) = b_request(
        b_address,
        false,
        "DELETE",
        &format!("/api/v1/hls/{first_id}"),
        &login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "confirmed predecessor End");
    // The successor serves and controls under its own tuple.
    let (status, _, _) = b_request(
        b_address,
        false,
        "GET",
        second["playlist_url"].as_str().expect("successor playlist"),
        &login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "successor playlist relay");
    let second_epoch = second["control"]["control_epoch"]
        .as_u64()
        .expect("B epoch");
    let (status, _, bytes) = b_request(
        b_address,
        false,
        "POST",
        &format!("/api/v1/hls/{second_id}/control"),
        &login,
        control(&second_generation, second_epoch, 1, directed),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "successor control: {}",
        String::from_utf8_lossy(&bytes)
    );
    let answer: crate::playback_control::ControlResponseV1 =
        serde_json::from_slice(&bytes).expect("successor control answer");
    assert_eq!(answer.effective_selection.height, 144);
    let (status, _, _) = b_request(
        b_address,
        false,
        "DELETE",
        &format!("/api/v1/hls/{second_id}"),
        &login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "confirmed successor End");
    assert_eq!(fixture.source.active_source_sessions().await, 0);
    pair.finish().await;
}

/// One real pinned Source and B pair on the disposable CGNAT address, with
/// the viewer's signed B file alias and the Source's create body. Shared by
/// the directed-change fixtures.
struct PinnedPair {
    fixture: RealReceiverFixture,
    b_address: std::net::SocketAddr,
    login: String,
    base: String,
    session: Value,
    b_stop: tokio::sync::oneshot::Sender<()>,
    b_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    source_stop: tokio::sync::oneshot::Sender<()>,
    source_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl PinnedPair {
    async fn start(address: IpAddr) -> Self {
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
        let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
        let source_task = tokio::spawn(crate::serve_http(
            SharingTlsListener::new(source_listener, tls),
            super::sharing::peer_router((*fixture.source.state).clone()),
            async move {
                let _ = source_stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        fixture.pair(endpoint).await;
        let b_listener = tokio::net::TcpListener::bind((address, 0))
            .await
            .expect("actual B CGNAT listener");
        let b_address = b_listener.local_addr().expect("B bind");
        let (b_stop, b_stopped) = tokio::sync::oneshot::channel();
        let b_task = tokio::spawn(crate::serve_http(
            b_listener,
            fixture_router(fixture.state.clone()),
            async move {
                let _ = b_stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let login = fixture.original_login.clone();
        let (status, _, bytes) = b_request(
            b_address,
            false,
            "GET",
            &format!(
                "/api/v1/shared/imports/{}/items/{}",
                fixture.import_id,
                fixture.source.reference.item_id.as_str(),
            ),
            &login,
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "actual details");
        let details: Value = serde_json::from_slice(&bytes).expect("details");
        let base = details["files"][0]["file_base"]
            .as_str()
            .expect("signed B file alias")
            .to_owned();
        let original: Value = serde_json::from_slice(&fixture.source.request).expect("recipe");
        let session = original["session"].clone();
        Self {
            fixture,
            b_address,
            login,
            base,
            session,
            b_stop,
            b_task,
            source_stop,
            source_task,
        }
    }

    async fn finish(self) {
        let _ = self.b_stop.send(());
        self.b_task
            .await
            .expect("actual B server joined")
            .expect("B server result");
        let _ = self.source_stop.send(());
        self.source_task
            .await
            .expect("actual Source server joined")
            .expect("Source server result");
        self.fixture.shutdown().await;
    }
}

/// An ordinary web v1 control exchange. `shared` adds the Shared successor
/// promise beside `prepare_replacement`; without it the client keeps the P0
/// reopen.
fn fixture_control(
    generation: &str,
    epoch: u64,
    sequence: u64,
    selection: crate::playback_control::ClientSelection,
    shared: bool,
    acknowledgement: Option<crate::playback_control::ActionAcknowledgement>,
) -> Vec<u8> {
    use crate::playback_control as pc;
    let mut supported_actions = vec![
        "hold".to_owned(),
        "retry_resource".to_owned(),
        pc::PREPARE_REPLACEMENT_ACTION.to_owned(),
    ];
    if shared {
        supported_actions.push(pc::SHARED_PREPARE_REPLACEMENT_ACTION.to_owned());
    }
    serde_json::to_vec(&pc::ControlRequestV1 {
        intent: None,
        protocol: pc::PROTOCOL_V1.to_owned(),
        generation: generation.to_owned(),
        control_epoch: epoch,
        client_instance_id: "6f1c2d1e-7f9a-4b8e-9d3c-2a1b0c9d8e7f".to_owned(),
        sequence,
        demand: pc::PlaybackDemand::Active,
        position_ms: 1_000,
        buffered_from_ms: Some(0),
        buffered_through_ms: 1_500,
        playback_rate: 1.0,
        render_state: pc::RenderState::Rendering,
        seek_target_ms: None,
        observed_download_bps: None,
        selection,
        capabilities: Some(pc::DynamicCapabilities {
            presentation_target: None,
            decoder_caps: None,
            platform: pc::ClientPlatform::Web,
            max_height: 1080,
            codecs: vec![pc::CodecPolicy::H264],
            dynamic_ranges: vec![pc::DynamicRangePolicy::Sdr],
            dual_player_preparation: true,
        }),
        observation: None,
        acknowledgement,
        supported_actions: Some(supported_actions),
    })
    .expect("ordinary v1 control")
}

#[tokio::test]
#[ignore = "requires an isolated Linux CGNAT namespace and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_receiver_real_pinned_prepared_handoff_commit_and_abort() {
    let address: IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit disposable CGNAT namespace")
        .parse()
        .expect("fixture IP");
    assert!(plurx_core::sharing::is_tailnet_address(address));
    for h2 in [false, true] {
        tokio::time::timeout(
            std::time::Duration::from_secs(330),
            Box::pin(actual_pinned_prepared_handoff(address, h2)),
        )
        .await
        .expect("bounded real prepared handoff fixture");
    }
}

/// The exchanges of one B control channel at a fixed tuple, spaced by the
/// per-session control budget between new sequences.
struct FixtureChannel<'a> {
    pair: &'a PinnedPair,
    h2: bool,
    session: String,
    generation: String,
    epoch: u64,
    sequence: u64,
}

impl FixtureChannel<'_> {
    fn of<'p>(pair: &'p PinnedPair, h2: bool, start: &Value) -> FixtureChannel<'p> {
        FixtureChannel {
            pair,
            h2,
            session: start["session_id"].as_str().expect("B session").to_owned(),
            generation: start["control"]["generation"]
                .as_str()
                .expect("B generation")
                .to_owned(),
            epoch: start["control"]["control_epoch"].as_u64().expect("B epoch"),
            sequence: 0,
        }
    }
    async fn raw(&self, body: Vec<u8>) -> (axum::http::StatusCode, axum::body::Bytes) {
        let (status, _, bytes) = b_request(
            self.pair.b_address,
            self.h2,
            "POST",
            &format!("/api/v1/hls/{}/control", self.session),
            &self.pair.login,
            body,
        )
        .await;
        (status, bytes)
    }
    /// The next sequence: the body sent, its status and its answer bytes.
    async fn next(
        &mut self,
        selection: crate::playback_control::ClientSelection,
        acknowledgement: Option<crate::playback_control::ActionAcknowledgement>,
    ) -> (Vec<u8>, axum::http::StatusCode, axum::body::Bytes) {
        if self.sequence > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        }
        self.sequence += 1;
        let body = fixture_control(
            &self.generation,
            self.epoch,
            self.sequence,
            selection,
            true,
            acknowledgement,
        );
        let (status, bytes) = self.raw(body.clone()).await;
        (body, status, bytes)
    }
    /// Exchange the ask until B offers its successor; every answer before it
    /// is `staging`.
    async fn offered(
        &mut self,
        selection: &crate::playback_control::ClientSelection,
    ) -> crate::playback_control::ControlAction {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            let (_, status, bytes) = self.next(selection.clone(), None).await;
            assert_eq!(
                status,
                axum::http::StatusCode::OK,
                "control {}: {}",
                self.sequence,
                String::from_utf8_lossy(&bytes)
            );
            let answer: crate::playback_control::ControlResponseV1 =
                serde_json::from_slice(&bytes).expect("control answer");
            match answer.delivery.preparation.as_deref() {
                Some("offered") => return answer.action,
                Some("staging") => {}
                other => panic!("B stages its own successor, never {other:?}"),
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the successor publishes"
            );
        }
    }
}

fn committed(
    action_id: &str,
    origin: i64,
) -> Option<crate::playback_control::ActionAcknowledgement> {
    Some(crate::playback_control::ActionAcknowledgement {
        action_id: action_id.to_owned(),
        state: crate::playback_control::AcknowledgementState::Committed,
        buffered_through_ms: None,
        committed_media_origin_ms: Some(origin),
        first_frame_unix_ms: Some(crate::state::clock_ms()),
    })
}

async fn wait_source_sessions(pair: &PinnedPair, expected: i64, what: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    while pair.fixture.source.active_source_sessions().await != expected {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// P2 through the real pinned B over H1 or H2: B stages its own successor
/// (a second Source session counted on both sides), offers a `prepare`
/// naming only B URLs once it is published, serves its playlist before
/// commit, settles a commit exactly (replayed byte-identical, predecessor
/// retired with a confirmed Source End), and an abort frees the successor's
/// Source slot and refuses a late commit.
async fn actual_pinned_prepared_handoff(address: IpAddr, h2: bool) {
    use crate::playback_control::{ControlAction, QualitySelection};
    use axum::http::StatusCode;
    let pair = PinnedPair::start(address).await;
    let (status, _, bytes) = b_request(
        pair.b_address,
        h2,
        "POST",
        &format!("{}/hls/sessions", pair.base),
        &pair.login,
        serde_json::to_vec(&pair.session).expect("CreateSession"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "first B Start: {}",
        String::from_utf8_lossy(&bytes)
    );
    let first: Value = serde_json::from_slice(&bytes).expect("first Start");
    let mut watched = FixtureChannel::of(&pair, h2, &first);
    let current = original_selection(&pair.session);
    let (_, status, bytes) = watched.next(current.clone(), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(pair.fixture.source.active_source_sessions().await, 1);

    // Commit.
    let mut directed = current.clone();
    directed.quality = QualitySelection::Manual { height: 144 };
    let ControlAction::Prepare {
        action_id,
        session_id,
        playlist_url,
        control,
        media_origin_ms,
        effective_selection,
    } = watched.offered(&directed).await
    else {
        panic!("an offered preparation carries prepare");
    };
    assert_ne!(session_id, watched.session);
    assert_eq!(playlist_url, format!("/api/v1/hls/{session_id}/index.m3u8"));
    let control = control.expect("successor control bootstrap");
    assert_eq!(control.url, format!("/api/v1/hls/{session_id}/control"));
    assert_eq!(effective_selection.height, 144);
    assert_eq!(
        pair.fixture.source.active_source_sessions().await,
        2,
        "the successor is a real second Source session"
    );
    // Served before commit.
    let (status, _, playlist) = b_request(
        pair.b_address,
        h2,
        "GET",
        &playlist_url,
        &pair.login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "successor playlist before commit");
    assert!(!String::from_utf8_lossy(&playlist).contains("sharing/v1"));
    let (commit, status, answer) = watched
        .next(directed.clone(), committed(&action_id, media_origin_ms))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&answer)
    );
    // A lost answer replays byte-identical and writes nothing.
    let (status, replay) = watched.raw(commit.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, answer, "exact replay");
    wait_source_sessions(&pair, 1, "the committed predecessor frees its Source slot").await;
    let (status, replay) = watched.raw(commit).await;
    assert_eq!(
        (status, replay),
        (StatusCode::OK, answer),
        "exact replay after the predecessor retired"
    );
    let (status, _, _) = b_request(
        pair.b_address,
        h2,
        "DELETE",
        &format!("/api/v1/hls/{}", watched.session),
        &pair.login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "confirmed predecessor End");

    // Abort, on the committed successor's own channel.
    let mut now_watched = FixtureChannel {
        pair: &pair,
        h2,
        session: session_id.clone(),
        generation: control.generation.clone(),
        epoch: control.control_epoch,
        sequence: 0,
    };
    let (_, status, bytes) = now_watched.next(directed.clone(), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let answer: crate::playback_control::ControlResponseV1 =
        serde_json::from_slice(&bytes).expect("successor answer");
    assert_eq!(answer.effective_selection.height, 144);
    let ControlAction::Prepare {
        action_id: aborted, ..
    } = now_watched.offered(&current).await
    else {
        panic!("a second change is offered too");
    };
    wait_source_sessions(&pair, 2, "the second successor holds a Source slot").await;
    let abort = Some(crate::playback_control::ActionAcknowledgement {
        action_id: aborted.clone(),
        state: crate::playback_control::AcknowledgementState::Aborted,
        buffered_through_ms: None,
        committed_media_origin_ms: None,
        first_frame_unix_ms: None,
    });
    let (_, status, bytes) = now_watched.next(current.clone(), abort).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    wait_source_sessions(&pair, 1, "the aborted successor frees its Source slot").await;
    let (_, status, _) = now_watched
        .next(current.clone(), committed(&aborted, 0))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "a late commit is stale");
    let (status, _, _) = b_request(
        pair.b_address,
        h2,
        "DELETE",
        &format!("/api/v1/hls/{session_id}"),
        &pair.login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "confirmed successor End");
    assert_eq!(pair.fixture.source.active_source_sessions().await, 0);
    pair.finish().await;
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

#[tokio::test]
#[ignore = "requires an isolated Linux CGNAT namespace and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_receiver_real_pinned_source_direct_range_head_through_b() {
    let address: IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit disposable CGNAT namespace")
        .parse()
        .expect("fixture IP");
    assert!(plurx_core::sharing::is_tailnet_address(address));
    for h2 in [false, true] {
        tokio::time::timeout(
            std::time::Duration::from_secs(330),
            Box::pin(actual_pinned_direct(address, h2)),
        )
        .await
        .expect("bounded real direct fixture");
    }
}

/// Direct play through the real pinned B: one authenticated Start, then GET,
/// HEAD, Range, 416 and If-Range relayed with Local's exact answers, all on
/// one Source claim; foreign and missing bindings refused; logout ends an
/// open body.
async fn actual_pinned_direct(address: IpAddr, h2: bool) {
    use axum::http::StatusCode;
    use plurx_core::sharing_tls::{LiveNodeTls, SharingTlsListener};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let fixture = real_receiver_fixture(address, SourceFixtureMode::Direct).await;
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
    let source_starts = Arc::new(AtomicUsize::new(0));
    let observed_starts = source_starts.clone();
    // Armed only for the logout phase: parks the Source's next physical read.
    let gate = Arc::new(super::shared_source_playback::SourceReadJobGate::default());
    let armed = Arc::new(AtomicBool::new(false));
    let (layer_gate, layer_armed) = (gate.clone(), armed.clone());
    let source_app = super::sharing::peer_router((*fixture.source.state).clone()).layer(
        axum::middleware::from_fn(
            move |mut request: Request<Body>, next: axum::middleware::Next| {
                let starts = observed_starts.clone();
                let gate = layer_gate.clone();
                let armed = layer_armed.clone();
                async move {
                    if request.method() == axum::http::Method::POST
                        && request.uri().path().ends_with("/sessions")
                    {
                        starts.fetch_add(1, Ordering::Relaxed);
                    }
                    if armed.load(Ordering::SeqCst) {
                        request.extensions_mut().insert(gate);
                    }
                    next.run(request).await
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
    let b_listener = tokio::net::TcpListener::bind((address, 0))
        .await
        .expect("actual B CGNAT listener");
    let b_address = b_listener.local_addr().expect("B bind");
    let (b_stop, b_stopped) = tokio::sync::oneshot::channel();
    let b_task = tokio::spawn(crate::serve_http(
        b_listener,
        fixture_router(fixture.state.clone()),
        async move {
            let _ = b_stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let (status, _, bytes) = b_request(
        b_address,
        h2,
        "GET",
        &format!(
            "/api/v1/shared/imports/{}/items/{}",
            fixture.import_id,
            fixture.source.reference.item_id.as_str(),
        ),
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "actual details; H2={h2}");
    let details: Value = serde_json::from_slice(&bytes).expect("details");
    let base = details["files"][0]["file_base"]
        .as_str()
        .expect("signed B file alias")
        .to_owned();
    let original: Value = serde_json::from_slice(&fixture.source.request).expect("recipe");
    let (status, _, bytes) = b_request(
        b_address,
        h2,
        "POST",
        &format!("{base}/playback"),
        &fixture.original_login,
        serde_json::to_vec(&original["session"]).expect("CreateSession"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "direct Start; H2={h2}: {}",
        String::from_utf8_lossy(&bytes)
    );
    let start: Value = serde_json::from_slice(&bytes).expect("direct reply");
    assert_eq!(start["presentation"], "direct");
    assert_eq!(start["mime"], "video/mp4");
    let session = start["session_id"].as_str().expect("B session").to_owned();
    let url = start["url"].as_str().expect("direct URL").to_owned();
    assert_eq!(url, format!("{base}/direct?session={session}"));
    assert!(!String::from_utf8_lossy(&bytes).contains("sharing/v1"));
    let snapshot = fixture
        .source
        .state
        .store
        .playback_planning_snapshot(1, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("Source planning read")
        .expect("Source file");
    let len = snapshot.file.size as u64;
    assert_eq!(start["length"].as_u64(), Some(len));
    for (method, range, if_range) in [
        ("GET", None, false),
        ("HEAD", None, false),
        ("GET", Some("bytes=2-40".to_owned()), false),
        ("GET", Some("bytes=-7".to_owned()), false),
        ("GET", Some(format!("bytes={len}-")), false),
        ("HEAD", Some("bytes=2-40".to_owned()), false),
        ("GET", Some("bytes=2-40".to_owned()), true),
        ("GET", Some("bytes=2-40".to_owned()), false),
    ] {
        let mut headers = vec![];
        let mut local_headers = axum::http::HeaderMap::new();
        if let Some(range) = &range {
            headers.push(("range", range.clone()));
            local_headers.insert("range", range.parse().expect("range"));
        }
        if if_range {
            headers.push(("if-range", "\"anything\"".to_owned()));
            local_headers.insert("if-range", "\"anything\"".parse().expect("if-range"));
        }
        // Native players send no account header: the B session binds.
        let (status, response_headers, bytes) =
            b_raw_request(b_address, h2, method, &url, &headers, Vec::new()).await;
        let local = crate::http::stream::serve_file_range(
            &snapshot.file.path,
            &local_headers,
            &method.parse().expect("method"),
            Some(len),
        )
        .await
        .expect("Local answer");
        let label = format!("{method} {range:?} if_range={if_range} H2={h2}");
        assert_eq!(status, local.status(), "{label}");
        for name in [
            "content-type",
            "accept-ranges",
            "content-range",
            "content-length",
        ] {
            assert_eq!(
                response_headers.get(name),
                local.headers().get(name),
                "{label} {name}"
            );
        }
        assert_eq!(response_headers["cache-control"], "no-store");
        let local = axum::body::to_bytes(local.into_body(), 4 * 1024 * 1024)
            .await
            .expect("Local bytes");
        assert_eq!(bytes, local, "{label}");
    }
    // Repeated byte requests never create another Source producer.
    assert_eq!(source_starts.load(Ordering::Relaxed), 1, "H2={h2}");
    let foreign = format!("{base}/direct?session={}", Uuid::new_v4());
    let (status, _, _) = b_raw_request(b_address, h2, "GET", &foreign, &[], Vec::new()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = b_raw_request(
        b_address,
        h2,
        "HEAD",
        &format!("{base}/direct"),
        &[("range", "bytes=0-1".into())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = b_raw_request(
        b_address,
        h2,
        "GET",
        &format!("/api/v1/hls/{session}/index.m3u8"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(source_starts.load(Ordering::Relaxed), 1);
    // Logout ends an open body: park the Source read, then revoke the login.
    armed.store(true, Ordering::SeqCst);
    let body_url = url.clone();
    let open = tokio::spawn(async move {
        b_raw_request(b_address, h2, "GET", &body_url, &[], Vec::new()).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !gate.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("open body reached the parked Source read");
    let (status, _, _) = b_request(
        b_address,
        h2,
        "POST",
        "/api/v1/auth/logout",
        &fixture.original_login,
        Vec::new(),
    )
    .await;
    assert!(status.is_success(), "logout; H2={h2}");
    let ended = tokio::time::timeout(std::time::Duration::from_secs(10), open).await;
    gate.release();
    let ended = ended
        .expect("logout ended the open body")
        .expect("reader task");
    assert!(
        ended.0 != StatusCode::OK || ended.2.len() as u64 != len,
        "a revoked login never completes the body; H2={h2}"
    );
    let (status, _, _) = b_raw_request(b_address, h2, "GET", &url, &[], Vec::new()).await;
    assert_ne!(status, StatusCode::OK);
    let _ = b_stop.send(());
    b_task.await.expect("B joined").expect("B result");
    let _ = source_stop.send(());
    source_task
        .await
        .expect("Source joined")
        .expect("Source result");
    fixture.shutdown().await;
}

/// A B request with exactly the given headers and no account credential.
/// A body error after the head is reported as an empty body, so a revoked
/// relay is observable as an incomplete answer.
async fn b_raw_request(
    address: std::net::SocketAddr,
    h2: bool,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
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
    let mut request = Request::builder()
        .method(method)
        .uri(format!("http://{address}{path}"));
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let request = request.body(Body::from(body)).expect("actual B request");
    let (response, driver) = if h2 {
        let (mut sender, driver) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .handshake::<_, Body>(hyper_util::rt::TokioIo::new(socket))
                .await
                .expect("actual B H2");
        let driver = tokio::spawn(driver);
        let response = sender.send_request(request).await.expect("B H2 response");
        drop(sender);
        (response, driver)
    } else {
        let (mut sender, driver) =
            hyper::client::conn::http1::handshake::<_, Body>(hyper_util::rt::TokioIo::new(socket))
                .await
                .expect("actual B H1");
        let driver = tokio::spawn(driver);
        let response = sender.send_request(request).await.expect("B H1 response");
        drop(sender);
        (response, driver)
    };
    let (parts, body) = response.into_parts();
    let bytes = http_body_util::Limited::new(body, 4 * 1024 * 1024)
        .collect()
        .await
        .map(|collected| collected.to_bytes())
        .unwrap_or_default();
    driver.abort();
    let _ = driver.await;
    (parts.status, parts.headers, bytes)
}

#[path = "shared_receiver_forwarding_fixture.rs"]
mod forwarding_fixture;
