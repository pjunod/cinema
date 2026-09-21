//! Login, logout, and current-user endpoints.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use plurx_core::auth;
use serde::{Deserialize, Serialize};

use super::dto::UserDto;
use super::error::ApiError;
use super::extract::{AuthUser, RawToken};
use super::internal_auth_revocation::ClusterCacheRevocation;
use crate::state::AppState;

/// Human-entered secrets are small. The byte cap keeps every creation and
/// verification route on one contract and bounds request memory without
/// silently changing any existing stored hash.
pub(crate) const MAX_PASSWORD_BYTES: usize = 1024;
const PASSWORD_HASH_WORKERS: usize = 2;
const PASSWORD_HASH_WAITERS: usize = 16;
const PASSWORD_HASH_ADMISSION_WAIT: Duration = Duration::from_secs(2);
const LOGIN_THROTTLE_ENTRIES: usize = 4_096;
const LOGIN_THROTTLE_IDLE: Duration = Duration::from_secs(15 * 60);
const LOGIN_THROTTLE_WINDOW: Duration = Duration::from_secs(10 * 60);
const LOGIN_FREE_FAILURES: usize = 5;
const LOGIN_BACKOFF_CAP: Duration = Duration::from_secs(60);
const LOGIN_PROXY_OBSERVATION_WINDOW: Duration = Duration::from_secs(60 * 60);
const LOGIN_PROXY_OBSERVATIONS: usize = 4_096;

static LOGIN_OK: AtomicU64 = AtomicU64::new(0);
static LOGIN_BAD_CREDENTIALS: AtomicU64 = AtomicU64::new(0);
static LOGIN_BACKOFF: AtomicU64 = AtomicU64::new(0);
static LOGIN_CAPACITY: AtomicU64 = AtomicU64::new(0);

static PASSWORD_ACTIVE: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(PASSWORD_HASH_WORKERS)));
static PASSWORD_WAITERS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(PASSWORD_HASH_WAITERS)));

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub device: Option<String>,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserDto,
}

pub(super) struct ClientPeer(Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for ClientPeer {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(peer)| *peer),
        ))
    }
}

#[derive(Clone, Default)]
pub(crate) struct LoginThrottle {
    inner: Arc<Mutex<LoginThrottleState>>,
}

#[derive(Default)]
struct LoginThrottleState {
    pair: HashMap<(String, IpAddr), LoginFailures>,
    address: HashMap<IpAddr, LoginFailures>,
    sequence: u64,
    proxy_observations: VecDeque<LoginProxyObservation>,
}

struct LoginProxyObservation {
    at: tokio::time::Instant,
    address: IpAddr,
    forwarded: bool,
}

#[derive(Default)]
struct LoginFailures {
    failures: VecDeque<tokio::time::Instant>,
    last_used: u64,
}

impl LoginThrottle {
    fn check(&self, username: &str, address: IpAddr) -> Result<(), u64> {
        let now = tokio::time::Instant::now();
        let username = username_key(username);
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune(now);
        state.sequence = state.sequence.wrapping_add(1);
        let sequence = state.sequence;
        let pair_wait = state
            .pair
            .get_mut(&(username, address))
            .map(|entry| entry.retry_after(now, sequence))
            .unwrap_or_default();
        let address_wait = state
            .address
            .get_mut(&address)
            .map(|entry| entry.retry_after(now, sequence))
            .unwrap_or_default();
        let retry_after = pair_wait.max(address_wait);
        if retry_after == 0 {
            Ok(())
        } else {
            Err(retry_after)
        }
    }

    fn record_failure(&self, username: &str, address: IpAddr) {
        let now = tokio::time::Instant::now();
        let username = username_key(username);
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune(now);
        state.sequence = state.sequence.wrapping_add(1);
        let sequence = state.sequence;
        LoginThrottleState::make_room(&mut state.pair, &(username.clone(), address));
        state
            .pair
            .entry((username, address))
            .or_default()
            .record(now, sequence);
        LoginThrottleState::make_room(&mut state.address, &address);
        state
            .address
            .entry(address)
            .or_default()
            .record(now, sequence);
    }

