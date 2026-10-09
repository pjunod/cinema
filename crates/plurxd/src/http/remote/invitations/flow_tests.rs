//! Actual home/broker handlers over trusted loopback TLS; provider is synthetic.
use super::broker_client::Client;
use super::*;
use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    Router,
};
use plurx_notification_broker::{
    config::{Android, Apple, AppleEnvironment, Publisher},
    http::Broker,
    provider,
    store::Store as BrokerStore,
};
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Mutex,
    },
    time::Duration,
};
use tokio::sync::Semaphore;
#[derive(Default)]
struct SyntheticProvider {
    sent: Mutex<Vec<Value>>,
}
#[async_trait::async_trait]
impl provider::Transport for SyntheticProvider {
    async fn request(
        &self,
        request: provider::Request,
    ) -> plurx_notification_broker::Result<provider::Response> {
        if request.url == "https://oauth2.googleapis.com/token" {
            return Ok(provider::Response{status:200,body:br#"{"access_token":"synthetic-provider-access","token_type":"Bearer","expires_in":3600}"#.to_vec().into()});
        }
        assert_eq!(
            request.url,
            "https://fcm.googleapis.com/v1/projects/cinema-fixture/messages:send"
        );
        let value: Value =
            serde_json::from_slice(&request.body).expect("synthetic provider payload");
        self.sent.lock().expect("synthetic provider").push(value);
        Ok(provider::Response {
            status: 200,
            body: br#"{"name":"projects/cinema-fixture/messages/synthetic"}"#
                .to_vec()
                .into(),
        })
    }
}
struct Gate {
    mode: AtomicU8,
    entered: Semaphore,
    release: Semaphore,
}
impl Default for Gate {
    fn default() -> Self {
        Self {
            mode: AtomicU8::new(0),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}
async fn pause(State(gate): State<Arc<Gate>>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let reply = next.run(request).await;
    let mode = gate.mode.load(Ordering::Relaxed);
    if (mode == 1 && method == Method::POST && path == "/broker/v1/tickets")
        || (mode == 2 && path == "/broker/v1/tickets/status")
    {
        gate.entered.add_permits(1);
        if let Ok(permit) = gate.release.acquire().await {
            permit.forget();
        }
    }
    reply
}
struct BrokerFixture {
    _tmp: tempfile::TempDir,
    origin: String,
    generation: String,
    certificate: Vec<u8>,
    provider: Arc<SyntheticProvider>,
    gate: Arc<Gate>,
    handle: axum_server::Handle<std::net::SocketAddr>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl BrokerFixture {
    async fn new_generation(state: &AppState, home_generation: Option<String>) -> Self {
        let tmp = tempfile::tempdir().expect("synthetic fixture");
        let instance = state.store.instance_id().await.expect("instance");
        let generation = Uuid::new_v4().to_string();
        let publisher_id = Uuid::new_v4().to_string();
        let (publisher_proof, _) = secret().expect("synthetic proof");
        let publisher = Publisher {
            publisher_id: publisher_id.clone(),
            server_instance_id: instance.clone(),
            proof_hash: auth::hash_token(&publisher_proof),
            apple: Some(Apple {
                team_id: "SYNTHETIC1".into(),
                key_id: "SYNTHETIC2".into(),
                topic: "local.cinema.fixture".into(),
                environment: AppleEnvironment::Sandbox,
                private_key_file: tmp.path().join("synthetic-apple.pk8"),
                key_der: rcgen::KeyPair::generate()
                    .expect("synthetic Apple key")
                    .serialize_der()
                    .into(),
            }),
            android: Some(Android {
                project_id: "cinema-fixture".into(),
                service_account_email: "fixture@cinema-fixture.iam.gserviceaccount.com".into(),
                private_key_file: tmp.path().join("synthetic-rsa.pk8"),
                key_der: include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../plurx-notification-broker/tests/fixtures/synthetic-rsa.pk8"
                ))
                .to_vec()
                .into(),
            }),
        };
        let db = tmp.path().join("broker.db");
        let key = [37_u8; 32];
        let config_hash = auth::hash_token("synthetic broker configuration");
        let now = now_seconds().expect("clock");
        BrokerStore::initialize(
            &db,
            &generation,
            &key,
            &config_hash,
            &[publisher.record()],
            now,
        )
        .expect("synthetic broker initialize");
        let store = BrokerStore::open(&db, &generation, &key, &config_hash)
            .expect("synthetic broker store");
        let provider = Arc::new(SyntheticProvider::default());
        let broker = Broker::new(
            generation.clone(),
            store,
            vec![publisher],
            provider.clone(),
            now,
        )
        .expect("synthetic broker");
        let gate = Arc::new(Gate::default());
        let router = broker
            .router()
            .layer(middleware::from_fn_with_state(gate.clone(), pause));
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .expect("synthetic TLS");
        let certificate = cert.cert.pem().into_bytes();
        let key_pem = cert.signing_key.serialize_pem().into_bytes();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(certificate.clone(), key_pem)
            .await
            .expect("synthetic TLS");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking loopback");
        let origin = format!(
            "https://localhost:{}",
            listener.local_addr().expect("loopback").port()
        );
        let handle = axum_server::Handle::new();
        let server = axum_server::from_tcp_rustls(listener, tls)
            .expect("loopback TLS")
            .handle(handle.clone());
        let task = tokio::spawn(server.serve(router.into_make_service()));
        let proof_path = tmp.path().join("publisher-proof");
        std::fs::write(&proof_path, publisher_proof).expect("synthetic proof");
        let configured_generation = home_generation.unwrap_or_else(|| generation.clone());
        let config = json!({"broker_origin":origin,"publisher_id":publisher_id,"broker_generation":configured_generation,"server_instance_id":instance,"publisher_secret_file":proof_path});
        let client = Client::fixture(
            &serde_json::to_vec(&config).expect("synthetic config"),
            &certificate,
        )
        .expect("trusted fixture client");
        assert!(state.invitations.broker.set(Ok(Some(client))).is_ok());
        Self {
            _tmp: tmp,
            origin,
            generation,
            certificate,
            provider,
            gate,
            handle,
            task,
        }
    }
    fn phone_client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .add_root_certificate(
                reqwest::Certificate::from_pem(&self.certificate)
                    .expect("trusted fixture certificate"),
            )
            .build()
            .expect("fixture phone")
    }
    async fn stop(self) {
        self.handle.graceful_shutdown(Some(Duration::from_secs(1)));
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(2), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
}
struct HomeFixture {
    app: Router,
    state: AppState,
    token: String,
    phone_id: String,
    phone_proof: String,
    receiver: String,
    grant: String,
    grant_proof: String,
    samples: Vec<Value>,
}
impl HomeFixture {
    async fn new() -> (Self, BrokerFixture) {
        Self::new_platform("android").await
    }
    async fn new_platform(platform: &str) -> (Self, BrokerFixture) {
        Self::new_platform_generation(platform, None).await
    }
    async fn new_platform_generation(
        platform: &str,
        home_generation: Option<String>,
    ) -> (Self, BrokerFixture) {
        use plurx_core::store::remote::{
            NewRemoteGrant, NewRemoteReceiver, RemoteGrant, RemoteReceiver,
        };
        let (app, state) = crate::http::tests::test_app_with_state();
        let token = super::tests::login(&state).await;
        let user = state
            .store
            .invitation_login(&auth::hash_token(&token), now_seconds().expect("clock"))
            .await
            .expect("login")
            .expect("native")
            .user_id;
        for key in ["cinema.remote_control", "cinema.remote_invitations"] {
            state
                .store
                .put_setting(key, "1")
                .await
                .expect("fixture setting");
        }
        let phone_id = Uuid::new_v4().to_string();
        let (_,registered)=super::tests::call(&app,&token,"phones",&json!({"version":"cinema.invitation.v1","installation_id":phone_id,"platform":platform,"name":"Phone"}).to_string(),None).await;
        let phone_proof = registered["phone_secret"]
            .as_str()
            .expect("phone proof")
            .to_owned();
        let mut samples = vec![json!({"operation":"phones","response":registered})];
        let availability = json!({"version":"cinema.invitation.v1","expected_phone_generation":1,"permission_granted":true,"resident_active":false});
        let available = super::tests::call(
            &app,
            &token,
            &format!("phones/{phone_id}/availability"),
            &availability.to_string(),
            Some(&phone_proof),
        )
        .await;
        assert_eq!(available.0, 200);
        samples.push(json!({"operation":"availability","response":available.1}));
        let receiver = Uuid::new_v4().to_string();
        let grant = Uuid::new_v4().to_string();
        let (receiver_proof, receiver_hash) = secret().expect("synthetic receiver");
        let (grant_proof, grant_hash) = secret().expect("synthetic grant");
        assert!(state
            .store
            .create_remote_receiver(NewRemoteReceiver {
                receiver: RemoteReceiver {
                    id: receiver.clone(),
                    user_id: user,
                    name: "TV".into(),
                    platform: "web".into(),
                    created_at: now_seconds().expect("clock")
                },
                secret_hash: receiver_hash
            })
            .await
            .expect("receiver"));
        assert!(state
            .store
            .create_remote_grant(NewRemoteGrant {
                grant: RemoteGrant {
                    id: grant.clone(),
                    receiver_id: receiver.clone(),
                    name: "Phone".into(),
                    created_at: now_seconds().expect("clock")
                },
                user_id: user,
                secret_hash: grant_hash
            })
            .await
            .expect("grant"));
        let dispatch = super::super::wire::Dispatch {
            version: plurx_core::remote_control::Version::V1,
            user_id: user,
            token_digest: auth::hash_token(&token),
            proof: super::super::wire::Proof {
                receiver_hash: Some(auth::hash_token(&receiver_proof)),
                grant_hash: None,
                pairing_hash: None,
            },
            request: super::super::wire::Request::CreateSession(
                super::super::wire::CreateSession {
                    version: plurx_core::remote_control::Version::V1,
                    receiver_id: Uuid::parse_str(&receiver).expect("receiver"),
                    foreground_id: Uuid::new_v4(),
                },
            ),
        };
        state
            .remote
            .perform(&state, dispatch)
            .await
            .expect("actual receiver owner");
        let broker = BrokerFixture::new_generation(&state, home_generation).await;
        let transport = if platform == "apple" { "apns" } else { "fcm" };
        let consent = json!({"version":"cinema.invitation.v1","installation_id":phone_id,"receiver_id":receiver,"grant_id":grant,"expected_phone_generation":2,"expected_consent_generation":0,"enabled":true,"transport":transport});
        let consent_reply = super::tests::call_proofs(
            &app,
            &token,
            "invitations/consent",
            &consent.to_string(),
            Some(&phone_proof),
            Some(&grant_proof),
        )
        .await;
        assert_eq!(consent_reply.0, 200);
        samples.push(json!({"operation":"consent_on","response":consent_reply.1}));
        (
            Self {
                app,
                state,
                token,
                phone_id,
                phone_proof,
                receiver,
                grant,
                grant_proof,
                samples,
            },
            broker,
        )
    }
    async fn start(&self, generation: i64) -> (u16, Value) {
        super::tests::call_proofs(&self.app,&self.token,"invitations/transport/start",&json!({"version":"cinema.invitation.v1","installation_id":self.phone_id,"receiver_id":self.receiver,"grant_id":self.grant,"expected_phone_generation":2,"expected_consent_generation":generation}).to_string(),Some(&self.phone_proof),Some(&self.grant_proof)).await
    }
    async fn confirm(&self, start: &Value) -> (u16, Value) {
        super::tests::call_proofs(&self.app,&self.token,"invitations/transport/confirm",&json!({"version":"cinema.invitation.v1","installation_id":self.phone_id,"receiver_id":self.receiver,"ticket_id":start["ticket"]["ticket_id"],"expected_phone_generation":2,"expected_consent_generation":start["consent"]["consent_generation"],"expected_transport_generation":start["consent"]["transport_generation"]}).to_string(),Some(&self.phone_proof),Some(&self.grant_proof)).await
    }
}
#[tokio::test]
async fn home_and_real_broker_enroll_claim_confirm_deliver_and_revoke() {
    let (home, broker) = HomeFixture::new().await;
    let (status, start) = home.start(1).await;
    assert_eq!(status, 200);
    assert!(!start["ticket"].is_null());
    assert_eq!(start["ticket"]["broker_origin"], broker.origin);
    assert_eq!(start["ticket"]["broker_generation"], broker.generation);
    let claim=broker.phone_client().post(format!("{}/broker/v1/tickets/claim",broker.origin)).bearer_auth(start["ticket"]["ticket_secret"].as_str().expect("ticket proof")).json(&json!({"version":"cinema.invitation.v1","ticket_id":start["ticket"]["ticket_id"],"platform":"android","device_token":"synthetic-device-token"})).send().await.expect("actual broker claim");
    assert_eq!(claim.status(), 200);
    assert_eq!(
        claim.headers()["x-cinema-broker-generation"],
        broker.generation
    );
    let (status, confirmed) = home.confirm(&start).await;
    assert_eq!(status, 200);
    assert_eq!(
        confirmed["consent"]["consent_generation"],
        start["consent"]["consent_generation"]
    );
    assert_eq!(confirmed["consent"]["readiness"]["status"], "ready");
    assert_eq!(home.confirm(&start).await.0, 200, "exact Confirm retry");
    let scope = home
        .state
        .store
        .invitation_scope(
            &home.phone_id,
            &home.receiver,
            home.state
                .store
                .invitation_login(
                    &auth::hash_token(&home.token),
                    now_seconds().expect("clock"),
                )
                .await
                .expect("login")
                .expect("native")
                .user_id,
        )
        .await
        .expect("scope")
        .expect("scope");
    super::worker::dispatch(&home.state, scope)
        .await
        .expect("actual worker");
    let sent = broker
        .provider
        .sent
        .lock()
        .expect("synthetic provider")
        .clone();
    assert_eq!(sent.len(), 1);
    let data = sent[0]["message"]["data"].as_object().expect("data only");
    assert_eq!(data.len(), 2);
    assert_eq!(data["category"], "CINEMA_REMOTE_INVITATION");
    assert!(sent[0]["message"].get("notification").is_none());
    let lookup=super::tests::call(&home.app,&home.token,"invitations/lookup",&json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"invitation_id":data["invitation_id"]}).to_string(),Some(&home.phone_proof)).await;
    assert_eq!(lookup.0, 200);
    assert_eq!(lookup.1["receiver_id"], home.receiver);
    if let Some(path) = std::env::var_os("PLURX_INVITATION_FIXTURE_SAMPLES") {
        let list = super::tests::call(&home.app,&home.token,"invitations/consents/list",&json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"after_receiver_id":null,"limit":20}).to_string(),Some(&home.phone_proof)).await;
        assert_eq!(list.0, 200);
        let mut samples = home.samples.clone();
        samples.extend([
            json!({"operation":"transport_start","response":start}),
            json!({"operation":"transport_confirm","response":confirmed}),
            json!({"operation":"consents_list","response":list.1}),
            json!({"operation":"lookup","response":lookup.1}),
        ]);
        std::fs::write(path,serde_json::to_vec_pretty(&json!({"synthetic_only":true,"live":false,"broker_origin":broker.origin,"broker_generation":broker.generation,"installation_id":home.phone_id,"receiver_id":home.receiver,"grant_id":home.grant,"samples":samples})).expect("synthetic samples")).expect("requested synthetic samples output");
    }

    let off = json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"receiver_id":home.receiver,"expected_phone_generation":2,"expected_consent_generation":start["consent"]["consent_generation"],"enabled":false});
    assert_eq!(
        super::tests::call(
            &home.app,
            &home.token,
            "invitations/consent",
            &off.to_string(),
            Some(&home.phone_proof)
        )
        .await
        .0,
        200
    );
    assert_eq!(
        home.state
            .store
            .invitation_revocations()
            .await
            .expect("retained cleanup")
            .len(),
        1
    );
    super::worker::cleanup(&home.state, &mut String::new())
        .await
        .expect("authenticated generation tombstone");
    assert!(home
        .state
        .store
        .invitation_revocations()
        .await
        .expect("cleanup ack")
        .is_empty());
    assert_eq!(super::tests::call(&home.app,&home.token,"invitations/lookup",&json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"invitation_id":data["invitation_id"]}).to_string(),Some(&home.phone_proof)).await.0,409);
    broker.stop().await;
}

