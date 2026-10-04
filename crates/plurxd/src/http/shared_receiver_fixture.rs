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
    fixture.shutdown().await;
}
