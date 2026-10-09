use super::*;
use crate::{
    config::{Apple, AppleEnvironment},
    provider::{Request as ProviderRequest, Response as ProviderResponse},
    wire::Platform,
};
use async_trait::async_trait;
use axum::body::Body;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{rand::SystemRandom, signature};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;
use zeroize::Zeroizing;
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
struct Fake {
    count: AtomicUsize,
    hold: bool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl Transport for Fake {
    async fn request(&self, _request: ProviderRequest) -> Result<ProviderResponse> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.hold {
            self.release.notified().await;
        }
        Ok(ProviderResponse {
            status: 200,
            body: Zeroizing::new(Vec::new()),
        })
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    broker: Arc<Broker>,
    generation: String,
    publisher: String,
    other_publisher: String,
    proof: String,
    ticket_proof: String,
    transport: Arc<Fake>,
}
impl Fixture {
    fn new(hold: bool) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let generation = uuid::Uuid::new_v4().to_string();
        let proof = URL_SAFE_NO_PAD.encode([5; 32]);
        let publisher = uuid::Uuid::new_v4().to_string();
        let der = signature::EcdsaKeyPair::generate_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &SystemRandom::new(),
        )
        .map_err(|_| "synthetic key")?;
        let config = Publisher {
            publisher_id: publisher.clone(),
            server_instance_id: "synthetic-home".into(),
            proof_hash: hex::encode(wire::proof_hash(&proof)?),
            apple: Some(Apple {
                team_id: "TEAM123456".into(),
                key_id: "KEY1234567".into(),
                topic: "tv.plurx.synthetic".into(),
                environment: AppleEnvironment::Sandbox,
                private_key_file: "unused.pk8".into(),
                key_der: Zeroizing::new(der.as_ref().to_vec()),
            }),
            android: None,
        };
        let path = dir.path().join("broker.sqlite");
        let now = provider::now()?;
        Store::initialize(
            &path,
            &generation,
            &[7; 32],
            crate::config::STORAGE_REALM,
            &[config.record()],
            now,
        )?;
        let store = Store::open(&path, &generation, &[7; 32], crate::config::STORAGE_REALM)?;
        let transport = Arc::new(Fake {
            count: AtomicUsize::new(0),
            hold,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let mut other = config.clone();
        other.publisher_id = uuid::Uuid::new_v4().to_string();
        let other_publisher = other.publisher_id.clone();
        let broker = Broker::new(
            generation.clone(),
            store,
            vec![config, other],
            transport.clone(),
            now,
        )?;
        Ok(Self {
            _dir: dir,
            broker,
            generation,
            publisher,
            other_publisher,
            proof,
            ticket_proof: URL_SAFE_NO_PAD.encode([6; 32]),
            transport,
        })
    }
    fn ticket(&self) -> Result<Ticket> {
        Ok(Ticket {
            version: wire::VERSION.into(),
            ticket_id: uuid::Uuid::new_v4().to_string(),
            ticket_secret_hash: hex::encode(wire::proof_hash(&self.ticket_proof)?),
            server_instance_id: "synthetic-home".into(),
            installation_id: uuid::Uuid::new_v4().to_string(),
            receiver_id: uuid::Uuid::new_v4().to_string(),
            consent_id: uuid::Uuid::new_v4().to_string(),
            phone_generation: 1,
            consent_generation: 1,
            transport_generation: 1,
            platform: Platform::Apple,
        })
    }
    fn request(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
    ) -> std::result::Result<Request, Box<dyn std::error::Error>> {
        Ok(Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", self.proof))
            .header(PUBLISHER_HEADER, &self.publisher)
            .header(GENERATION_HEADER, &self.generation)
            .body(Body::from(body))?)
    }
    async fn send(
        &self,
        request: Request,
    ) -> std::result::Result<(StatusCode, serde_json::Value), Box<dyn std::error::Error>> {
        let response = self.broker.clone().router().oneshot(request).await?;
        assert_eq!(
            response
                .headers()
                .get(GENERATION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(self.generation.as_str())
        );
        let status = response.status();
        let bytes = to_bytes(response.into_body(), wire::MAX_BODY).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }
    async fn enroll(&self, ticket: &Ticket) -> TestResult {
        let (status, _) = self
            .send(self.request(
                Method::POST,
                "/broker/v1/tickets",
                serde_json::to_vec(ticket)?,
            )?)
            .await?;
        assert_eq!(status, StatusCode::OK);
        let claim = serde_json::json!({"version":wire::VERSION,"ticket_id":ticket.ticket_id,"platform":"apple","device_token":"aabb"});
        let request = Request::builder()
            .method(Method::POST)
            .uri("/broker/v1/tickets/claim")
            .header("content-type", "application/json; charset=utf-8")
            .header("authorization", format!("Bearer {}", self.ticket_proof))
            .body(Body::from(serde_json::to_vec(&claim)?))?;
        assert_eq!(self.send(request).await?.0, StatusCode::OK);
        Ok(())
    }
    fn delivery(ticket: &Ticket) -> Result<Delivery> {
        let mut bytes = wire::uuid(&ticket.installation_id)?.as_bytes().to_vec();
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        Ok(Delivery {
            version: wire::VERSION.into(),
            enrollment_id: ticket.ticket_id.clone(),
            installation_id: ticket.installation_id.clone(),
            phone_generation: 1,
            consent_generation: 1,
            transport_generation: 1,
            invitation_id: URL_SAFE_NO_PAD.encode(bytes),
            expires_at: provider::now()? + 120,
        })
    }
}
#[tokio::test]
async fn real_router_ticket_claim_delivery_dedupe_and_scoped_revoke() -> TestResult {
    let fixture = Fixture::new(false)?;
    let ticket = fixture.ticket()?;
    fixture.enroll(&ticket).await?;
    let status = serde_json::json!({"version":wire::VERSION,"ticket_id":ticket.ticket_id});
    let (_, body) = fixture
        .send(fixture.request(
            Method::POST,
            "/broker/v1/tickets/status",
            serde_json::to_vec(&status)?,
        )?)
        .await?;
    assert_eq!(body["status"], "claimed");
    assert_eq!(body["enrollment_id"], ticket.ticket_id);
    let delivery = Fixture::delivery(&ticket)?;
    let bytes = serde_json::to_vec(&delivery)?;
    let (_, body) = fixture
        .send(fixture.request(Method::POST, "/broker/v1/deliveries", bytes.clone())?)
        .await?;
    assert_eq!(body["status"], "accepted");
    let (_, body) = fixture
        .send(fixture.request(Method::POST, "/broker/v1/deliveries", bytes)?)
        .await?;
    assert_eq!(body["status"], "duplicate");
    assert_eq!(fixture.transport.count.load(Ordering::SeqCst), 1);
    let path = format!("/broker/v1/enrollments/{}", ticket.ticket_id);
    assert_eq!(
        fixture
            .send(fixture.request(Method::DELETE, &path, Vec::new())?)
            .await?
            .0,
        StatusCode::OK
    );
    let (_, body) = fixture
        .send(fixture.request(
            Method::POST,
            "/broker/v1/tickets/status",
            serde_json::to_vec(&status)?,
        )?)
        .await?;
    assert_eq!(body["status"], "revoked");
    Ok(())
}
#[tokio::test]
async fn generation_and_strict_headers_fail_before_effect_and_all_errors_are_fenced() -> TestResult
{
    let fixture = Fixture::new(false)?;
    let ticket = fixture.ticket()?;
    let bytes = serde_json::to_vec(&ticket)?;
    for mode in [
        "missing",
        "duplicate",
        "mismatch",
        "proof-whitespace",
        "duplicate-auth",
        "query",
        "claim-identity",
    ] {
        let path = if mode == "query" {
            "/broker/v1/tickets?proof=x"
        } else if mode == "claim-identity" {
            "/broker/v1/tickets/claim"
        } else {
            "/broker/v1/tickets"
        };
        let mut request = fixture.request(Method::POST, path, bytes.clone())?;
        match mode {
            "missing" => {
                request.headers_mut().remove(GENERATION_HEADER);
            }
            "duplicate" => {
                request.headers_mut().append(
                    GENERATION_HEADER,
                    HeaderValue::from_str(&fixture.generation)?,
                );
            }
            "mismatch" => {
                request.headers_mut().insert(
                    GENERATION_HEADER,
                    HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())?,
                );
            }
            "proof-whitespace" => {
                request.headers_mut().insert(
                    "authorization",
                    HeaderValue::from_str(&format!("Bearer  {}", fixture.proof))?,
                );
            }
            "duplicate-auth" => {
                request.headers_mut().append(
                    "authorization",
                    HeaderValue::from_str(&format!("Bearer {}", fixture.proof))?,
                );
            }
            _ => {}
        }
        let (status, body) = fixture.send(request).await?;
        assert!(status.is_client_error());
        assert_eq!(body["version"], wire::VERSION);
        if mode == "mismatch" {
            assert_eq!(body["code"], "stale_broker_generation");
        }
    }
    let status = serde_json::json!({"version":wire::VERSION,"ticket_id":ticket.ticket_id});
    assert_eq!(
        fixture
            .send(fixture.request(
                Method::POST,
                "/broker/v1/tickets/status",
                serde_json::to_vec(&status)?
            )?)
            .await?
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(fixture.transport.count.load(Ordering::SeqCst), 0);
    Ok(())
}
#[tokio::test]
async fn provider_admission_is_four_and_rejects_without_recording_an_attempt() -> TestResult {
    let fixture = Fixture::new(true)?;
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let ticket = fixture.ticket()?;
        fixture.enroll(&ticket).await?;
        let request = fixture.request(
            Method::POST,
            "/broker/v1/deliveries",
            serde_json::to_vec(&Fixture::delivery(&ticket)?)?,
        )?;
        let router = fixture.broker.clone().router();
        tasks.push(tokio::spawn(async move { router.oneshot(request).await }));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.transport.count.load(Ordering::SeqCst) < 4 {
            fixture.transport.entered.notified().await;
        }
    })
    .await?;
    let ticket = fixture.ticket()?;
    fixture.enroll(&ticket).await?;
    let delivery = Fixture::delivery(&ticket)?;
    let (status, body) = fixture
        .send(fixture.request(
            Method::POST,
            "/broker/v1/deliveries",
            serde_json::to_vec(&delivery)?,
        )?)
        .await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "busy");
    fixture.transport.release.notify_waiters();
    for task in tasks {
        assert_eq!(task.await??.status(), StatusCode::OK);
    }
    assert_eq!(fixture.transport.count.load(Ordering::SeqCst), 4);
    let (status, body) = fixture
        .send(fixture.request(
            Method::POST,
            "/broker/v1/deliveries",
            serde_json::to_vec(&delivery)?,
        )?)
        .await?; // retry reaches provider: rejected admission did not consume the ID.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "unknown");
    assert_eq!(fixture.transport.count.load(Ordering::SeqCst), 5);
    Ok(())
}
#[tokio::test]
async fn loopback_bounded_listener_serves_real_generation_fenced_router() -> TestResult {
    let fixture = Fixture::new(false)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (shutdown, closed) = tokio::sync::oneshot::channel();
    let router = fixture.broker.clone().router();
    let task = tokio::spawn(async move {
        axum::serve(crate::listener::BoundedListener::new(listener), router)
            .with_graceful_shutdown(async {
                let _ = closed.await;
            })
            .await
    });
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .post(format!("http://{address}/broker/v1/tickets"))
        .header("authorization", format!("Bearer {}", fixture.proof))
        .header(PUBLISHER_HEADER, &fixture.publisher)
        .header(GENERATION_HEADER, &fixture.generation)
        .json(&fixture.ticket()?)
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get(GENERATION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(fixture.generation.as_str())
    );
    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["status"], "pending");
    let _ = shutdown.send(());
    task.await??;
    Ok(())
}