#[tokio::test]
async fn issue_reply_rechecks_permission_login_and_global_switch() {
    for change in ["permission", "login", "global"] {
        let (home, broker) = HomeFixture::new().await;
        broker.gate.mode.store(1, Ordering::Relaxed);
        let issue = home.start(1);
        let invalidate = async {
            tokio::time::timeout(Duration::from_secs(3), broker.gate.entered.acquire())
                .await
                .expect("bounded issued response")
                .expect("gate")
                .forget();
            match change {
                "permission" => {
                    let body = json!({"version":"cinema.invitation.v1","expected_phone_generation":2,"permission_granted":false,"resident_active":false});
                    assert_eq!(
                        super::tests::call(
                            &home.app,
                            &home.token,
                            &format!("phones/{}/availability", home.phone_id),
                            &body.to_string(),
                            Some(&home.phone_proof)
                        )
                        .await
                        .0,
                        200
                    );
                }
                "login" => {
                    assert!(home
                        .state
                        .store
                        .delete_token(&auth::hash_token(&home.token))
                        .await
                        .expect("revoke synthetic login"));
                }
                _ => {
                    home.state
                        .store
                        .put_setting("cinema.remote_invitations", "0")
                        .await
                        .expect("global OFF");
                }
            }
            broker.gate.release.add_permits(1);
        };
        let (reply, ()) = tokio::join!(issue, invalidate);
        assert_ne!(reply.0, 200, "late {change} cannot return capability");
        assert!(reply.1.get("ticket").is_none());
        assert_eq!(
            home.state
                .store
                .invitation_revocations()
                .await
                .expect("retained exact cleanup")
                .len(),
            1
        );
        broker.stop().await;
    }
}