    fn record_success(&self, username: &str, address: IpAddr) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pair.remove(&(username_key(username), address));
        // Deliberately retain the address-wide bucket. Clearing it with any
        // valid account would let a guesser reset distributed-username
        // protection using an account they control.
    }

    fn observe_proxy_shape(&self, address: IpAddr, forwarded: bool) {
        let now = tokio::time::Instant::now();
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_proxy_observations(now);
        if state.proxy_observations.len() == LOGIN_PROXY_OBSERVATIONS {
            state.proxy_observations.pop_front();
        }
        state.proxy_observations.push_back(LoginProxyObservation {
            at: now,
            address,
            forwarded,
        });
    }

    pub(crate) fn unconfigured_proxy_advisory(&self) -> bool {
        let now = tokio::time::Instant::now();
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_proxy_observations(now);
        let total = state.proxy_observations.len();
        if total == 0 {
            return false;
        }
        let mut by_address = HashMap::<IpAddr, (usize, bool)>::new();
        for observation in &state.proxy_observations {
            let entry = by_address.entry(observation.address).or_default();
            entry.0 += 1;
            entry.1 |= observation.forwarded;
        }
        by_address
            .values()
            .any(|(count, forwarded)| *forwarded && count.saturating_mul(2) > total)
    }
}

impl LoginThrottleState {
    fn prune(&mut self, now: tokio::time::Instant) {
        self.pair.retain(|_, entry| entry.active(now));
        self.address.retain(|_, entry| entry.active(now));
    }

    fn prune_proxy_observations(&mut self, now: tokio::time::Instant) {
        while self.proxy_observations.front().is_some_and(|observation| {
            now.saturating_duration_since(observation.at) >= LOGIN_PROXY_OBSERVATION_WINDOW
        }) {
            self.proxy_observations.pop_front();
        }
    }

    fn make_room<K: std::hash::Hash + Eq + Clone>(map: &mut HashMap<K, LoginFailures>, key: &K) {
        if map.len() < LOGIN_THROTTLE_ENTRIES || map.contains_key(key) {
            return;
        }
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
        {
            map.remove(&oldest);
        }
    }
}

impl LoginFailures {
    fn active(&mut self, now: tokio::time::Instant) -> bool {
        self.prune_window(now);
        self.failures
            .back()
            .is_some_and(|last| now.saturating_duration_since(*last) < LOGIN_THROTTLE_IDLE)
    }

    fn retry_after(&mut self, now: tokio::time::Instant, sequence: u64) -> u64 {
        self.prune_window(now);
        self.last_used = sequence;
        if self.failures.len() < LOGIN_FREE_FAILURES {
            return 0;
        }
        let exponent = (self.failures.len() - LOGIN_FREE_FAILURES).min(6) as u32;
        let backoff = Duration::from_secs((1_u64 << exponent).min(LOGIN_BACKOFF_CAP.as_secs()));
        let elapsed = self
            .failures
            .back()
            .map(|last| now.saturating_duration_since(*last))
            .unwrap_or(backoff);
        backoff
            .saturating_sub(elapsed)
            .as_secs()
            .max(u64::from(elapsed < backoff))
    }

    fn record(&mut self, now: tokio::time::Instant, sequence: u64) {
        self.prune_window(now);
        self.failures.push_back(now);
        self.last_used = sequence;
    }

    fn prune_window(&mut self, now: tokio::time::Instant) {
        while self
            .failures
            .front()
            .is_some_and(|seen| now.saturating_duration_since(*seen) >= LOGIN_THROTTLE_WINDOW)
        {
            self.failures.pop_front();
        }
    }
}

/// POST /api/v1/auth/login
pub async fn login(
    State(state): State<AppState>,
    ClientPeer(peer): ClientPeer,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    validate_password_size(&req.password)?;
    let address = client_ip(&headers, peer, &state.trusted_proxies);
    state
        .login_throttle
        .observe_proxy_shape(address, headers.contains_key("x-forwarded-for"));
    if let Err(retry_after_seconds) = state.login_throttle.check(&req.username, address) {
        LOGIN_BACKOFF.fetch_add(1, Ordering::Relaxed);
        return Err(ApiError::TypedRetry {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "login_backoff",
            message: "too many failed sign-in attempts from this address; retry later".into(),
            retry_after_seconds,
        });
    }
    let proof_ticket = state.cache_only_admin_proofs.authentication_ticket();
    let user = state.store.get_user_by_username(&req.username).await?;
    // Verify even on unknown user to keep timing uniform.
    let password = req.password;
    let (ok, user) = run_password_work(move || match user {
        Some(u) => {
            let ok = auth::verify_password(&password, &u.password_hash);
            (ok, Some(u))
        }
        None => {
            let _ = auth::verify_password(&password, &DUMMY_HASH);
            (false, None)
        }
    })
    .await?;
    let user = match (ok, user) {
        (true, Some(u)) => u,
        _ => {
            state.login_throttle.record_failure(&req.username, address);
            LOGIN_BAD_CREDENTIALS.fetch_add(1, Ordering::Relaxed);
            return Err(ApiError::Unauthorized);
        }
    };

    let token = auth::generate_token().map_err(|e| ApiError::Internal(e.to_string()))?;
    let hash = auth::hash_token(&token);
    state
        .store
        .create_token_if_password_matches(
            &hash,
            user.id,
            req.device.as_deref(),
            &user.password_hash,
        )
        .await?
        .then_some(())
        .ok_or_else(|| {
            state.login_throttle.record_failure(&req.username, address);
            LOGIN_BAD_CREDENTIALS.fetch_add(1, Ordering::Relaxed);
            ApiError::Unauthorized
        })?;
    state.login_throttle.record_success(&req.username, address);
    LOGIN_OK.fetch_add(1, Ordering::Relaxed);
    state
        .cache_only_admin_proofs
        .record_authenticated(proof_ticket, hash, &user);

    Ok(Json(LoginResponse {
        token,
        user: user.into(),
    }))
}

fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trusted: &[ipnet::IpNet]) -> IpAddr {
    let peer = peer
        .map(|address| address.ip())
        .unwrap_or(IpAddr::from([127, 0, 0, 1]));
    if !trusted.iter().any(|network| network.contains(&peer)) {
        return peer;
    }
    let Some(forwarded) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    else {
        return peer;
    };
    for raw in forwarded.split(',').rev() {
        let Some(address) = parse_forwarded_ip(raw.trim()) else {
            return peer;
        };
        if !trusted.iter().any(|network| network.contains(&address)) {
            return address;
        }
    }
    peer
}

fn parse_forwarded_ip(raw: &str) -> Option<IpAddr> {
    raw.parse::<IpAddr>()
        .ok()
        .or_else(|| raw.parse::<SocketAddr>().ok().map(|value| value.ip()))
}

fn username_key(username: &str) -> String {
    auth::hash_token(&username.to_lowercase())
}

pub(crate) fn prometheus_login_attempts() -> String {
    format!(
        "# HELP plurx_login_attempts_total Login attempts by bounded outcome.\n\
         # TYPE plurx_login_attempts_total counter\n\
         plurx_login_attempts_total{{outcome=\"ok\"}} {}\n\
         plurx_login_attempts_total{{outcome=\"bad_credentials\"}} {}\n\
         plurx_login_attempts_total{{outcome=\"backoff\"}} {}\n\
         plurx_login_attempts_total{{outcome=\"capacity\"}} {}\n",
        LOGIN_OK.load(Ordering::Relaxed),
        LOGIN_BAD_CREDENTIALS.load(Ordering::Relaxed),
        LOGIN_BACKOFF.load(Ordering::Relaxed),
        LOGIN_CAPACITY.load(Ordering::Relaxed),
    )
}

/// POST /api/v1/auth/logout — invalidate the presented token.
pub async fn logout(
    State(state): State<AppState>,
    _user: AuthUser,
    RawToken(token): RawToken,
) -> Result<Json<serde_json::Value>, ApiError> {
    let hash = auth::hash_token(&token);
    let proof_revocation = ClusterCacheRevocation::begin_digest(&state, &hash).await?;
    if !state
        .store
        .delete_token_with_cache_admin_claim(&hash, proof_revocation.mutation_claim())
        .await?
    {
        return Err(ApiError::ServiceUnavailable(
            "logout lost its cache-revocation exclusion; retry the request".into(),
        ));
    }
    proof_revocation.finish(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// GET /api/v1/me
pub async fn me(AuthUser(user): AuthUser) -> Json<UserDto> {
    Json(user.into())
}

pub(crate) fn validate_password_size(password: &str) -> Result<(), ApiError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::BadRequest(format!(
            "password must be at most {MAX_PASSWORD_BYTES} bytes"
        )));
    }
    Ok(())
}

