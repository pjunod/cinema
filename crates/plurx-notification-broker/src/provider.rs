//! Fixed provider requests; transport injection never changes production hosts.
use crate::{
    config::{AppleEnvironment, Publisher},
    store::Attempt,
    wire::Platform,
    Error, Result,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{rand::SystemRandom, signature};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

const VISIBLE_BODY: &str = "A paired screen is ready";
const CATEGORY: &str = "CINEMA_REMOTE_INVITATION";
pub fn now() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::unavailable())?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::unavailable())
}
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, Zeroizing<String>)>,
    pub body: Zeroizing<Vec<u8>>,
    pub limit: usize,
    pub http2: bool,
}
pub struct Response {
    pub status: u16,
    pub body: Zeroizing<Vec<u8>>,
}
#[async_trait]
pub trait Transport: Send + Sync {
    async fn request(&self, request: Request) -> Result<Response>;
}
pub struct TlsTransport {
    client: reqwest::Client,
}
impl TlsTransport {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .connect_timeout(Duration::from_secs(3))
                .pool_max_idle_per_host(4)
                .build()
                .map_err(|_| Error::unavailable())?,
        })
    }
}
#[async_trait]
impl Transport for TlsTransport {
    async fn request(&self, request: Request) -> Result<Response> {
        let mut builder = self.client.post(&request.url);
        if request.http2 {
            builder = builder.version(reqwest::Version::HTTP_2);
        }
        for (name, value) in &request.headers {
            let mut header =
                reqwest::header::HeaderValue::from_str(value).map_err(|_| Error::unavailable())?;
            header.set_sensitive(true);
            builder = builder.header(name, header);
        }
        let mut response = builder
            .body(request.body.to_vec())
            .send()
            .await
            .map_err(|_| Error::unavailable())?;
        if request.http2 && response.version() != reqwest::Version::HTTP_2 {
            return Err(Error::unavailable());
        }
        if response
            .content_length()
            .is_some_and(|size| size > request.limit as u64)
        {
            return Err(Error::unavailable());
        }
        let status = response.status().as_u16();
        let mut body = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::unavailable())? {
            if chunk.len() > request.limit.saturating_sub(body.len()) {
                return Err(Error::unavailable());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response { status, body })
    }
}
struct Cached {
    issued: i64,
    expires: i64,
    token: Zeroizing<String>,
}
struct AppleSigner {
    key: signature::EcdsaKeyPair,
    cache: Mutex<Option<Cached>>,
}
struct AndroidSigner {
    key: signature::RsaKeyPair,
    cache: Mutex<Option<Cached>>,
}
pub struct Provider {
    publisher: Publisher,
    apple: Option<AppleSigner>,
    android: Option<AndroidSigner>,
    transport: Arc<dyn Transport>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: &'static str,
    pub invalid_token: bool,
}
impl Outcome {
    fn new(status: &'static str) -> Self {
        Self {
            status,
            invalid_token: false,
        }
    }
}
fn jwt_parts(header: Value, claims: Value) -> Result<String> {
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).map_err(|_| Error::unavailable())?),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).map_err(|_| Error::unavailable())?)
    ))
}
fn cached(cache: &Mutex<Option<Cached>>, now: i64) -> Result<Option<Zeroizing<String>>> {
    let guard = cache.lock().map_err(|_| Error::unavailable())?;
    if let Some(value) = guard.as_ref() {
        if now < value.issued {
            return Err(Error::unavailable());
        }
        if now < value.expires {
            return Ok(Some(value.token.clone()));
        }
    }
    Ok(None)
}
fn save(
    cache: &Mutex<Option<Cached>>,
    issued: i64,
    expires: i64,
    token: &Zeroizing<String>,
) -> Result<()> {
    *cache.lock().map_err(|_| Error::unavailable())? = Some(Cached {
        issued,
        expires,
        token: token.clone(),
    });
    Ok(())
}
fn headers(token: Zeroizing<String>, content_type: &str) -> Vec<(String, Zeroizing<String>)> {
    vec![
        (
            "authorization".into(),
            Zeroizing::new(format!("Bearer {}", token.as_str())),
        ),
        ("content-type".into(), Zeroizing::new(content_type.into())),
    ]
}
impl Provider {
    pub fn load(publisher: Publisher, transport: Arc<dyn Transport>) -> Result<Self> {
        let apple = publisher
            .apple
            .as_ref()
            .map(|value| {
                let bytes = &value.key_der;
                let key = signature::EcdsaKeyPair::from_pkcs8(
                    &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                    bytes,
                    &SystemRandom::new(),
                )
                .map_err(|_| Error::unavailable())?;
                Ok(AppleSigner {
                    key,
                    cache: Mutex::new(None),
                })
            })
            .transpose()?;
        let android = publisher
            .android
            .as_ref()
            .map(|value| {
                let bytes = &value.key_der;
                let key =
                    signature::RsaKeyPair::from_pkcs8(bytes).map_err(|_| Error::unavailable())?;
                Ok(AndroidSigner {
                    key,
                    cache: Mutex::new(None),
                })
            })
            .transpose()?;
        Ok(Self {
            publisher,
            apple,
            android,
            transport,
        })
    }
    async fn authorization(&self, platform: Platform, now: i64) -> Result<Zeroizing<String>> {
        match platform {
            Platform::Apple => {
                let config = self
                    .publisher
                    .apple
                    .as_ref()
                    .ok_or_else(Error::unavailable)?;
                let signer = self.apple.as_ref().ok_or_else(Error::unavailable)?;
                if let Some(token) = cached(&signer.cache, now)? {
                    return Ok(token);
                }
                let message = jwt_parts(
                    json!({"alg":"ES256","kid":config.key_id}),
                    json!({"iss":config.team_id,"iat":now}),
                )?;
                let signature = signer
                    .key
                    .sign(&SystemRandom::new(), message.as_bytes())
                    .map_err(|_| Error::unavailable())?;
                let token = Zeroizing::new(format!(
                    "{}.{}",
                    message,
                    URL_SAFE_NO_PAD.encode(signature.as_ref())
                ));
                save(&signer.cache, now, now + 1200, &token)?;
                Ok(token)
            }
            Platform::Android => {
                let config = self
                    .publisher
                    .android
                    .as_ref()
                    .ok_or_else(Error::unavailable)?;
                let signer = self.android.as_ref().ok_or_else(Error::unavailable)?;
                if let Some(token) = cached(&signer.cache, now)? {
                    return Ok(token);
                }
                let message = jwt_parts(
                    json!({"alg":"RS256","typ":"JWT"}),
                    json!({"iss":config.service_account_email,"scope":"https://www.googleapis.com/auth/firebase.messaging","aud":"https://oauth2.googleapis.com/token","iat":now,"exp":now+3600}),
                )?;
                let mut bytes = vec![0u8; signer.key.public().modulus_len()];
                signer
                    .key
                    .sign(
                        &signature::RSA_PKCS1_SHA256,
                        &SystemRandom::new(),
                        message.as_bytes(),
                        &mut bytes,
                    )
                    .map_err(|_| Error::unavailable())?;
                let assertion =
                    Zeroizing::new(format!("{}.{}", message, URL_SAFE_NO_PAD.encode(bytes)));
                // JWT alphabet needs no form escaping. The fixed grant value is encoded.
                let body=Zeroizing::new(format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={}",assertion.as_str()).into_bytes());
                let response = self
                    .transport
                    .request(Request {
                        url: "https://oauth2.googleapis.com/token".into(),
                        headers: vec![(
                            "content-type".into(),
                            Zeroizing::new("application/x-www-form-urlencoded".into()),
                        )],
                        body,
                        limit: 16 * 1024,
                        http2: false,
                    })
                    .await?;
                if response.status != 200 || response.body.len() > 16 * 1024 {
                    return Err(Error::unavailable());
                }
                #[derive(serde::Deserialize)]
                struct Token {
                    access_token: String,
                    token_type: String,
                    expires_in: u64,
                }
                let result: Token =
                    serde_json::from_slice(&response.body).map_err(|_| Error::unavailable())?;
                let token = Zeroizing::new(result.access_token);
                if result.token_type != "Bearer"
                    || token.is_empty()
                    || token.len() > 8192
                    || !token.bytes().all(|byte| byte.is_ascii_graphic())
                    || result.expires_in == 0
                    || result.expires_in > 3600
                {
                    return Err(Error::unavailable());
                }
                save(
                    &signer.cache,
                    now,
                    now + result.expires_in.saturating_sub(60) as i64,
                    &token,
                )?;
                Ok(token)
            }
        }
    }
    fn request(&self, attempt: &Attempt, token: Zeroizing<String>, now: i64) -> Result<Request> {
        let (url, mut headers, body, http2) = match attempt.platform {
            Platform::Apple => {
                let config = self
                    .publisher
                    .apple
                    .as_ref()
                    .ok_or_else(Error::unavailable)?;
                let host = match config.environment {
                    AppleEnvironment::Production => "api.push.apple.com",
                    AppleEnvironment::Sandbox => "api.sandbox.push.apple.com",
                };
                let mut values = headers(token, "application/json");
                for (name, value) in [
                    ("apns-topic", config.topic.clone()),
                    ("apns-push-type", "alert".into()),
                    ("apns-priority", "10".into()),
                    ("apns-expiration", attempt.delivery.expires_at.to_string()),
                ] {
                    values.push((name.into(), Zeroizing::new(value)));
                }
                (
                    format!(
                        "https://{}/3/device/{}",
                        host,
                        attempt.device_token.as_str()
                    ),
                    values,
                    json!({"aps":{"alert":{"body":VISIBLE_BODY},"category":CATEGORY},"invitation_id":attempt.delivery.invitation_id}),
                    true,
                )
            }
            Platform::Android => {
                let config = self
                    .publisher
                    .android
                    .as_ref()
                    .ok_or_else(Error::unavailable)?;
                (
                    format!(
                        "https://fcm.googleapis.com/v1/projects/{}/messages:send",
                        config.project_id
                    ),
                    headers(token, "application/json"),
                    json!({"message":{"token":attempt.device_token.as_str(),"data":{"invitation_id":attempt.delivery.invitation_id,"category":CATEGORY},"android":{"priority":"HIGH","ttl":format!("{}s",attempt.delivery.expires_at-now)}}}),
                    false,
                )
            }
        };
        headers.shrink_to_fit();
        Ok(Request {
            url,
            headers,
            body: Zeroizing::new(serde_json::to_vec(&body).map_err(|_| Error::unavailable())?),
            limit: 64 * 1024,
            http2,
        })
    }
    pub async fn deliver<F>(&self, attempt: &Attempt, eligible: F) -> Outcome
    where
        F: FnOnce(i64) -> Result<bool> + Send,
    {
        let operation = async {
            let token = self.authorization(attempt.platform, now()?).await?;
            let current = now()?;
            // No await between this durable check and starting the visible
            // request. Revocation during OAuth/admission consumes without send.
            if !eligible(current)? {
                return Ok(Outcome::new("denied"));
            }
            let request = self.request(attempt, token, current)?;
            let response = self.transport.request(request).await?;
            if response.body.len() > 64 * 1024 {
                return Err(Error::unavailable());
            }
            Ok(classify(attempt.platform, response))
        };
        match tokio::time::timeout(Duration::from_secs(5), operation).await {
            Ok(Ok(outcome)) => outcome,
            _ => Outcome::new("unknown"),
        }
    }
}
fn classify(platform: Platform, response: Response) -> Outcome {
    if response.status == 200 {
        return match platform {
            Platform::Apple if response.body.is_empty() => Outcome::new("accepted"),
            Platform::Android
                if serde_json::from_slice::<Value>(&response.body)
                    .ok()
                    .and_then(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
                    .is_some_and(|name| name.starts_with("projects/") && name.len() <= 4096) =>
            {
                Outcome::new("accepted")
            }
            _ => Outcome::new("unknown"),
        };
    }
    let value = serde_json::from_slice::<Value>(&response.body).ok();
    let invalid = match platform {
        Platform::Apple => value
            .as_ref()
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .is_some_and(|reason| {
                matches!(
                    reason,
                    "BadDeviceToken" | "Unregistered" | "DeviceTokenNotForTopic"
                )
            }),
        Platform::Android => value
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|value| value.get("details"))
            .and_then(Value::as_array)
            .is_some_and(|details| {
                details.iter().any(|detail| {
                    detail.get("@type").and_then(Value::as_str)
                        == Some("type.googleapis.com/google.firebase.fcm.v1.FcmError")
                        && detail.get("errorCode").and_then(Value::as_str) == Some("UNREGISTERED")
                })
            }),
    };
    Outcome {
        status: if response.status >= 400 && response.status < 500 {
            "denied"
        } else {
            "unknown"
        },
        invalid_token: invalid,
    }
}
#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