#[tokio::test]
async fn retained_authority_audit_runs_with_dispatch_disabled() {
    for change in ["valid", "login", "grant", "receiver"] {
        let (home, broker) = HomeFixture::new().await;
        let (status, _) = home.start(1).await;
        assert_eq!(status, 200);
        let user = home
            .state
            .store
            .invitation_login(
                &auth::hash_token(&home.token),
                now_seconds().expect("clock"),
            )
            .await
            .expect("native login")
            .expect("login")
            .user_id;
        home.state
            .store
            .put_setting("cinema.remote_invitations", "0")
            .await
            .expect("dispatch OFF");
        match change {
            "login" => {
                assert!(home
                    .state
                    .store
                    .delete_token(&auth::hash_token(&home.token))
                    .await
                    .expect("revoke login"));
            }
            "grant" => {
                home.state
                    .store
                    .revoke_remote_grant(&home.grant, user, now_seconds().expect("clock"))
                    .await
                    .expect("revoke grant");
            }
            "receiver" => {
                home.state
                    .store
                    .revoke_remote_receiver(&home.receiver, user, now_seconds().expect("clock"))
                    .await
                    .expect("revoke receiver");
            }
            _ => {}
        }
        super::worker::audit(&home.state, &mut String::new())
            .await
            .expect("bounded retained authority audit");
        let queued = home
            .state
            .store
            .invitation_revocations()
            .await
            .expect("durable cleanup");
        assert_eq!(
            queued.len(),
            usize::from(change != "valid"),
            "global OFF alone preserves valid binding; invalid {change} retains cleanup"
        );
        broker.stop().await;
    }
}

