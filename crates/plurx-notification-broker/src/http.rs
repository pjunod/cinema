//! Strict capability API; no account login, enumeration, or arbitrary pushes.
use crate::{
    config::Publisher,
    provider::{self, Provider, Transport},
    store::{Admission, Store},
    wire::{self, Claim, Delivery, Outcome, Status, Ticket},
    Error, Result,
};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

pub const GENERATION_HEADER: &str = "x-cinema-broker-generation";
const PUBLISHER_HEADER: &str = "x-cinema-publisher-id";
struct Authority {
    config: Publisher,
    provider: Provider,
    permits: Arc<Semaphore>,
}
pub struct Broker {
    generation: String,
    store: Mutex<Store>,
    publishers: HashMap<String, Authority>,
    permits: Arc<Semaphore>,
}
impl Broker {
    pub fn new(
        generation: String,
        mut store: Store,
        publishers: Vec<Publisher>,
        transport: Arc<dyn Transport>,
        now: i64,
    ) -> Result<Arc<Self>> {
        wire::uuid(&generation)?;
        let mut authorities = HashMap::new();
        for config in publishers {
            let provider = Provider::load(config.clone(), transport.clone())?;
            if authorities
                .insert(
                    config.publisher_id.clone(),
                    Authority {
                        config,
                        provider,
                        permits: Arc::new(Semaphore::new(4)),
                    },
                )
                .is_some()
            {
                return Err(Error::invalid());
            }
        }
        store.reconcile(
            &authorities
                .values()
                .map(|value| value.config.record())
                .collect::<Vec<_>>(),
            now,
        )?;
        Ok(Arc::new(Self {
            generation,
            store: Mutex::new(store),
            publishers: authorities,
            permits: Arc::new(Semaphore::new(16)),
        }))
    }
    pub fn router(self: Arc<Self>) -> Router {
        Router::new().fallback(handle).with_state(self)
    }
    fn store<T>(&self, operation: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        {
            let mut guard = self.store.lock().map_err(|_| Error::unavailable())?;
            operation(&mut guard)
        }
    }
    fn authority<'a>(&'a self, headers: &HeaderMap, now: i64) -> Result<&'a Authority> {
        let generation = single(headers, GENERATION_HEADER)?;
        wire::uuid(generation)?;
        if generation != self.generation {
            return Err(Error::new(409, "stale_broker_generation"));
        }
        for name in headers.keys() {
            if name.as_str().starts_with("x-cinema-")
                && ![GENERATION_HEADER, PUBLISHER_HEADER].contains(&name.as_str())
            {
                return Err(Error::new(401, "unauthorized"));
            }
        }
        let id = single(headers, PUBLISHER_HEADER)?;
        wire::uuid(id)?;
        let hash = proof(headers)?;
        let authority = self
            .publishers
            .get(id)
            .ok_or_else(|| Error::new(401, "unauthorized"))?;
        if !bool::from(wire::digest(&authority.config.proof_hash)?.ct_eq(&hash)) {
            return Err(Error::new(401, "unauthorized"));
        }
        self.store(|store| {
            store.authorized(&authority.config.record(), now)?;
            store.rate(id, now)
        })?;
        Ok(authority)
    }
    fn response<T: Serialize>(&self, result: Result<T>) -> Response {
        let mut response = match result {
            Ok(value) => axum::Json(value).into_response(),
            Err(error) => {
                #[derive(Serialize)]
                struct Failure {
                    version: &'static str,
                    code: &'static str,
                    message: &'static str,
                }
                (
                    StatusCode::from_u16(error.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                    axum::Json(Failure {
                        version: wire::VERSION,
                        code: error.code,
                        message: message(error.code),
                    }),
                )
                    .into_response()
            }
        };
        if let Ok(value) = HeaderValue::from_str(&self.generation) {
            response.headers_mut().insert(GENERATION_HEADER, value);
        }
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        response
    }
    async fn dispatch(&self, request: Request) -> Result<Response> {
        if request.uri().query().is_some() || request.headers().contains_key("x-api-key") {
            return Err(Error::new(401, "unauthorized"));
        }
        let path = request.uri().path().to_owned();
        let method = request.method().clone();
        let (parts, body) = request.into_parts();
        let now = provider::now()?;
        let claim = path == "/broker/v1/tickets/claim" && method == Method::POST;
        let authority = if claim {
            if parts
                .headers
                .keys()
                .any(|name| name.as_str().starts_with("x-cinema-"))
            {
                return Err(Error::new(401, "unauthorized"));
            }
            None
        } else {
            Some(self.authority(&parts.headers, now)?)
        };
        let hash = proof(&parts.headers)?;
        if method == Method::POST && !json_content_type(&parts.headers)? {
            return Err(Error::invalid());
        }
        let bytes = tokio::time::timeout(Duration::from_secs(5), to_bytes(body, wire::MAX_BODY))
            .await
            .map_err(|_| Error::new(408, "timeout"))?
            .map_err(|_| Error::new(413, "invalid"))?;
        if claim {
            let claim: Claim = wire::parse(&bytes)?;
            claim.validate()?;
            self.store(|store| {
                store.claim(
                    &claim.ticket_id,
                    &hash,
                    claim.platform,
                    &claim.device_token,
                    &self.publishers.keys().cloned().collect::<Vec<_>>(),
                    provider::now()?,
                )
            })?;
            return Ok(self.response(Ok(Outcome::new("claimed"))));
        }
        let authority = authority.ok_or_else(Error::unavailable)?;
        let publisher = &authority.config.publisher_id;
        let record = authority.config.record();
        // A body read may have stalled while another opener reconciled config.
        self.store(|store| store.authorized(&record, provider::now()?))?;
        match (method, path.as_str()) {
            (Method::POST, "/broker/v1/tickets") => {
                let ticket: Ticket = wire::parse(&bytes)?;
                ticket.validate()?;
                if ticket.server_instance_id != authority.config.server_instance_id {
                    return Err(Error::new(403, "scope_mismatch"));
                }
                if match ticket.platform {
                    wire::Platform::Apple => authority.config.apple.is_none(),
                    wire::Platform::Android => authority.config.android.is_none(),
                } {
                    return Err(Error::new(503, "provider_unconfigured"));
                }
                let issued =
                    self.store(|store| store.issue(publisher, &ticket, provider::now()?))?;
                Ok(self.response(Ok(issued)))
            }
            (Method::POST, "/broker/v1/tickets/status") => {
                let status: Status = wire::parse(&bytes)?;
                status.validate()?;
                let status = self
                    .store(|store| store.status(publisher, &status.ticket_id, provider::now()?))?;
                Ok(self.response(Ok(status)))
            }
            (Method::DELETE, path) if path.starts_with("/broker/v1/enrollments/") => {
                if !bytes.is_empty() {
                    return Err(Error::invalid());
                }
                let id = path.trim_start_matches("/broker/v1/enrollments/");
                wire::uuid(id)?;
                self.store(|store| store.revoke(publisher, id, provider::now()?))?;
                Ok(self.response(Ok(Outcome::new("revoked"))))
            }
            (Method::POST, "/broker/v1/deliveries") => {
                let delivery: Delivery = wire::parse(&bytes)?;
                delivery.validate(provider::now()?)?;
                // Immediate admission: no unbounded tasks waiting for permits.
                let _permit = authority
                    .permits
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| Error::new(429, "busy"))?;
                let admission =
                    self.store(|store| store.admit(publisher, &delivery, provider::now()?))?;
                let outcome = match admission {
                    Admission::Duplicate => Outcome::new("duplicate"),
                    Admission::Denied => Outcome::new("denied"),
                    Admission::Attempt(attempt) => {
                        let outcome = authority
                            .provider
                            .deliver(&attempt, |now| {
                                self.store(|store| store.eligible(&record, &delivery, now))
                            })
                            .await;
                        self.store(|store| {
                            store.finish(
                                publisher,
                                &delivery.invitation_id,
                                outcome.status,
                                outcome.invalid_token,
                                &delivery.enrollment_id,
                                provider::now()?,
                            )
                        })?;
                        Outcome::new(outcome.status)
                    }
                };
                Ok(self.response(Ok(outcome)))
            }
            _ => Err(Error::new(404, "not_found")),
        }
    }
}
fn single<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .ok_or_else(|| Error::new(401, "unauthorized"))?;
    if values.next().is_some() {
        return Err(Error::new(401, "unauthorized"));
    }
    value.to_str().map_err(|_| Error::new(401, "unauthorized"))
}
fn json_content_type(headers: &HeaderMap) -> Result<bool> {
    let value = single(headers, "content-type").map_err(|_| Error::invalid())?;
    if value.len() > 128 {
        return Ok(false);
    }
    let mut parts = value.split(';');
    if !parts
        .next()
        .is_some_and(|part| part.trim().eq_ignore_ascii_case("application/json"))
    {
        return Ok(false);
    }
    let valid = match parts.next() {
        None => true,
        Some(parameter) => parameter.split_once('=').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("charset")
                && (value.trim().eq_ignore_ascii_case("utf-8")
                    || value.trim().eq_ignore_ascii_case("\"utf-8\""))
        }),
    };
    Ok(valid && parts.next().is_none())
}
fn proof(headers: &HeaderMap) -> Result<[u8; 32]> {
    let value = single(headers, "authorization")?;
    let secret = value
        .strip_prefix("Bearer ")
        .ok_or_else(|| Error::new(401, "unauthorized"))?;
    wire::proof_hash(secret)
}
fn message(code: &str) -> &'static str {
    match code {
        "stale_broker_generation" => "Broker generation changed; reconnect explicitly.",
        "retention_limit" => "Durable identity capacity reached.",
        "provider_unconfigured" => "Provider is not configured.",
        "restore_fence_required" => "Operator restore fence is required.",
        "unauthorized" => "Capability authorization failed.",
        "busy" | "rate_limited" => "Admission is temporarily full.",
        "not_found" => "Capability was not found in this scope.",
        _ => "Request could not be completed.",
    }
}
async fn handle(State(broker): State<Arc<Broker>>, request: Request) -> Response {
    let Ok(_permit) = broker.permits.clone().try_acquire_owned() else {
        return broker.response::<Outcome>(Err(Error::new(429, "busy")));
    };
    match tokio::time::timeout(Duration::from_secs(11), broker.dispatch(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => broker.response::<Outcome>(Err(error)),
        Err(_) => broker.response::<Outcome>(Err(Error::new(408, "timeout"))),
    }
}
#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