#[tokio::test]
async fn cross_publisher_status_and_delete_do_not_disclose_or_revoke_owner() -> TestResult {
    let f = Fixture::new(false)?;
    let ticket = f.ticket()?;
    f.enroll(&ticket).await?;
    let status = serde_json::json!({"version":wire::VERSION,"ticket_id":ticket.ticket_id});
    let mut request = f.request(
        Method::POST,
        "/broker/v1/tickets/status",
        serde_json::to_vec(&status)?,
    )?;
    request
        .headers_mut()
        .insert(PUBLISHER_HEADER, HeaderValue::from_str(&f.other_publisher)?);
    assert_eq!(f.send(request).await?.0, StatusCode::NOT_FOUND);
    for id in [ticket.ticket_id.clone(), uuid::Uuid::new_v4().to_string()] {
        let mut request = f.request(
            Method::DELETE,
            &format!("/broker/v1/enrollments/{id}"),
            Vec::new(),
        )?;
        request
            .headers_mut()
            .insert(PUBLISHER_HEADER, HeaderValue::from_str(&f.other_publisher)?);
        let (code, body) = f.send(request).await?;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["status"], "revoked");
    }
    assert_eq!(
        f.send(f.request(
            Method::POST,
            "/broker/v1/tickets/status",
            serde_json::to_vec(&status)?
        )?)
        .await?
        .1["status"],
        "claimed"
    );
    Ok(())
}
#[tokio::test]
async fn global_http_admission_rejects_without_unbounded_body_waiters() -> TestResult {
    let f = Fixture::new(false)?;
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let mut request = f.request(Method::POST, "/broker/v1/tickets", Vec::new())?;
        *request.body_mut() = Body::from_stream(futures_util::stream::pending::<
            std::result::Result<axum::body::Bytes, std::io::Error>,
        >());
        let router = f.broker.clone().router();
        tasks.push(tokio::spawn(async move { router.oneshot(request).await }));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.broker.permits.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let (status, body) = f
        .send(f.request(
            Method::POST,
            "/broker/v1/tickets",
            serde_json::to_vec(&f.ticket()?)?,
        )?)
        .await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "busy");
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    assert_eq!(f.broker.permits.available_permits(), 16);
    Ok(())
}

#[tokio::test]
async fn native_utf8_json_media_type_is_accepted_and_non_json_charset_refused() -> TestResult {
    let f = Fixture::new(false)?;
    for media in [
        "application/json",
        "application/json; charset=utf-8",
        "Application/JSON; Charset=\"UTF-8\"",
    ] {
        let mut request = f.request(
            Method::POST,
            "/broker/v1/tickets",
            serde_json::to_vec(&f.ticket()?)?,
        )?;
        request
            .headers_mut()
            .insert("content-type", HeaderValue::from_str(media)?);
        assert_eq!(f.send(request).await?.0, StatusCode::OK);
    }
    for media in [
        "text/json",
        "application/json; charset=latin1",
        "application/json; charset=utf-8; charset=utf-8",
        "application/json; unknown=true",
        "application/json; charset=\"utf-8",
        "application/json, text/plain",
    ] {
        let mut request = f.request(
            Method::POST,
            "/broker/v1/tickets",
            serde_json::to_vec(&f.ticket()?)?,
        )?;
        request
            .headers_mut()
            .insert("content-type", HeaderValue::from_str(media)?);
        assert_eq!(f.send(request).await?.0, StatusCode::BAD_REQUEST);
    }
    Ok(())
}