#[tokio::test]
async fn confirm_reply_rechecks_permission_login_and_global_switch() {
    for change in ["permission", "login", "global"] {
        let (home, broker) = HomeFixture::new().await;
        let (status, start) = home.start(1).await;
        assert_eq!(status, 200);
        let claim = broker.phone_client().post(format!("{}/broker/v1/tickets/claim",broker.origin)).bearer_auth(start["ticket"]["ticket_secret"].as_str().expect("ticket proof")).json(&json!({"version":"cinema.invitation.v1","ticket_id":start["ticket"]["ticket_id"],"platform":"android","device_token":"synthetic-device-token"})).send().await.expect("actual claim");
        assert_eq!(claim.status(), 200);
        broker.gate.mode.store(2, Ordering::Relaxed);
        let confirm = home.confirm(&start);
        let invalidate = async {
            tokio::time::timeout(Duration::from_secs(3), broker.gate.entered.acquire())
                .await
                .expect("bounded status response")
                .expect("gate")
                .forget();
            match change {
                "permission" => {
                    let body = json!({"version":"cinema.invitation.v1","expected_phone_generation":2,"permission_granted":false,"resident_active":false});
                    assert_eq!(
                        super::tests::call(
                            &home.app,
                            &home.token,
                            &format!("phones/{}/availability", home.phone_id),
                            &body.to_string(),
                            Some(&home.phone_proof)
                        )
                        .await
                        .0,
                        200
                    );
                }
                "login" => {
                    assert!(home
                        .state
                        .store
                        .delete_token(&auth::hash_token(&home.token))
                        .await
                        .expect("revoke synthetic login"));
                }
                _ => {
                    home.state
                        .store
                        .put_setting("cinema.remote_invitations", "0")
                        .await
                        .expect("global OFF");
                }
            }
            broker.gate.release.add_permits(1);
        };
        let (reply, ()) = tokio::join!(confirm, invalidate);
        assert_ne!(reply.0, 200, "late {change} cannot report ready");
        assert_eq!(
            home.state
                .store
                .invitation_revocations()
                .await
                .expect("exact retained cleanup")
                .len(),
            1
        );
        broker.stop().await;
    }
}

