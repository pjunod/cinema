//! Fixed-origin publisher transport. A generation mismatch never acknowledges cleanup.
use super::wire::Version;
use axum::http::HeaderMap;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use plurx_core::{auth, store::invitations::BrokerReference};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use uuid::Uuid;

const GENERATION: &str = "x-cinema-broker-generation";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    broker_origin: String,
    publisher_id: String,
    broker_generation: String,
    server_instance_id: String,
    publisher_secret_file: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Unconfigured,
    Denied,
    Unknown,
    Remediation,
    Busy,
}
#[derive(Clone)]
pub struct Client {
    origin: String,
    publisher: String,
    generation: String,
    instance: String,
    proof: String,
    scope_hash: String,
    http: reqwest::Client,
    slots: Arc<Semaphore>,
}
fn uuid(raw: &str) -> bool {
    Uuid::parse_str(raw).is_ok_and(|v| v.to_string() == raw)
}
fn generation(headers: &HeaderMap, expected: &str) -> Result<(), Failure> {
    let mut all = headers.get_all(GENERATION).iter();
    let value = all
        .next()
        .and_then(|v| v.to_str().ok())
        .ok_or(Failure::Remediation)?;
    if all.next().is_some() || !uuid(value) || value != expected {
        return Err(Failure::Remediation);
    }
    Ok(())
}
fn read(path: &Path, max: usize) -> Result<Vec<u8>, Failure> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|_| Failure::Unconfigured)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::Unconfigured)?;
    if bytes.len() > max {
        return Err(Failure::Unconfigured);
    }
    Ok(bytes)
}
impl Client {
    pub fn load() -> Result<Option<Self>, Failure> {
        let Some(path) = std::env::var_os("PLURX_INVITATION_PUBLISHER_CONFIG") else {
            return Ok(None);
        };
        let config: Config = serde_json::from_slice(&read(Path::new(&path), 64 * 1024)?)
            .map_err(|_| Failure::Unconfigured)?;
        Self::configured(config).map(Some)
    }
    fn configured(c: Config) -> Result<Self, Failure> {
        let url = reqwest::Url::parse(&c.broker_origin).map_err(|_| Failure::Unconfigured)?;
        if c.broker_origin.len() > 2048
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || c.broker_origin != url.origin().ascii_serialization()
            || !uuid(&c.publisher_id)
            || !uuid(&c.broker_generation)
            || c.server_instance_id.is_empty()
            || c.server_instance_id.len() > 128
            || c.server_instance_id.chars().any(char::is_control)
        {
            return Err(Failure::Unconfigured);
        }
        let bytes = read(Path::new(&c.publisher_secret_file), 128)?;
        let raw = std::str::from_utf8(&bytes)
            .map_err(|_| Failure::Unconfigured)?
            .strip_suffix('\n')
            .unwrap_or(std::str::from_utf8(&bytes).map_err(|_| Failure::Unconfigured)?);
        let decoded = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| Failure::Unconfigured)?;
        if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(decoded) != raw {
            return Err(Failure::Unconfigured);
        }
        let scope_hash = auth::hash_token(
            &serde_json::to_string(&(
                &c.broker_origin,
                &c.publisher_id,
                &c.server_instance_id,
                &c.broker_generation,
            ))
            .map_err(|_| Failure::Unconfigured)?,
        );
        let http = Self::builder().build().map_err(|_| Failure::Unconfigured)?;
        Ok(Self {
            origin: c.broker_origin,
            publisher: c.publisher_id,
            generation: c.broker_generation,
            instance: c.server_instance_id,
            proof: raw.to_owned(),
            scope_hash,
            http,
            slots: Arc::new(Semaphore::new(16)),
        })
    }
    fn builder() -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(4)
    }
    #[cfg(test)]
    pub(super) fn fixture(config: &[u8], root: &[u8]) -> Result<Self, Failure> {
        let mut client =
            Self::configured(serde_json::from_slice(config).map_err(|_| Failure::Unconfigured)?)?;
        let root = reqwest::Certificate::from_pem(root).map_err(|_| Failure::Unconfigured)?;
        client.http = Self::builder()
            .add_root_certificate(root)
            .build()
            .map_err(|_| Failure::Unconfigured)?;
        Ok(client)
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn generation(&self) -> &str {
        &self.generation
    }
    pub fn instance(&self) -> &str {
        &self.instance
    }
    pub fn scope(&self) -> &str {
        &self.scope_hash
    }
    pub fn reference(&self, id: String) -> BrokerReference {
        BrokerReference {
            ticket_id: id,
            scope_hash: self.scope_hash.clone(),
        }
    }
    pub fn matches(&self, r: &BrokerReference) -> bool {
        r.encode().is_ok() && r.scope_hash == self.scope_hash
    }
    async fn request<T: DeserializeOwned>(
        &self,
        actual_instance: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, Failure> {
        if actual_instance != self.instance {
            return Err(Failure::Remediation);
        }
        let _permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Busy)?;
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut request = self
                .http
                .request(method, format!("{}{path}", self.origin))
                .bearer_auth(&self.proof)
                .header("x-cinema-publisher-id", &self.publisher)
                .header(GENERATION, &self.generation);
            if let Some(body) = body {
                let bytes = serde_json::to_vec(body).map_err(|_| Failure::Unknown)?;
                if bytes.len() > 64 * 1024 {
                    return Err(Failure::Unknown);
                }
                request = request
                    .header("content-type", "application/json")
                    .body(bytes);
            }
            let mut reply = request.send().await.map_err(|_| Failure::Unknown)?;
            generation(reply.headers(), &self.generation)?;
            let status = reply.status();
            let mut bytes = Vec::new();
            while let Some(chunk) = reply.chunk().await.map_err(|_| Failure::Unknown)? {
                if bytes.len() + chunk.len() > 64 * 1024 {
                    return Err(Failure::Unknown);
                }
                bytes.extend_from_slice(&chunk);
            }
            if !status.is_success() {
                let error: Error = serde_json::from_slice(&bytes).map_err(|_| Failure::Unknown)?;
                let Version::V1 = error.version;
                if error.message.len() > 1024 || error.code.len() > 80 {
                    return Err(Failure::Unknown);
                }
                return Err(if error.code == "stale_broker_generation" {
                    Failure::Remediation
                } else if error.code == "provider_unconfigured" {
                    Failure::Unconfigured
                } else {
                    Failure::Denied
                });
            }
            serde_json::from_slice(&bytes).map_err(|_| Failure::Unknown)
        })
        .await
        .map_err(|_| Failure::Unknown)?
    }
    pub async fn issue(
        &self,
        actual_instance: &str,
        body: &serde_json::Value,
    ) -> Result<Issued, Failure> {
        self.request(
            actual_instance,
            reqwest::Method::POST,
            "/broker/v1/tickets",
            Some(body),
        )
        .await
    }
    pub async fn status(
        &self,
        actual_instance: &str,
        r: &BrokerReference,
    ) -> Result<TicketStatus, Failure> {
        if !self.matches(r) {
            return Err(Failure::Remediation);
        }
        self.request(
            actual_instance,
            reqwest::Method::POST,
            "/broker/v1/tickets/status",
            Some(&serde_json::json!({"version":"cinema.invitation.v1","ticket_id":r.ticket_id})),
        )
        .await
    }
    pub async fn revoke(&self, actual_instance: &str, r: &BrokerReference) -> Result<(), Failure> {
        if !self.matches(r) {
            return Err(Failure::Remediation);
        }
        let reply: Revoked = self
            .request(
                actual_instance,
                reqwest::Method::DELETE,
                &format!("/broker/v1/enrollments/{}", r.ticket_id),
                None,
            )
            .await?;
        let Version::V1 = reply.version;
        if reply.status != "revoked" {
            return Err(Failure::Unknown);
        }
        Ok(())
    }
    pub async fn deliver(
        &self,
        actual_instance: &str,
        r: &BrokerReference,
        body: &serde_json::Value,
    ) -> Result<String, Failure> {
        if !self.matches(r) {
            return Err(Failure::Remediation);
        }
        let reply: Delivery = self
            .request(
                actual_instance,
                reqwest::Method::POST,
                "/broker/v1/deliveries",
                Some(body),
            )
            .await?;
        let Version::V1 = reply.version;
        if !matches!(
            reply.status.as_str(),
            "accepted" | "duplicate" | "denied" | "unknown"
        ) {
            return Err(Failure::Unknown);
        }
        Ok(reply.status)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Error {
    version: Version,
    code: String,
    message: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revoked {
    version: Version,
    status: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    version: Version,
    status: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issued {
    pub version: Version,
    #[serde(deserialize_with = "canonical_id")]
    pub ticket_id: String,
    #[serde(deserialize_with = "positive")]
    pub expires_at: i64,
    pub status: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TicketStatus {
    pub version: Version,
    #[serde(deserialize_with = "canonical_id")]
    pub ticket_id: String,
    pub status: String,
    #[serde(deserialize_with = "positive")]
    pub expires_at: i64,
    #[serde(deserialize_with = "optional_id")]
    pub enrollment_id: Option<String>,
    pub server_instance_id: String,
    #[serde(deserialize_with = "canonical_id")]
    pub installation_id: String,
    #[serde(deserialize_with = "canonical_id")]
    pub receiver_id: String,
    #[serde(deserialize_with = "canonical_id")]
    pub consent_id: String,
    #[serde(deserialize_with = "positive")]
    pub phone_generation: i64,
    #[serde(deserialize_with = "positive")]
    pub consent_generation: i64,
    #[serde(deserialize_with = "positive")]
    pub transport_generation: i64,
    pub platform: String,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_is_exact_and_required_before_cleanup_ack() {
        let expected = Uuid::new_v4().to_string();
        let newer = Uuid::new_v4().to_string();
        let mut h = HeaderMap::new();
        assert_eq!(generation(&h, &expected), Err(Failure::Remediation));
        h.insert(GENERATION, newer.parse().expect("header"));
        assert_eq!(generation(&h, &expected), Err(Failure::Remediation));
        h.insert(GENERATION, expected.parse().expect("header"));
        assert_eq!(generation(&h, &expected), Ok(()));
        h.append(GENERATION, expected.parse().expect("duplicate"));
        assert_eq!(generation(&h, &expected), Err(Failure::Remediation));
    }
}

fn positive<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let value = u64::deserialize(d)?;
    if !(1..=9_007_199_254_740_991).contains(&value) {
        return Err(serde::de::Error::custom("invalid integer"));
    }
    i64::try_from(value).map_err(serde::de::Error::custom)
}
fn canonical_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    if !uuid(&value) {
        return Err(serde::de::Error::custom("invalid identifier"));
    }
    Ok(value)
}
fn optional_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(d)?;
    if value.as_ref().is_some_and(|v| !uuid(v)) {
        return Err(serde::de::Error::custom("invalid identifier"));
    }
    Ok(value)
}

#[cfg(test)]
mod response_tests {
    use super::*;
    #[test]
    fn broker_capability_replies_reject_unsafe_lexemes_and_ambiguous_fields() {
        let id = Uuid::new_v4().to_string();
        let body=serde_json::json!({"version":"cinema.invitation.v1","ticket_id":id,"expires_at":120,"status":"pending"}).to_string();
        assert!(serde_json::from_str::<Issued>(&body).is_ok());
        for lexical in [
            "-0",
            "-1",
            "0",
            "1e0",
            "1.0",
            "9007199254740992",
            "18446744073709551616",
        ] {
            let invalid = body.replace("\"expires_at\":120", &format!("\"expires_at\":{lexical}"));
            assert!(
                serde_json::from_str::<Issued>(&invalid).is_err(),
                "{lexical}"
            );
        }
        let duplicate = body.replacen('{', "{\"expires_at\":120,", 1);
        assert!(serde_json::from_str::<Issued>(&duplicate).is_err());
        let extra = body.replacen('{', "{\"extra\":true,", 1);
        assert!(serde_json::from_str::<Issued>(&extra).is_err());
        assert!(
            serde_json::from_str::<Issued>(&body.replace(&id, "not-a-canonical-uuid")).is_err()
        );
    }
}