pub(crate) fn validate_new_password(password: &str) -> Result<(), ApiError> {
    validate_password_size(password)?;
    if password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn hash_password_bounded(password: String) -> Result<String, ApiError> {
    run_password_work(move || auth::hash_password(&password))
        .await?
        .map_err(|error| ApiError::Internal(error.to_string()))
}

/// Admit both queued and active Argon2 work before leaving the async executor.
/// The active permit moves into the blocking closure, so cancellation of the
/// HTTP request cannot advertise capacity while its abandoned hash still runs.
async fn run_password_work<T, Work>(work: Work) -> Result<T, ApiError>
where
    T: Send + 'static,
    Work: FnOnce() -> T + Send + 'static,
{
    let _waiting = Arc::clone(&PASSWORD_WAITERS)
        .try_acquire_owned()
        .map_err(|_| password_capacity_error())?;
    let active = tokio::time::timeout(
        PASSWORD_HASH_ADMISSION_WAIT,
        Arc::clone(&PASSWORD_ACTIVE).acquire_owned(),
    )
    .await
    .map_err(|_| password_capacity_error())?
    .map_err(|_| password_capacity_error())?;
    tokio::task::spawn_blocking(move || {
        let _active = active;
        work()
    })
    .await
    .map_err(|error| ApiError::Internal(format!("password worker failed: {error}")))
}

fn password_capacity_error() -> ApiError {
    LOGIN_CAPACITY.fetch_add(1, Ordering::Relaxed);
    ApiError::ServiceUnavailable(
        "password verification capacity is busy; retry in a few seconds".into(),
    )
}

/// A real Argon2 hash (of a throwaway password), computed once, used to spend
/// the same verification time on unknown usernames (mitigates user enumeration
/// via login timing). Verifying against it always fails for real passwords.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    auth::hash_password("plurx-timing-placeholder")
        .unwrap_or_else(|_| "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned())
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_password_route_has_one_encoded_byte_ceiling() {
        assert!(validate_password_size(&"x".repeat(MAX_PASSWORD_BYTES)).is_ok());
        assert!(validate_password_size(&"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
        assert!(validate_new_password(&"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
        assert!(validate_password_size(&"é".repeat(MAX_PASSWORD_BYTES / 2 + 1)).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn five_failures_are_free_and_the_sixth_waits_without_extending() {
        let throttle = LoginThrottle::default();
        let address = IpAddr::from([192, 0, 2, 10]);
        for _ in 0..LOGIN_FREE_FAILURES {
            assert!(throttle.check("Paul", address).is_ok());
            throttle.record_failure("Paul", address);
        }
        assert_eq!(throttle.check("paul", address), Err(1));
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(throttle.check("paul", address), Err(1));
        tokio::time::advance(Duration::from_millis(500)).await;
        assert!(throttle.check("paul", address).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_caps_at_sixty_seconds_and_another_address_is_independent() {
        let throttle = LoginThrottle::default();
        let hostile = IpAddr::from([192, 0, 2, 20]);
        let legitimate = IpAddr::from([198, 51, 100, 20]);
        for _ in 0..20 {
            throttle.record_failure("paul", hostile);
        }
        assert_eq!(throttle.check("paul", hostile), Err(60));
        assert!(throttle.check("paul", legitimate).is_ok());
    }

    #[test]
    fn success_clears_the_pair_without_resetting_rotating_username_protection() {
        let throttle = LoginThrottle::default();
        let address = IpAddr::from([192, 0, 2, 30]);
        throttle.record_failure("paul", address);
        throttle.record_success("PAUL", address);
        let state = throttle.inner.lock().expect("throttle state");
        assert!(!state.pair.contains_key(&(username_key("paul"), address)));
        assert!(state.address.contains_key(&address));
    }

    #[test]
    fn throttle_maps_evict_to_the_hard_bound() {
        let throttle = LoginThrottle::default();
        for offset in 0..5_000_u32 {
            let address = IpAddr::from(offset.to_be_bytes());
            throttle.record_failure(&format!("user-{offset}"), address);
        }
        let state = throttle.inner.lock().expect("throttle state");
        assert_eq!(state.pair.len(), LOGIN_THROTTLE_ENTRIES);
        assert_eq!(state.address.len(), LOGIN_THROTTLE_ENTRIES);
    }

    #[test]
    fn forwarded_for_requires_a_trusted_peer_and_uses_rightmost_untrusted_hop() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.4, 10.1.2.3".parse().expect("header"),
        );
        let untrusted_peer: SocketAddr = "203.0.113.9:443".parse().expect("peer");
        let trusted_peer: SocketAddr = "10.9.8.7:443".parse().expect("peer");
        let proxies = vec!["10.0.0.0/8".parse().expect("network")];
        assert_eq!(
            client_ip(&headers, Some(untrusted_peer), &proxies),
            untrusted_peer.ip()
        );
        assert_eq!(
            client_ip(&headers, Some(trusted_peer), &proxies),
            "198.51.100.4".parse::<IpAddr>().expect("client")
        );
    }

    #[test]
    fn dominant_forwarded_address_sets_only_an_advisory() {
        let throttle = LoginThrottle::default();
        let proxy = IpAddr::from([192, 0, 2, 40]);
        let other = IpAddr::from([192, 0, 2, 41]);
        throttle.observe_proxy_shape(proxy, true);
        throttle.observe_proxy_shape(proxy, true);
        throttle.observe_proxy_shape(other, false);
        assert!(throttle.unconfigured_proxy_advisory());
        assert!(throttle.check("paul", proxy).is_ok());
    }
}