#[tokio::test]
async fn resident_poll_holds_admission_barrier_and_does_not_starve_off() {
    let (home, broker, _) = resident_admission().await;
    let poll = json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"after_revision":0,"wait_ms":0});
    let reply = super::tests::call(
        &home.app,
        &home.token,
        "invitations/poll",
        &poll.to_string(),
        Some(&home.phone_proof),
    )
    .await;
    assert_eq!(reply.0, 200);
    assert_eq!(reply.1["revision"], 0);
    assert_eq!(reply.1["invitations"], json!([]));
    assert_eq!(
        home.state.invitations.poll_slots.available_permits(),
        64,
        "completed poll releases admission permit"
    );
    let held = home
        .state
        .invitations
        .poll_slots
        .clone()
        .acquire_many_owned(64)
        .await
        .expect("saturated dedicated poll pool");
    assert_eq!(
        super::tests::call(
            &home.app,
            "not-a-valid-login",
            "invitations/poll",
            &poll.to_string(),
            None
        )
        .await
        .0,
        429,
        "dedicated admission rejects before authentication I/O"
    );
    let off = json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"receiver_id":home.receiver,"expected_phone_generation":3,"expected_consent_generation":2,"enabled":false});
    assert_eq!(
        super::tests::call(
            &home.app,
            &home.token,
            "invitations/consent",
            &off.to_string(),
            Some(&home.phone_proof)
        )
        .await
        .0,
        200,
        "foreground OFF retains its independent request capacity"
    );
    drop(held);
    broker.stop().await;
}

