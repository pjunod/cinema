use super::*;
use crate::{
    config::{Android, Apple},
    wire::{Delivery, VERSION},
};
use ring::signature::KeyPair;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
struct Fake {
    requests: Mutex<Vec<Request>>,
    responses: Mutex<VecDeque<Response>>,
    after_oauth: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
#[async_trait]
impl Transport for Fake {
    async fn request(&self, request: Request) -> Result<Response> {
        let oauth = request.url == "https://oauth2.googleapis.com/token";
        self.requests
            .lock()
            .map_err(|_| Error::unavailable())?
            .push(request);
        if oauth {
            if let Some(callback) = self
                .after_oauth
                .lock()
                .map_err(|_| Error::unavailable())?
                .take()
            {
                callback();
            }
        }
        self.responses
            .lock()
            .map_err(|_| Error::unavailable())?
            .pop_front()
            .ok_or_else(Error::unavailable)
    }
}
fn response(status: u16, body: &str) -> Response {
    Response {
        status,
        body: Zeroizing::new(body.as_bytes().to_vec()),
    }
}
fn fake(responses: Vec<Response>) -> Arc<Fake> {
    Arc::new(Fake {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(responses.into()),
        after_oauth: Mutex::new(None),
    })
}
fn publisher(platform: Platform) -> std::result::Result<Publisher, Box<dyn std::error::Error>> {
    let mut value = Publisher {
        publisher_id: uuid::Uuid::new_v4().to_string(),
        server_instance_id: "synthetic-home".into(),
        proof_hash: "11".repeat(32),
        apple: None,
        android: None,
    };
    match platform {
        Platform::Apple => {
            let der = signature::EcdsaKeyPair::generate_pkcs8(
                &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                &SystemRandom::new(),
            )
            .map_err(|_| "generate synthetic key")?;
            value.apple = Some(Apple {
                team_id: "TEAM123456".into(),
                key_id: "KEY1234567".into(),
                topic: "tv.plurx.synthetic".into(),
                environment: AppleEnvironment::Sandbox,
                private_key_file: "unused-injected.pk8".into(),
                key_der: Zeroizing::new(der.as_ref().to_vec()),
            });
        }
        Platform::Android => {
            value.android = Some(Android {
                project_id: "synthetic-project".into(),
                service_account_email: "synthetic@example.invalid".into(),
                private_key_file: "unused-injected.pk8".into(),
                key_der: Zeroizing::new(
                    include_bytes!("../tests/fixtures/synthetic-rsa.pk8").to_vec(),
                ),
            })
        }
    };
    Ok(value)
}
fn attempt(platform: Platform) -> Attempt {
    let installation = uuid::Uuid::new_v4();
    let mut invitation = installation.as_bytes().to_vec();
    invitation.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    Attempt {
        publisher: uuid::Uuid::new_v4().to_string(),
        enrollment: uuid::Uuid::new_v4().to_string(),
        platform,
        device_token: Zeroizing::new("aabb".into()),
        delivery: Delivery {
            version: VERSION.into(),
            enrollment_id: uuid::Uuid::new_v4().to_string(),
            installation_id: installation.to_string(),
            phone_generation: 1,
            consent_generation: 1,
            transport_generation: 1,
            invitation_id: URL_SAFE_NO_PAD.encode(invitation),
            expires_at: now().expect("synthetic clock") + 120,
        },
    }
}
#[tokio::test]
async fn apns_fixed_host_payload_es256_and_cached_authorization() -> TestResult {
    let config = publisher(Platform::Apple)?;
    let key = signature::EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &config.apple.as_ref().ok_or("apple")?.key_der,
        &SystemRandom::new(),
    )
    .map_err(|_| "key")?;
    let transport = fake(vec![response(200, ""), response(200, "")]);
    let provider = Provider::load(config, transport.clone())?;
    let attempt = attempt(Platform::Apple);
    assert_eq!(
        provider.deliver(&attempt, |_| Ok(true)).await.status,
        "accepted"
    );
    assert_eq!(
        provider.deliver(&attempt, |_| Ok(true)).await.status,
        "accepted"
    );
    let requests = transport.requests.lock().map_err(|_| "lock")?;
    let request = &requests[0];
    assert_eq!(
        request.url,
        "https://api.sandbox.push.apple.com/3/device/aabb"
    );
    assert!(request.http2);
    let body: Value = serde_json::from_slice(&request.body)?;
    assert_eq!(
        body,
        json!({"aps":{"alert":{"body":VISIBLE_BODY},"category":CATEGORY},"invitation_id":attempt.delivery.invitation_id})
    );
    let auth = &request
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .ok_or("authorization")?
        .1;
    let token = auth.strip_prefix("Bearer ").ok_or("prefix")?;
    let pieces = token.split('.').collect::<Vec<_>>();
    assert_eq!(pieces.len(), 3);
    let signature = URL_SAFE_NO_PAD.decode(pieces[2])?;
    assert_eq!(signature.len(), 64);
    signature::UnparsedPublicKey::new(
        &signature::ECDSA_P256_SHA256_FIXED,
        key.public_key().as_ref(),
    )
    .verify(
        format!("{}.{}", pieces[0], pieces[1]).as_bytes(),
        &signature,
    )
    .map_err(|_| "signature")?;
    assert_eq!(
        auth,
        &requests[1]
            .headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .ok_or("authorization")?
            .1
    );
    Ok(())
}
#[tokio::test]
async fn fcm_rs256_oauth_fixed_host_data_only_and_bounded_ttl() -> TestResult {
    let config = publisher(Platform::Android)?;
    let key = signature::RsaKeyPair::from_pkcs8(&config.android.as_ref().ok_or("android")?.key_der)
        .map_err(|_| "key")?;
    let transport = fake(vec![
        response(
            200,
            r#"{"access_token":"synthetic-access","token_type":"Bearer","expires_in":3600}"#,
        ),
        response(
            200,
            r#"{"name":"projects/synthetic-project/messages/synthetic"}"#,
        ),
    ]);
    let provider = Provider::load(config, transport.clone())?;
    let attempt = attempt(Platform::Android);
    assert_eq!(
        provider.deliver(&attempt, |_| Ok(true)).await.status,
        "accepted"
    );
    let requests = transport.requests.lock().map_err(|_| "lock")?;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, "https://oauth2.googleapis.com/token");
    let form = std::str::from_utf8(&requests[0].body)?;
    let assertion = form.split("&assertion=").nth(1).ok_or("assertion")?;
    let pieces = assertion.split('.').collect::<Vec<_>>();
    let signature = URL_SAFE_NO_PAD.decode(pieces[2])?;
    signature::UnparsedPublicKey::new(
        &signature::RSA_PKCS1_2048_8192_SHA256,
        key.public_key().as_ref(),
    )
    .verify(
        format!("{}.{}", pieces[0], pieces[1]).as_bytes(),
        &signature,
    )
    .map_err(|_| "signature")?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(pieces[1])?)?;
    assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/firebase.messaging"
    );
    let request = &requests[1];
    assert_eq!(
        request.url,
        "https://fcm.googleapis.com/v1/projects/synthetic-project/messages:send"
    );
    let body: Value = serde_json::from_slice(&request.body)?;
    assert!(body["message"].get("notification").is_none());
    assert_eq!(
        body["message"]["data"],
        json!({"invitation_id":attempt.delivery.invitation_id,"category":CATEGORY})
    );
    assert_eq!(body["message"]["android"]["priority"], "HIGH");
    let ttl = body["message"]["android"]["ttl"]
        .as_str()
        .ok_or("ttl")?
        .trim_end_matches('s')
        .parse::<u64>()?;
    assert!((1..=120).contains(&ttl));
    Ok(())
}
#[tokio::test]
async fn revoke_during_oauth_denies_before_visible_send() -> TestResult {
    let transport = fake(vec![response(
        200,
        r#"{"access_token":"synthetic-access","token_type":"Bearer","expires_in":3600}"#,
    )]);
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let changed = alive.clone();
    *transport.after_oauth.lock().map_err(|_| "lock")? = Some(Box::new(move || {
        changed.store(false, std::sync::atomic::Ordering::SeqCst)
    }));
    let provider = Provider::load(publisher(Platform::Android)?, transport.clone())?;
    let outcome = provider
        .deliver(&attempt(Platform::Android), |_| {
            Ok(alive.load(std::sync::atomic::Ordering::SeqCst))
        })
        .await;
    assert_eq!(outcome.status, "denied");
    assert_eq!(transport.requests.lock().map_err(|_| "lock")?.len(), 1);
    Ok(())
}
#[test]
fn bounded_provider_results_and_invalid_tokens_are_honest() {
    assert_eq!(
        classify(
            Platform::Apple,
            response(410, r#"{"reason":"Unregistered"}"#)
        ),
        Outcome {
            status: "denied",
            invalid_token: true
        }
    );
    assert_eq!(
        classify(
            Platform::Android,
            response(
                404,
                r#"{"error":{"details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}}"#
            )
        ),
        Outcome {
            status: "denied",
            invalid_token: true
        }
    );
    assert_eq!(
        classify(Platform::Android, response(200, "bad JSON")).status,
        "unknown"
    );
    assert_eq!(
        classify(Platform::Apple, response(503, "temporary")).status,
        "unknown"
    );
}

struct Delayed {
    count: AtomicUsize,
}
#[async_trait]
impl Transport for Delayed {
    async fn request(&self, _request: Request) -> Result<Response> {
        let count = self.count.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(3)).await;
        Ok(if count == 0 {
            response(
                200,
                r#"{"access_token":"synthetic-access","token_type":"Bearer","expires_in":3600}"#,
            )
        } else {
            response(200, r#"{"name":"projects/synthetic/messages/synthetic"}"#)
        })
    }
}
#[tokio::test]
async fn overall_deadline_includes_oauth_and_send_and_oversize_is_unknown() -> TestResult {
    let transport = Arc::new(Delayed {
        count: AtomicUsize::new(0),
    });
    let provider = Provider::load(publisher(Platform::Android)?, transport.clone())?;
    let start = tokio::time::Instant::now();
    assert_eq!(
        provider
            .deliver(&attempt(Platform::Android), |_| Ok(true))
            .await
            .status,
        "unknown"
    );
    assert!(start.elapsed() < Duration::from_millis(5600));
    assert_eq!(transport.count.load(Ordering::SeqCst), 2);
    let transport = fake(vec![Response {
        status: 200,
        body: Zeroizing::new(vec![b' '; 16 * 1024 + 1]),
    }]);
    let provider = Provider::load(publisher(Platform::Android)?, transport.clone())?;
    assert_eq!(
        provider
            .deliver(&attempt(Platform::Android), |_| Ok(true))
            .await
            .status,
        "unknown"
    );
    assert_eq!(transport.requests.lock().map_err(|_| "lock")?.len(), 1);
    let transport = fake(vec![Response {
        status: 200,
        body: Zeroizing::new(vec![b' '; 64 * 1024 + 1]),
    }]);
    let provider = Provider::load(publisher(Platform::Apple)?, transport)?;
    assert_eq!(
        provider
            .deliver(&attempt(Platform::Apple), |_| Ok(true))
            .await
            .status,
        "unknown"
    );
    Ok(())
}

#[tokio::test]
async fn durable_revoke_in_oauth_other_connection_fences_provider_effect() -> TestResult {
    use crate::store::{Admission, Store};
    use crate::wire::Ticket;
    let config = publisher(Platform::Android)?;
    let record = config.record();
    let generation = uuid::Uuid::new_v4().to_string();
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("broker.sqlite");
    let current = now()?;
    Store::initialize(
        &path,
        &generation,
        &[7; 32],
        crate::config::STORAGE_REALM,
        std::slice::from_ref(&record),
        current,
    )?;
    let mut store = Store::open(&path, &generation, &[7; 32], crate::config::STORAGE_REALM)?;
    let ticket = Ticket {
        version: VERSION.into(),
        ticket_id: uuid::Uuid::new_v4().to_string(),
        ticket_secret_hash: "22".repeat(32),
        server_instance_id: record.server.clone(),
        installation_id: uuid::Uuid::new_v4().to_string(),
        receiver_id: uuid::Uuid::new_v4().to_string(),
        consent_id: uuid::Uuid::new_v4().to_string(),
        phone_generation: 1,
        consent_generation: 1,
        transport_generation: 1,
        platform: Platform::Android,
    };
    store.issue(&record.id, &ticket, current)?;
    store.claim(
        &ticket.ticket_id,
        &[0x22; 32],
        Platform::Android,
        "synthetic-device-token",
        std::slice::from_ref(&record.id),
        current,
    )?;
    let mut attempt = attempt(Platform::Android);
    attempt.delivery.enrollment_id = ticket.ticket_id.clone();
    attempt.delivery.installation_id = ticket.installation_id.clone();
    let mut opaque = uuid::Uuid::parse_str(&ticket.installation_id)?
        .as_bytes()
        .to_vec();
    opaque.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    attempt.delivery.invitation_id = URL_SAFE_NO_PAD.encode(opaque);
    let Admission::Attempt(attempt) = store.admit(&record.id, &attempt.delivery, current)? else {
        return Err("attempt".into());
    };
    let transport = fake(vec![response(
        200,
        r#"{"access_token":"synthetic-access","token_type":"Bearer","expires_in":3600}"#,
    )]);
    let mut revoker = Store::open(&path, &generation, &[7; 32], crate::config::STORAGE_REALM)?;
    let publisher = record.id.clone();
    let id = ticket.ticket_id.clone();
    *transport.after_oauth.lock().map_err(|_| "lock")? = Some(Box::new(move || {
        revoker
            .revoke(&publisher, &id, now().expect("clock"))
            .expect("durable revoke");
    }));
    let provider = Provider::load(config, transport.clone())?;
    let outcome = provider
        .deliver(&attempt, |current| {
            store.eligible(&record, &attempt.delivery, current)
        })
        .await;
    assert_eq!(outcome.status, "denied");
    assert_eq!(transport.requests.lock().map_err(|_| "lock")?.len(), 1);
    store.finish(
        &record.id,
        &attempt.delivery.invitation_id,
        outcome.status,
        false,
        &ticket.ticket_id,
        now()?,
    )?;
    assert_eq!(
        store.status(&record.id, &ticket.ticket_id, now()?)?.status,
        "revoked"
    );
    let database = rusqlite::Connection::open(&path)?;
    assert_eq!(
        database.query_row("SELECT state FROM deliveries", [], |row| row
            .get::<_, String>(0))?,
        "denied"
    );
    Ok(())
}