#[tokio::test]
async fn apple_actual_router_enrollment_matches_frozen_native_contract() {
    let (home, broker) = HomeFixture::new_platform("apple").await;
    let (status, start) = home.start(1).await;
    assert_eq!(status, 200);
    let claim = broker.phone_client().post(format!("{}/broker/v1/tickets/claim",broker.origin)).bearer_auth(start["ticket"]["ticket_secret"].as_str().expect("ticket proof")).json(&json!({"version":"cinema.invitation.v1","ticket_id":start["ticket"]["ticket_id"],"platform":"apple","device_token":"ab".repeat(32)})).send().await.expect("actual ticket-only Apple claim");
    assert_eq!(claim.status(), 200);
    let generation = claim.headers()["x-cinema-broker-generation"]
        .to_str()
        .expect("generation")
        .to_owned();
    assert_eq!(generation, broker.generation);
    let claim_bytes = claim
        .bytes()
        .await
        .expect("bounded synthetic claim response");
    assert!(claim_bytes.len() <= 65536);
    let claim_value: Value = serde_json::from_slice(&claim_bytes).expect("actual claim JSON");
    let (status, confirmed) = home.confirm(&start).await;
    assert_eq!(status, 200);
    assert_eq!(confirmed["consent"]["transport"], "apns");
    assert_eq!(confirmed["consent"]["readiness"]["status"], "ready");
    let retry = super::tests::call(&home.app,&home.token,"phones",&json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"platform":"apple","name":"Phone"}).to_string(),Some(&home.phone_proof)).await;
    assert_eq!(retry.0, 200);
    assert!(retry.1["phone_secret"].is_null());
    let list = super::tests::call(&home.app,&home.token,"invitations/consents/list",&json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"after_receiver_id":null,"limit":20}).to_string(),Some(&home.phone_proof)).await;
    assert_eq!(list.0, 200);
    if let Some(path) = std::env::var_os("PLURX_INVITATION_APPLE_FIXTURE_SAMPLES") {
        let mut samples = home.samples.clone();
        samples.extend([
            json!({"operation":"phones_proved_retry","response":retry.1}),
            json!({"operation":"transport_start","response":start}),
            json!({"operation":"transport_confirm","response":confirmed}),
            json!({"operation":"consents_list","response":list.1}),
            json!({"operation":"broker_claim","response":claim_value}),
        ]);
        std::fs::write(path,serde_json::to_vec_pretty(&json!({"synthetic_only":true,"live":false,"installation_id":home.phone_id,"receiver_id":home.receiver,"grant_id":home.grant,"broker_origin":broker.origin,"broker_generation":generation,"raw_claim_body":String::from_utf8(claim_bytes.to_vec()).expect("JSON UTF8"),"claim_headers":{"X-Cinema-Broker-Generation":generation},"samples":samples})).expect("synthetic samples")).expect("requested Apple conformance output");
    }
    broker.stop().await;
}

#[tokio::test]
async fn new_broker_generation_cannot_acknowledge_old_scope_cleanup() {
    let old_generation = Uuid::new_v4().to_string();
    let (home, broker) =
        HomeFixture::new_platform_generation("android", Some(old_generation.clone())).await;
    assert_ne!(old_generation, broker.generation);
    // Publisher and rotated proof are valid at the actual broker; only the
    // externally verified generation differs from the retained home scope.
    let (status, reply) = home.start(1).await;
    assert_eq!(status, 503);
    assert_eq!(reply["code"], "migration_remediation");
    let before = home
        .state
        .store
        .invitation_revocations()
        .await
        .expect("old obligation");
    assert_eq!(before.len(), 1);
    super::worker::cleanup(&home.state, &mut String::new())
        .await
        .expect("unknown generation cleanup retained");
    let after = home
        .state
        .store
        .invitation_revocations()
        .await
        .expect("retained old scope");
    assert_eq!(after.len(), 1);
    assert_eq!(before[0].id, after[0].id);
    assert!(broker
        .provider
        .sent
        .lock()
        .expect("synthetic provider")
        .is_empty());
    broker.stop().await;
}

async fn resident_admission() -> (
    HomeFixture,
    BrokerFixture,
    plurx_core::store::invitations::AdmitInvitation,
) {
    use plurx_core::store::invitations::{AdmitInvitation, InvitationAdmission};
    let (home, broker) = HomeFixture::new().await;
    let available = json!({"version":"cinema.invitation.v1","expected_phone_generation":2,"permission_granted":true,"resident_active":true});
    assert_eq!(
        super::tests::call(
            &home.app,
            &home.token,
            &format!("phones/{}/availability", home.phone_id),
            &available.to_string(),
            Some(&home.phone_proof)
        )
        .await
        .0,
        200
    );
    let consent = json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"receiver_id":home.receiver,"grant_id":home.grant,"expected_phone_generation":3,"expected_consent_generation":1,"enabled":true,"transport":"android_resident"});
    assert_eq!(
        super::tests::call_proofs(
            &home.app,
            &home.token,
            "invitations/consent",
            &consent.to_string(),
            Some(&home.phone_proof),
            Some(&home.grant_proof)
        )
        .await
        .0,
        200
    );
    let digest = auth::hash_token(&home.token);
    let login = home
        .state
        .store
        .invitation_login(&digest, now_seconds().expect("clock"))
        .await
        .expect("login")
        .expect("native");
    let scope = home
        .state
        .store
        .invitation_scope(&home.phone_id, &home.receiver, login.user_id)
        .await
        .expect("scope")
        .expect("scope");
    let summaries = super::snapshots::collect(&home.state, login.user_id, &digest)
        .await
        .expect("actual owner summary");
    let receiver = super::snapshots::unique(&summaries, &home.receiver).expect("unique owner");
    let admit = AdmitInvitation {
        id: Uuid::new_v4().to_string(),
        enrollment_id: scope.consent.id,
        foreground_id: receiver.foreground_id.to_string(),
        receiver_hash: scope.receiver_hash,
        phone_generation: 3,
        consent_generation: 2,
        transport_generation: scope.consent.transport_generation,
        phone_login: login.clone(),
        receiver_login: login,
        now: now_seconds().expect("clock"),
    };
    assert!(matches!(
        home.state
            .store
            .admit_invitation(admit.clone())
            .await
            .expect("admission"),
        InvitationAdmission::Admitted
    ));
    (home, broker, admit)
}

#[tokio::test]
async fn resident_poll_rechecks_off_after_awaited_owner_collection() {
    use plurx_core::store::invitations::InvitationDispatchOutcome;
    for global in [false, true] {
        let (home, broker, admit) = resident_admission().await;
        assert!(home
            .state
            .store
            .attempt_invitation(admit.clone())
            .await
            .expect("attempt"));
        home.state
            .store
            .finish_invitation(&admit.id, InvitationDispatchOutcome::ResidentReady)
            .await
            .expect("resident ready");
        let gate = Arc::new(super::snapshots::TestGate {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        *home.state.invitations.snapshot_gate.lock().expect("gate") = Some(gate.clone());
        let body=json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"after_revision":0,"wait_ms":0}).to_string();
        let poll = super::tests::call(
            &home.app,
            &home.token,
            "invitations/poll",
            &body,
            Some(&home.phone_proof),
        );
        let off = async {
            tokio::time::timeout(Duration::from_secs(1), gate.entered.acquire())
                .await
                .expect("bounded owner await")
                .expect("gate")
                .forget();
            if global {
                home.state
                    .store
                    .put_setting("cinema.remote_invitations", "0")
                    .await
                    .expect("global OFF");
            } else {
                let off = json!({"version":"cinema.invitation.v1","installation_id":home.phone_id,"receiver_id":home.receiver,"expected_phone_generation":3,"expected_consent_generation":2,"enabled":false});
                assert_eq!(
                    super::tests::call(
                        &home.app,
                        &home.token,
                        "invitations/consent",
                        &off.to_string(),
                        Some(&home.phone_proof)
                    )
                    .await
                    .0,
                    200
                );
            }
            gate.release.add_permits(1);
        };
        let (reply, ()) = tokio::join!(poll, off);
        if global {
            assert_eq!(reply.0, 409);
            assert_eq!(reply.1["code"], "global_disabled");
        } else {
            assert_eq!(reply.0, 200);
            assert_eq!(reply.1["invitations"], json!([]));
        }
        broker.stop().await;
    }
}
