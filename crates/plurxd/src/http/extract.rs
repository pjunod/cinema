//! Auth extractors. Any handler taking [`AuthUser`] requires a valid token;
//! [`AdminUser`] additionally requires the admin flag.
//!
//! Tokens arrive either as `Authorization: Bearer <token>` (API clients) or as
//! a `?token=` query parameter — the latter because `<img>` and `<video>` tags
//! can't set headers, so image and stream URLs carry the token inline.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use plurx_core::auth;
use plurx_core::domain::ApiKey;
use plurx_core::domain::User;
use plurx_core::store::TokenAuthentication;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::error::ApiError;
use crate::state::AppState;

pub struct AuthUser(pub User);
/// Guard that requires the caller be an admin; carries the admin's identity.
pub struct AdminUser(pub User);

/// Store-free proof for the two cluster recovery reads.
///
/// A successful ordinary authentication publishes only the SHA-256 token
/// digest, the user id needed for explicit revocation, and a fixed expiry.
/// Cache-only reads never renew that expiry.
///
/// **The Store is never asked first.** A cached proof answers with no Store
/// contact at all, which is the property these two reads exist for: an
/// operator diagnosing a wedged cluster must still be able to load the page
/// that describes it, for as long as a proof lives.
///
/// What the cache must not do is answer *for* the Store when it has nothing to
/// say. A miss — no proof yet, or the whole cache fenced closed by the
/// revocation protocol — is this node's condition, not a verdict on the
/// caller's credential, so the guard falls back to a bounded Store read for
/// the same authority every other admin route already grants, and returns
/// [`ApiError::Forbidden`] for a real non-admin, `401` only for a credential
/// the Store does not know, and a named `503` when it cannot find out.
/// Answering `401` from the cache alone is what logged an administrator out of
/// a working session every time they opened Settings while the protocol
/// activation was stuck.
///
/// Two limits worth stating plainly rather than discovering during an
/// incident. The fallback is a linearizable read, so it cannot answer while
/// quorum is lost: past the five-minute proof TTL these reads degrade to the
/// named `503` in that incident rather than to data. And `user_for_token` also
/// touches the token's `last_seen_at`, so the bound can cancel a replicated
/// write mid-flight — that write is an idempotent activity stamp whose own
/// caller already tolerates losing it, and the ordinary [`AuthUser`] path
/// makes the same write with no bound at all.
pub struct CacheOnlyAdminUser;

const CACHE_ONLY_ADMIN_PROOF_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CACHE_ONLY_ADMIN_PROOFS: usize = 64;
/// Queued callers admitted behind the one active operation, process-wide.
pub(super) const REVOCATION_WAITERS: usize = 8;
pub(super) const REVOCATION_ADMISSION_WAIT: Duration = Duration::from_millis(2_500);

static REVOCATION_COMPLETE: AtomicU64 = AtomicU64::new(0);
static REVOCATION_QUEUE_FULL: AtomicU64 = AtomicU64::new(0);
static REVOCATION_ADMISSION_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static REVOCATION_PROPAGATION_FAILED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(crate) struct CacheOnlyAdminProofCache {
    inner: Arc<Mutex<CachedAdminProofState>>,
    revocation_operation_gate: Arc<tokio::sync::Mutex<()>>,
    revocation_waiters: Arc<tokio::sync::Semaphore>,
}

impl Default for CacheOnlyAdminProofCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CachedAdminProofState::default())),
            revocation_operation_gate: Arc::new(tokio::sync::Mutex::new(())),
            revocation_waiters: Arc::new(tokio::sync::Semaphore::new(REVOCATION_WAITERS)),
        }
    }
}

/// Ownership of the single active cache-admin revocation slot.
///
/// The queue permit is deliberately **not** held here. The operation mutex is
/// itself the active slot; keeping the permit for the Begin/Store/End lifetime
/// would spend one of [`REVOCATION_WAITERS`] on the caller that is no longer
/// waiting, leaving a queue seven deep behind a contract that says eight.
pub(crate) struct RevocationAdmission {
    _operation: tokio::sync::OwnedMutexGuard<()>,
}

pub(crate) fn record_revocation_complete() {
    REVOCATION_COMPLETE.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_revocation_propagation_failed() {
    REVOCATION_PROPAGATION_FAILED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn prometheus_auth_revocations() -> String {
    format!(
        "# HELP plurx_auth_revocations_total Cache-admin revocation operations by outcome.\n\
         # TYPE plurx_auth_revocations_total counter\n\
         plurx_auth_revocations_total{{outcome=\"complete\"}} {}\n\
         plurx_auth_revocations_total{{outcome=\"queue_full\"}} {}\n\
         plurx_auth_revocations_total{{outcome=\"admission_timeout\"}} {}\n\
         plurx_auth_revocations_total{{outcome=\"propagation_failed\"}} {}\n",
        REVOCATION_COMPLETE.load(Ordering::Relaxed),
        REVOCATION_QUEUE_FULL.load(Ordering::Relaxed),
        REVOCATION_ADMISSION_TIMEOUT.load(Ordering::Relaxed),
        REVOCATION_PROPAGATION_FAILED.load(Ordering::Relaxed),
    )
}

struct CachedAdminProofState {
    /// Changes before and after every explicit revocation. Store-backed
    /// authentication captures this before its first read and may publish only
    /// if no revocation crossed that read.
    generation: u64,
    /// Cache-only recovery authorization fails closed while any local or peer
    /// revocation brackets a Store mutation. This is deliberately global: the
    /// cache is tiny and refusing unrelated recovery reads for the short
    /// mutation window is safer than allowing a target-matching mistake.
    local_active_revocations: usize,
    /// A Store write that returned without a definite outcome may still be
    /// committed by Raft after the request future is gone. Keep the whole
    /// cache closed for one proof lifetime instead of treating guard drop as
    /// proof that the mutation did not commit.
    local_ambiguity_expires_at: Option<tokio::time::Instant>,
    /// Replicated nodes start closed and are opened only by a fresh committed-
    /// roster proof that every running member implements peer revocation.
    /// Standalone SQLite has no peer writer and initializes this to true.
    cluster_revocation_capability_ready: bool,
    remote_revocations: BTreeMap<String, RemoteCacheOnlyAdminRevocation>,
    proofs: BTreeMap<String, CachedAdminProof>,
}

impl Default for CachedAdminProofState {
    fn default() -> Self {
        Self {
            generation: 0,
            local_active_revocations: 0,
            local_ambiguity_expires_at: None,
            cluster_revocation_capability_ready: true,
            remote_revocations: BTreeMap::new(),
            proofs: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct CachedAdminProof {
    user_id: i64,
    expires_at: tokio::time::Instant,
}

#[derive(Clone, Copy)]
pub(crate) struct CacheOnlyAdminAuthenticationTicket(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
enum CacheOnlyAdminRevocationTarget {
    All,
    Digest(String),
    User(i64),
}

struct RemoteCacheOnlyAdminRevocation {
    expires_at: tokio::time::Instant,
}

const MAX_REMOTE_CACHE_ONLY_ADMIN_REVOCATIONS: usize = 128;
const REMOTE_CACHE_ONLY_ADMIN_REVOCATION_TTL: Duration = CACHE_ONLY_ADMIN_PROOF_TTL;

/// Brackets a Store mutation with two cache generations. The first removes
/// existing proof before the mutation starts; the second prevents an ordinary
/// authentication that read the old Store state concurrently from publishing
/// after the mutation completes.
pub(crate) struct CacheOnlyAdminRevocation {
    cache: CacheOnlyAdminProofCache,
    target: CacheOnlyAdminRevocationTarget,
    retain_ambiguity_on_drop: bool,
}

impl Drop for CacheOnlyAdminRevocation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.cache.inner.lock() {
            let now = tokio::time::Instant::now();
            state.generation = state.generation.wrapping_add(1);
            state.local_active_revocations = state.local_active_revocations.saturating_sub(1);
            if self.retain_ambiguity_on_drop {
                let expires_at = now + CACHE_ONLY_ADMIN_PROOF_TTL;
                state.local_ambiguity_expires_at = Some(
                    state
                        .local_ambiguity_expires_at
                        .map_or(expires_at, |current| current.max(expires_at)),
                );
            }
            state.invalidate_target(&self.target);
        }
    }
}

impl CacheOnlyAdminRevocation {
    /// The peer begin phase completed, so the caller can now issue the Store
    /// mutation. From this point cancellation or an error is commit-ambiguous
    /// and guard drop must retain a bounded fail-closed fence.
    pub(crate) fn arm_ambiguity(&mut self) {
        self.retain_ambiguity_on_drop = true;
    }

    /// Release normally only after both the Store result and peer end fanout
    /// are definite successes.
    pub(crate) fn complete(mut self) {
        self.retain_ambiguity_on_drop = false;
    }
}

impl CachedAdminProofState {
    fn invalidate_target(&mut self, target: &CacheOnlyAdminRevocationTarget) {
        match target {
            CacheOnlyAdminRevocationTarget::All => self.proofs.clear(),
            CacheOnlyAdminRevocationTarget::Digest(digest) => {
                self.proofs.remove(digest);
            }
            CacheOnlyAdminRevocationTarget::User(user_id) => {
                self.proofs.retain(|_, proof| proof.user_id != *user_id);
            }
        }
    }

    fn prune_revocations(&mut self, now: tokio::time::Instant) {
        let local_ambiguity_expired = self
            .local_ambiguity_expires_at
            .is_some_and(|expires_at| expires_at <= now);
        if local_ambiguity_expired {
            self.local_ambiguity_expires_at = None;
        }
        let expired = self
            .remote_revocations
            .iter()
            .filter(|(_, revocation)| revocation.expires_at <= now)
            .map(|(operation_id, _)| operation_id.clone())
            .collect::<Vec<_>>();
        if local_ambiguity_expired || !expired.is_empty() {
            self.generation = self.generation.wrapping_add(1);
            self.proofs.clear();
        }
        for operation_id in expired {
            self.remote_revocations.remove(&operation_id);
        }
    }

    fn revocation_active(&self) -> bool {
        !self.cluster_revocation_capability_ready
            || self.local_active_revocations != 0
            || self.local_ambiguity_expires_at.is_some()
            || !self.remote_revocations.is_empty()
    }
}

impl CacheOnlyAdminProofCache {
    pub(crate) fn new(replicated: bool) -> Self {
        let cache = Self::default();
        cache.set_cluster_revocation_capability_ready(!replicated);
        cache
    }

    /// Admit one active cache-admin mutation coordinator and at most eight
    /// bounded waiters. No waiter can create a replicated claim before it owns
    /// the operation mutex.
    pub(crate) async fn acquire_revocation_operation(
        &self,
    ) -> Result<RevocationAdmission, &'static str> {
        let waiting = self
            .revocation_waiters
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                REVOCATION_QUEUE_FULL.fetch_add(1, Ordering::Relaxed);
                "cache-admin revocation queue is full"
            })?;
        let operation = tokio::time::timeout(
            REVOCATION_ADMISSION_WAIT,
            self.revocation_operation_gate.clone().lock_owned(),
        )
        .await
        .map_err(|_| {
            REVOCATION_ADMISSION_TIMEOUT.fetch_add(1, Ordering::Relaxed);
            "cache-admin revocation admission timed out"
        })?;
        // Admitted: this caller now owns the one active slot and has left the
        // queue, so its permit belongs to the next waiter. Releasing here is
        // what makes the depth eight waiters behind one active operation
        // rather than seven; the bound on futures, claims and detached
        // cleanup tasks is unchanged at nine in flight.
        drop(waiting);
        Ok(RevocationAdmission {
            _operation: operation,
        })
    }

    /// Publish the latest heartbeat-coupled committed-roster verdict. Every
    /// closed edge clears proofs and changes the generation, including a
    /// refresh error after a previous success; a later recovery therefore
    /// requires a new ordinary Store-backed authentication.
    pub(crate) fn set_cluster_revocation_capability_ready(&self, ready: bool) {
        if let Ok(mut state) = self.inner.lock() {
            if state.cluster_revocation_capability_ready != ready || !ready {
                state.generation = state.generation.wrapping_add(1);
                state.proofs.clear();
            }
            state.cluster_revocation_capability_ready = ready;
        }
    }

    /// Capture the cache generation before beginning Store authentication.
    /// A poisoned cache refuses publication and cache-only authorization.
    pub(crate) fn authentication_ticket(&self) -> Option<CacheOnlyAdminAuthenticationTicket> {
        let mut state = self.inner.lock().ok()?;
        state.prune_revocations(tokio::time::Instant::now());
        (!state.revocation_active()).then_some(CacheOnlyAdminAuthenticationTicket(state.generation))
    }

    /// Publish the result of one Store-backed authentication. Non-admin proof
    /// removes any older admin proof for the same token instead of caching a
    /// broader user record.
    pub(crate) fn record_authenticated(
        &self,
        ticket: Option<CacheOnlyAdminAuthenticationTicket>,
        token_digest: String,
        user: &User,
    ) {
        let Some(ticket) = ticket else {
            return;
        };
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        let now = tokio::time::Instant::now();
        state.prune_revocations(now);
        if state.revocation_active() || state.generation != ticket.0 {
            return;
        }
        state.proofs.retain(|_, proof| proof.expires_at > now);
        if !user.is_admin {
            state.proofs.remove(&token_digest);
            return;
        }
        if !state.proofs.contains_key(&token_digest)
            && state.proofs.len() >= MAX_CACHE_ONLY_ADMIN_PROOFS
        {
            let oldest = state
                .proofs
                .iter()
                .min_by_key(|(_, proof)| proof.expires_at)
                .map(|(digest, _)| digest.clone());
            if let Some(oldest) = oldest {
                state.proofs.remove(&oldest);
            }
        }
        state.proofs.insert(
            token_digest,
            CachedAdminProof {
                user_id: user.id,
                expires_at: now + CACHE_ONLY_ADMIN_PROOF_TTL,
            },
        );
    }

    pub(crate) fn begin_digest_revocation(&self, token_digest: &str) -> CacheOnlyAdminRevocation {
        let target = CacheOnlyAdminRevocationTarget::Digest(token_digest.to_owned());
        self.begin_local_revocation(&target);
        CacheOnlyAdminRevocation {
            cache: self.clone(),
            target,
            retain_ambiguity_on_drop: false,
        }
    }

    pub(crate) fn begin_user_revocation(&self, user_id: i64) -> CacheOnlyAdminRevocation {
        let target = CacheOnlyAdminRevocationTarget::User(user_id);
        self.begin_local_revocation(&target);
        CacheOnlyAdminRevocation {
            cache: self.clone(),
            target,
            retain_ambiguity_on_drop: false,
        }
    }

    /// Fence and invalidate every local proof while the cluster permanently
    /// activates the current revocation protocol. Activation has no Store
    /// credential target, so its safe scope is the complete bounded cache.
    pub(crate) fn begin_global_revocation(&self) -> CacheOnlyAdminRevocation {
        let target = CacheOnlyAdminRevocationTarget::All;
        self.begin_local_revocation(&target);
        CacheOnlyAdminRevocation {
            cache: self.clone(),
            target,
            retain_ambiguity_on_drop: false,
        }
    }

    fn begin_local_revocation(&self, target: &CacheOnlyAdminRevocationTarget) {
        if let Ok(mut state) = self.inner.lock() {
            state.prune_revocations(tokio::time::Instant::now());
            state.generation = state.generation.wrapping_add(1);
            state.local_active_revocations = state.local_active_revocations.saturating_add(1);
            state.invalidate_target(target);
        }
    }

    /// Begin a conservative global peer fence. The wire operation deliberately
    /// carries no token digest or user identifier; the cache is bounded and a
    /// short global refusal is preferable to transmitting credential-derived
    /// data between processes.
    pub(crate) fn begin_remote_revocation(&self, operation_id: &str) -> Result<(), &'static str> {
        let mut state = self.inner.lock().map_err(|_| "cache unavailable")?;
        let now = tokio::time::Instant::now();
        state.prune_revocations(now);
        if state.remote_revocations.contains_key(operation_id) {
            return Ok(());
        }
        if state.remote_revocations.len() >= MAX_REMOTE_CACHE_ONLY_ADMIN_REVOCATIONS {
            return Err("revocation capacity exhausted");
        }
        state.generation = state.generation.wrapping_add(1);
        state.proofs.clear();
        state.remote_revocations.insert(
            operation_id.to_owned(),
            RemoteCacheOnlyAdminRevocation {
                expires_at: now + REMOTE_CACHE_ONLY_ADMIN_REVOCATION_TTL,
            },
        );
        Ok(())
    }

    pub(crate) fn end_remote_revocation(&self, operation_id: &str) -> Result<(), &'static str> {
        let mut state = self.inner.lock().map_err(|_| "cache unavailable")?;
        state.prune_revocations(tokio::time::Instant::now());
        state.remote_revocations.remove(operation_id);
        state.generation = state.generation.wrapping_add(1);
        state.proofs.clear();
        Ok(())
    }

    pub(crate) fn invalidate_digest(&self, token_digest: &str) {
        if let Ok(mut state) = self.inner.lock() {
            state.prune_revocations(tokio::time::Instant::now());
            state.generation = state.generation.wrapping_add(1);
            state.proofs.remove(token_digest);
        }
    }

    #[cfg(test)]
    pub(crate) fn invalidate_user(&self, user_id: i64) {
        if let Ok(mut state) = self.inner.lock() {
            state.prune_revocations(tokio::time::Instant::now());
            state.generation = state.generation.wrapping_add(1);
            state.proofs.retain(|_, proof| proof.user_id != user_id);
        }
    }

    pub(crate) fn authenticate(&self, token_digest: &str) -> bool {
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        let now = tokio::time::Instant::now();
        state.prune_revocations(now);
        if state.revocation_active() {
            return false;
        }
        state.proofs.retain(|_, proof| proof.expires_at > now);
        state.proofs.contains_key(token_digest)
    }

    /// Why the Store-free answer was unavailable, for the refusal that has to
    /// explain itself. Read after [`Self::authenticate`] has already said no.
    ///
    /// Not read-only: like `authenticate`, it prunes expired revocations, and
    /// that prune clears the cache and bumps the generation when a remote
    /// revocation or the ambiguity fence has just lapsed. Same behaviour as
    /// the call that preceded it, so it introduces no hazard of its own — but
    /// it is a state transition, not an observation.
    pub(crate) fn closure_reason(&self) -> CacheOnlyAdminClosure {
        let Ok(mut state) = self.inner.lock() else {
            return CacheOnlyAdminClosure::Poisoned;
        };
        state.prune_revocations(tokio::time::Instant::now());
        if !state.cluster_revocation_capability_ready {
            return CacheOnlyAdminClosure::ProtocolNotActivated;
        }
        if state.local_active_revocations != 0 || !state.remote_revocations.is_empty() {
            return CacheOnlyAdminClosure::RevocationInFlight;
        }
        if state.local_ambiguity_expires_at.is_some() {
            return CacheOnlyAdminClosure::MutationAmbiguityFence;
        }
        CacheOnlyAdminClosure::NoProofForCaller
    }
}

/// The named reasons a Store-free admin proof was not available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheOnlyAdminClosure {
    /// Every committed member has not yet proven it runs the revocation
    /// protocol, so no proof may be published or honoured on this node. This
    /// is a cluster condition and it persists until activation completes.
    ProtocolNotActivated,
    /// A credential mutation is bracketing the cache right now.
    RevocationInFlight,
    /// A commit-ambiguous mutation left the bounded fail-closed fence up.
    MutationAmbiguityFence,
    /// The cache is open and simply holds no proof for this caller yet.
    NoProofForCaller,
    /// The cache mutex was poisoned by a panic in another request.
    Poisoned,
}

impl CacheOnlyAdminClosure {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::ProtocolNotActivated => {
                "its Store-free admin proof is closed because the cluster has not finished \
                 activating the credential-revocation protocol on every committed member"
            }
            Self::RevocationInFlight => {
                "its Store-free admin proof is closed while a credential change propagates"
            }
            Self::MutationAmbiguityFence => {
                "its Store-free admin proof is fenced after a credential change whose outcome \
                 was not definite"
            }
            Self::NoProofForCaller => "it holds no Store-free admin proof for this session yet",
            Self::Poisoned => "its Store-free admin proof cache was poisoned by an earlier panic",
        }
    }
}

/// Guard for machine callers: a scoped API key, and nothing else.
///
/// **Two credential kinds, two doors.** A login token does not open a
/// key-scoped route and a key does not open a user route, deliberately and
/// in both directions:
///
/// - A token must not pass here, because then "monarr can trigger scans"
///   would be satisfiable by handing it an admin token — which can also
///   read the TMDB/Trakt secrets out of `GET /api/v1/settings`. The narrow
///   credential only helps if the narrow route insists on it.
/// - A key must not pass a user route, because a key has no user. There is
///   no "who" to attribute a watch state or a playback session to, and
///   inventing one would be worse than refusing.
///
/// Build with [`ScopedKey::require`] inside a handler rather than as an
/// extractor generic, so the scope a route needs is written in that route's
/// own body where it can be read.
// Unused until the `/api/v1/scan` routes it guards land (integration plan
// P3). It lives here now rather than arriving with them because the auth
// matrix — key-only, scope-exact, revocation-first — is the security claim
// keys exist to make, and it is written and tested beside the credential it
// belongs to. `ApiKey::allows` is covered by the tests below.
#[allow(dead_code)]
pub struct ScopedKey(pub plurx_core::domain::ApiKey);

#[allow(dead_code)]
impl ScopedKey {
    /// Extract a key from the request and require `scope`.
    ///
    /// `last_used_at` is refreshed at most once per minute on successful
    /// checks. It remains a useful activity signal without turning every
    /// request into a replicated write.
    pub async fn require(
        parts: &mut Parts,
        state: &AppState,
        scope: &'static str,
    ) -> Result<ScopedKey, ApiError> {
        let secret = token_from_parts(parts).ok_or(ApiError::Unauthorized)?;
        if !auth::is_api_key(&secret) {
            // A user token on a machine route. Unauthorized rather than
            // Forbidden: the credential is the wrong KIND, and saying
            // "forbidden" would suggest the right token could work.
            return Err(ApiError::Unauthorized);
        }
        let key = state
            .store
            .api_key_for_hash(&auth::hash_token(&secret))
            .await?
            .ok_or(ApiError::Unauthorized)?;
        if key.disabled {
            // Revoked is indistinguishable from never-existed, on purpose.
            return Err(ApiError::Unauthorized);
        }
        if !key.allows(scope) {
            // A real key that lacks this scope: Forbidden, because the
            // credential is valid and the answer is about permission.
            return Err(ApiError::Forbidden);
        }
        if key_activity_refresh_due(&key, unix_seconds()) {
            // Best-effort activity freshness: authentication already
            // succeeded, and a later request retries the bounded refresh.
            crate::store_result::observe(
                crate::store_result::Operation::TouchApiKey,
                crate::store_result::Discard::BestEffort,
                state.store.touch_api_key(key.id).await,
            );
        }
        Ok(ScopedKey(key))
    }
}

fn unix_seconds() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

fn key_activity_refresh_due(key: &ApiKey, now: Option<i64>) -> bool {
    now.is_some_and(|now| auth::activity_refresh_due(key.last_used_at, now))
}

/// The raw bearer token, for endpoints that operate on the token itself
/// (e.g. logout). Does not validate the token against the store.
pub struct RawToken(pub String);

fn token_from_parts(parts: &Parts) -> Option<String> {
    // Authorization: Bearer <token>
    if let Some(value) = parts.headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = value.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                return Some(token.trim().to_owned());
            }
        }
    }
    // X-Api-Key: <key>
    //
    // The header every *arr application already uses for exactly this. There
    // is no reason to be fussy about which one carries the same secret: the
    // `plx_` prefix is what decides whether a credential is a key or a login
    // token, and it decides that identically whichever header it arrived in.
    // Accepting only one of the two conventions turns a valid key into a 401
    // that reads as "your key is wrong".
    if let Some(value) = parts.headers.get("x-api-key") {
        if let Ok(s) = value.to_str() {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_owned());
            }
        }
    }
    // ?token=<token>
    parts
        .uri
        .query()
        .and_then(|q| url_decode_lookup(q, "token"))
}

/// Minimal `application/x-www-form-urlencoded` lookup for a single key.
fn url_decode_lookup(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = token_from_parts(parts).ok_or(ApiError::Unauthorized)?;
        let hash = auth::hash_token(&token);
        let ticket = state.cache_only_admin_proofs.authentication_ticket();
        let user = match state.store.authenticate_token(&hash).await? {
            TokenAuthentication::Authenticated(user) => user,
            TokenAuthentication::Expired { idle_days } => {
                state.cache_only_admin_proofs.invalidate_digest(&hash);
                return Err(session_expired(idle_days));
            }
            TokenAuthentication::Unknown => {
                state.cache_only_admin_proofs.invalidate_digest(&hash);
                return Err(ApiError::Unauthorized);
            }
        };
        state
            .cache_only_admin_proofs
            .record_authenticated(ticket, hash, &user);
        Ok(AuthUser(user))
    }
}

/// Stable code every client matches to land on its sign-in screen with the
/// reason, rather than a generic "your session ended".
pub(crate) const SESSION_EXPIRED_CODE: &str = "session_expired";

/// The 401 for a login token that sat unused for the whole idle window. The
/// status stays 401 so every client that predates the code still signs out;
/// the code and `idle_days` let a current client say why.
pub(crate) fn session_expired(idle_days: i64) -> ApiError {
    let unit = if idle_days == 1 { "day" } else { "days" };
    ApiError::typed_detail(
        axum::http::StatusCode::UNAUTHORIZED,
        SESSION_EXPIRED_CODE,
        format!("Signed out after {idle_days} {unit} of inactivity. Sign in again to continue."),
        serde_json::json!({ "idle_days": idle_days }),
    )
}

/// Response header on every request that made a watch write (K-04 M2): the
/// highest Raft log index of those writes when every one reported it, or
/// [`COMMIT_INDEX_UNKNOWN`] when any did not. Absent when the request made
/// no watch write.
pub const COMMIT_INDEX_HEADER: &str = "x-plurx-commit-index";
/// [`COMMIT_INDEX_HEADER`]'s value for a request with a watch write whose
/// position is unknown. A client that sees it drops the index it echoes:
/// keeping an older one would name a floor below a write it was just told
/// succeeded.
pub const COMMIT_INDEX_UNKNOWN: &str = "unknown";
/// Request header by which a client echoes the newest numeric
/// [`COMMIT_INDEX_HEADER`] it has seen, for 60 seconds after seeing it, and
/// stops echoing on [`COMMIT_INDEX_UNKNOWN`].
pub const READ_AFTER_HEADER: &str = "x-plurx-read-after";

/// The client's `X-Plurx-Read-After` fence, if it sent exactly one
/// well-formed positive index. Anything else is `None`, which makes watch
/// reads go to Authority: a malformed fence can only cost latency. A
/// well-formed but stale index could skip a write, which is why a response
/// whose watch write has no known position says [`COMMIT_INDEX_UNKNOWN`]
/// and the client drops its echo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadAfter(pub Option<u64>);

impl ReadAfter {
    #[must_use]
    pub fn from_headers(headers: &axum::http::HeaderMap) -> Self {
        let mut values = headers.get_all(READ_AFTER_HEADER).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return Self(None);
        };
        let index = value
            .to_str()
            .ok()
            .filter(|text| !text.is_empty() && text.len() <= 20)
            .filter(|text| text.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|text| text.parse::<u64>().ok())
            .filter(|index| *index > 0);
        Self(index)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ReadAfter {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_headers(&parts.headers))
    }
}

impl FromRequestParts<AppState> for RawToken {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        token_from_parts(parts)
            .map(RawToken)
            .ok_or(ApiError::Unauthorized)
    }
}

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let AuthUser(user) = AuthUser::from_request_parts(parts, state).await?;
        if user.is_admin {
            Ok(AdminUser(user))
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

impl FromRequestParts<AppState> for CacheOnlyAdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = token_from_parts(parts).ok_or(ApiError::Unauthorized)?;
        let digest = auth::hash_token(&token);
        // The Store-free answer first, and on its own. While a cached proof
        // exists this guard reaches no Store at all, which is the property
        // the recovery reads were built for: a wedged Store must not be able
        // to lock an operator out of the page that diagnoses it.
        if state.cache_only_admin_proofs.authenticate(&digest) {
            return Ok(Self);
        }
        let closure = state.cache_only_admin_proofs.closure_reason();
        // No proof here means this process cannot vouch for the caller by
        // itself — it does not mean the caller's credential is bad. Ask the
        // Store, under a bound, for exactly the authority every other admin
        // route already grants from it. A Store that answers is strictly
        // safer than a cached proof: a revoked token has no row to find.
        let ticket = state.cache_only_admin_proofs.authentication_ticket();
        let lookup = tokio::time::timeout(
            CACHE_ONLY_ADMIN_STORE_FALLBACK_TIMEOUT,
            state.store.user_for_token(&digest),
        )
        .await;
        match lookup {
            Ok(Ok(Some(user))) => {
                state
                    .cache_only_admin_proofs
                    .record_authenticated(ticket, digest, &user);
                if user.is_admin {
                    Ok(Self)
                } else {
                    Err(ApiError::Forbidden)
                }
            }
            Ok(Ok(None)) => {
                state.cache_only_admin_proofs.invalidate_digest(&digest);
                Err(ApiError::Unauthorized)
            }
            // The Store could not answer inside the bound. That is this
            // node's condition, not the caller's credential, and it has to
            // say so: a 401 here logs an administrator out of a working
            // session over a cluster fault they were trying to look at.
            //
            // The precise condition goes to the log, not the body. This point
            // is reachable with any bearer string, and "a credential change is
            // propagating" or "the cache was poisoned by an earlier panic" is
            // cluster-internal state that an unauthenticated caller has not
            // earned. The operator reads it beside the rest of the incident.
            Ok(Err(_)) | Err(_) => {
                tracing::warn!(
                    closure = closure.describe(),
                    "refused a cluster recovery read: the Store did not answer within the bound"
                );
                Err(cache_only_admin_unavailable())
            }
        }
    }
}

/// How long the cache-miss path may wait on the Store before answering that
/// it cannot authorize. Deliberately shorter than any page's own patience:
/// the point of the bound is that a wedged Store degrades this guard to a
/// named refusal instead of hanging the request that reports the wedge.
const CACHE_ONLY_ADMIN_STORE_FALLBACK_TIMEOUT: Duration = Duration::from_secs(2);

/// A cluster recovery read this node could not authorize. The code is stable
/// so a client can tell it apart from a verdict on the credential; the sentence
/// says which of the two it is and nothing else, because the caller has not
/// authenticated. The specific condition is logged.
fn cache_only_admin_unavailable() -> ApiError {
    ApiError::typed(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "cluster_recovery_authorization_unavailable",
        format!(
            "this node could not authorize a cluster recovery read within {} seconds; this is \
             the node's condition, not a verdict on your credential",
            CACHE_ONLY_ADMIN_STORE_FALLBACK_TIMEOUT.as_secs()
        ),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn read_after_accepts_exactly_one_positive_decimal_index() {
        use super::{ReadAfter, READ_AFTER_HEADER};
        use axum::http::{HeaderMap, HeaderValue};

        let parse = |values: &[&str]| {
            let mut headers = HeaderMap::new();
            for value in values {
                headers.append(
                    READ_AFTER_HEADER,
                    HeaderValue::from_str(value).expect("header value"),
                );
            }
            ReadAfter::from_headers(&headers).0
        };
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&["41"]), Some(41));
        assert_eq!(parse(&["18446744073709551615"]), Some(u64::MAX));
        // Anything else is no fence at all, which sends watch reads to
        // Authority: a bad header can cost latency, never skip a write.
        for bad in [
            "0",
            "",
            "-1",
            "+41",
            " 41",
            "41 ",
            "4.1",
            "0x29",
            "18446744073709551616",
            // A client that echoed the invalidation verbatim instead of
            // dropping its echo still reaches Authority.
            super::COMMIT_INDEX_UNKNOWN,
        ] {
            assert_eq!(parse(&[bad]), None, "{bad:?}");
        }
        assert_eq!(parse(&["41", "42"]), None, "two fences are ambiguous");
    }

    use super::{
        key_activity_refresh_due, percent_decode, CacheOnlyAdminProofCache,
        CACHE_ONLY_ADMIN_PROOF_TTL,
    };
    use plurx_core::auth;
    use plurx_core::domain::{scopes, ApiKey, User};
    use std::time::Duration;

    fn key(scopes: &[&str], disabled: bool) -> ApiKey {
        ApiKey {
            id: 1,
            name: "monarr".into(),
            key_hash: "h".into(),
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            created_at: 0,
            last_used_at: None,
            disabled,
        }
    }

    fn user(id: i64, is_admin: bool) -> User {
        User {
            id,
            username: format!("user-{id}"),
            password_hash: String::new(),
            is_admin,
            created_at: 1,
        }
    }

    fn record(cache: &CacheOnlyAdminProofCache, digest: String, user: &User) {
        let ticket = cache.authentication_ticket();
        cache.record_authenticated(ticket, digest, user);
    }

    #[tokio::test(start_paused = true)]
    async fn cache_only_admin_proofs_expire_without_sliding_and_refuse_non_admins() {
        let cache = CacheOnlyAdminProofCache::default();
        let admin_digest = auth::hash_token("admin-token");
        let viewer_digest = auth::hash_token("viewer-token");
        record(&cache, admin_digest.clone(), &user(1, true));
        record(&cache, viewer_digest.clone(), &user(2, false));

        assert!(cache.authenticate(&admin_digest));
        assert!(!cache.authenticate(&viewer_digest));
        assert!(!cache
            .inner
            .lock()
            .expect("admin proof cache")
            .proofs
            .contains_key("admin-token"));
        tokio::time::advance(CACHE_ONLY_ADMIN_PROOF_TTL).await;
        assert!(!cache.authenticate(&admin_digest));
    }

    #[test]
    fn cache_only_admin_proofs_honor_digest_and_user_invalidation() {
        let cache = CacheOnlyAdminProofCache::default();
        let first_digest = auth::hash_token("first-admin-token");
        let second_digest = auth::hash_token("second-admin-token");
        record(&cache, first_digest.clone(), &user(3, true));
        record(&cache, second_digest.clone(), &user(3, true));
        cache.invalidate_digest(&first_digest);
        assert!(!cache.authenticate(&first_digest));
        assert!(cache.authenticate(&second_digest));
        cache.invalidate_user(3);
        assert!(!cache.authenticate(&second_digest));
    }

    #[test]
    fn active_revocation_blocks_ticket_publication_until_guard_drop() {
        use std::sync::{Arc, Barrier};

        let cache = CacheOnlyAdminProofCache::default();
        let digest = auth::hash_token("racing-admin-token");
        let admin = user(7, true);
        record(&cache, digest.clone(), &admin);
        let stale_ticket = cache
            .authentication_ticket()
            .expect("ticket before revocation");

        let revocation = cache.begin_user_revocation(admin.id);
        let revocation_active = Arc::new(Barrier::new(2));
        let publication_attempted = Arc::new(Barrier::new(2));
        let publisher = {
            let cache = cache.clone();
            let digest = digest.clone();
            let admin = admin.clone();
            let revocation_active = revocation_active.clone();
            let publication_attempted = publication_attempted.clone();
            std::thread::spawn(move || {
                revocation_active.wait();
                assert!(cache.authentication_ticket().is_none());
                cache.record_authenticated(Some(stale_ticket), digest, &admin);
                publication_attempted.wait();
            })
        };

        revocation_active.wait();
        publication_attempted.wait();
        assert!(!cache.authenticate(&digest));
        drop(revocation);
        publisher.join().expect("publication thread");
        assert!(!cache.authenticate(&digest));
        assert!(cache.authentication_ticket().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn remote_revocation_fence_expires_bounded_and_reinvalidates() {
        let cache = CacheOnlyAdminProofCache::default();
        let digest = auth::hash_token("remote-admin-token");
        let admin = user(8, true);
        record(&cache, digest.clone(), &admin);
        cache
            .begin_remote_revocation("remote-operation")
            .expect("remote begin");
        assert!(!cache.authenticate(&digest));
        assert!(cache.authentication_ticket().is_none());

        tokio::time::advance(super::REMOTE_CACHE_ONLY_ADMIN_REVOCATION_TTL).await;
        assert!(cache.authentication_ticket().is_some());
        assert!(!cache.authenticate(&digest));
    }

    #[tokio::test(start_paused = true)]
    async fn replicated_exclusion_projection_outlives_remote_ttl_and_clock_skew() {
        let cache = CacheOnlyAdminProofCache::new(true);
        let digest = auth::hash_token("restart-during-revocation");
        let admin = user(11, true);
        cache.set_cluster_revocation_capability_ready(true);
        record(&cache, digest.clone(), &admin);
        assert!(cache.authenticate(&digest));

        cache
            .begin_remote_revocation("exact-replicated-claim")
            .expect("peer Begin");
        // A restarted process has no memory fence, but starts with the same
        // closed readiness value until its local-applied projection proves
        // that the exact replicated exclusion was deleted.
        cache.set_cluster_revocation_capability_ready(false);
        tokio::time::advance(
            super::REMOTE_CACHE_ONLY_ADMIN_REVOCATION_TTL + Duration::from_secs(60 * 60),
        )
        .await;
        assert!(cache.authentication_ticket().is_none());
        assert!(!cache.authenticate(&digest));

        cache.set_cluster_revocation_capability_ready(true);
        assert!(cache.authentication_ticket().is_some());
        assert!(
            !cache.authenticate(&digest),
            "the replicated deletion edge must clear every pre-revocation proof"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cache_admin_revocation_operation_queue_is_bounded_and_raii_released() {
        let cache = CacheOnlyAdminProofCache::default();
        let owner = cache
            .acquire_revocation_operation()
            .await
            .expect("first request owns the process gate");
        // The active operation holds the mutex, not a queue slot. If it kept
        // its permit for the Begin/Store/End lifetime the queue would be one
        // shorter than the contract states.
        assert_eq!(
            cache.revocation_waiters.available_permits(),
            super::REVOCATION_WAITERS,
            "an admitted operation must not occupy a waiting slot"
        );
        let mut waiters = Vec::new();
        for _ in 0..super::REVOCATION_WAITERS {
            let cache = cache.clone();
            waiters.push(tokio::spawn(async move {
                cache.acquire_revocation_operation().await
            }));
        }
        for _ in 0..64 {
            if cache.revocation_waiters.available_permits() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(cache.revocation_waiters.available_permits(), 0);
        assert!(cache.acquire_revocation_operation().await.is_err());

        tokio::time::advance(super::REVOCATION_ADMISSION_WAIT).await;
        for waiter in waiters {
            // Every one of the eight was *queued*: none of them was turned
            // away at the door because the active operation had taken a slot.
            match waiter.await.expect("waiter task") {
                Ok(_) => panic!("a waiter behind a held operation cannot be admitted"),
                Err(error) => {
                    assert_eq!(error, "cache-admin revocation admission timed out")
                }
            }
        }
        drop(owner);
        assert!(
            cache.acquire_revocation_operation().await.is_ok(),
            "RAII cancellation releases the gate when no cleanup owns it"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn commit_ambiguous_local_mutation_retains_one_bounded_fail_closed_fence() {
        let cache = CacheOnlyAdminProofCache::default();
        let digest = auth::hash_token("ambiguous-admin-token");
        let admin = user(9, true);
        record(&cache, digest.clone(), &admin);

        let mut revocation = cache.begin_user_revocation(admin.id);
        // This is the exact transition performed only after the cluster begin
        // fanout succeeds and immediately before the handler can call Store.
        revocation.arm_ambiguity();
        drop(revocation);

        assert!(!cache.authenticate(&digest));
        assert!(cache.authentication_ticket().is_none());
        tokio::time::advance(CACHE_ONLY_ADMIN_PROOF_TTL - Duration::from_millis(1)).await;
        assert!(cache.authentication_ticket().is_none());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(cache.authentication_ticket().is_some());
        assert!(
            !cache.authenticate(&digest),
            "fence expiry must not resurrect the invalidated credential"
        );
    }

    #[test]
    fn replicated_capability_loss_clears_proofs_and_invalidates_in_flight_publication() {
        let cache = CacheOnlyAdminProofCache::new(true);
        let digest = auth::hash_token("rolling-admin-token");
        let admin = user(10, true);
        assert!(cache.authentication_ticket().is_none());

        cache.set_cluster_revocation_capability_ready(true);
        let before_rollback = cache
            .authentication_ticket()
            .expect("all committed members proved the capability");
        cache.record_authenticated(Some(before_rollback), digest.clone(), &admin);
        assert!(cache.authenticate(&digest));

        let racing = cache
            .authentication_ticket()
            .expect("ticket before a peer rollback heartbeat");
        cache.set_cluster_revocation_capability_ready(false);
        cache.record_authenticated(Some(racing), digest.clone(), &admin);
        assert!(!cache.authenticate(&digest));
        assert!(cache.authentication_ticket().is_none());

        cache.set_cluster_revocation_capability_ready(true);
        assert!(
            !cache.authenticate(&digest),
            "roll-forward requires a new ordinary authentication"
        );
    }

    #[test]
    fn remote_revocation_fences_have_a_hard_capacity() {
        let cache = CacheOnlyAdminProofCache::default();
        for id in 0..super::MAX_REMOTE_CACHE_ONLY_ADMIN_REVOCATIONS {
            cache
                .begin_remote_revocation(&format!("operation-{id}"))
                .expect("bounded remote fence");
        }
        assert!(cache.begin_remote_revocation("one-too-many").is_err());
        cache
            .end_remote_revocation("operation-0")
            .expect("end one fence");
        cache
            .begin_remote_revocation("replacement")
            .expect("released capacity is reusable");
    }

    #[test]
    fn poisoned_proof_cache_fails_closed_for_authentication_and_revocation() {
        let cache = CacheOnlyAdminProofCache::default();
        let poisoner = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                let _locked = cache.inner.lock().expect("proof cache lock");
                panic!("poison proof cache");
            })
        };
        assert!(poisoner.join().is_err());
        assert!(cache.authentication_ticket().is_none());
        assert!(!cache.authenticate(&auth::hash_token("admin-token")));
        assert!(cache.begin_remote_revocation("remote-operation").is_err());
    }

    #[test]
    fn cache_only_admin_proofs_have_a_hard_capacity() {
        let cache = CacheOnlyAdminProofCache::default();
        for id in 0..=super::MAX_CACHE_ONLY_ADMIN_PROOFS as i64 {
            record(
                &cache,
                auth::hash_token(&format!("token-{id}")),
                &user(id, true),
            );
        }
        let state = cache.inner.lock().expect("admin proof cache");
        assert_eq!(state.proofs.len(), super::MAX_CACHE_ONLY_ADMIN_PROOFS);
        assert!(state.proofs.contains_key(&auth::hash_token(&format!(
            "token-{}",
            super::MAX_CACHE_ONLY_ADMIN_PROOFS
        ))));
    }

    #[test]
    fn logout_revocation_generation_rejects_an_in_flight_store_result() {
        let cache = CacheOnlyAdminProofCache::default();
        let digest = auth::hash_token("racing-logout-token");
        let before_revocation = cache.authentication_ticket();

        let revocation = cache.begin_digest_revocation(&digest);
        let during_store_delete = cache.authentication_ticket();
        drop(revocation);
        cache.record_authenticated(before_revocation, digest.clone(), &user(4, true));
        cache.record_authenticated(during_store_delete, digest.clone(), &user(4, true));

        assert!(
            !cache.authenticate(&digest),
            "a Store result started before logout must not revive the token"
        );
    }

    #[test]
    fn user_revocation_generation_rejects_all_in_flight_store_results() {
        let cache = CacheOnlyAdminProofCache::default();
        let first_digest = auth::hash_token("racing-user-token-1");
        let second_digest = auth::hash_token("racing-user-token-2");
        let before_revocation = cache.authentication_ticket();

        let revocation = cache.begin_user_revocation(5);
        let during_store_mutation = cache.authentication_ticket();
        drop(revocation);
        cache.record_authenticated(before_revocation, first_digest.clone(), &user(5, true));
        cache.record_authenticated(during_store_mutation, second_digest.clone(), &user(5, true));

        assert!(!cache.authenticate(&first_digest));
        assert!(!cache.authenticate(&second_digest));
    }

    #[test]
    fn authentication_and_revocation_callers_bracket_store_work_with_generations() {
        let extractor = include_str!("extract.rs")
            .split_once("impl FromRequestParts<AppState> for AuthUser")
            .expect("AuthUser extractor")
            .1
            .split_once("impl FromRequestParts<AppState> for RawToken")
            .expect("AuthUser extractor end")
            .0;
        assert!(
            extractor
                .find("authentication_ticket()")
                .expect("auth ticket")
                < extractor
                    .find("authenticate_token(&hash)")
                    .expect("Store auth")
        );
        assert!(extractor.contains("record_authenticated(ticket, hash, &user)"));

        let auth = include_str!("auth.rs");
        let login = auth
            .split_once("pub async fn login(")
            .expect("login")
            .1
            .split_once("pub async fn logout(")
            .expect("login end")
            .0;
        assert!(
            login.find("authentication_ticket()").expect("login ticket")
                < login
                    .find("get_user_by_username")
                    .expect("login Store read")
        );
        assert!(login.contains("record_authenticated(proof_ticket, hash, &user)"));
        let logout = auth
            .split_once("pub async fn logout(")
            .expect("logout")
            .1
            .split_once("pub async fn me(")
            .expect("logout end")
            .0;
        assert!(
            logout
                .find("ClusterCacheRevocation::begin_digest")
                .expect("logout revocation guard")
                < logout
                    .find("delete_token_with_cache_admin_claim")
                    .expect("logout Store mutation")
        );
        assert!(
            logout
                .find("delete_token_with_cache_admin_claim")
                .expect("logout Store mutation")
                < logout
                    .find("proof_revocation.finish")
                    .expect("logout peer revocation end")
        );
        assert!(logout.contains("proof_revocation.mutation_claim()"));

        let setup = include_str!("system.rs")
            .split_once("pub async fn setup(")
            .expect("setup")
            .1
            .split_once("pub struct SystemDto")
            .expect("setup end")
            .0;
        assert!(
            setup.find("authentication_ticket()").expect("setup ticket")
                < setup.find("count_users()").expect("setup Store read")
        );
        assert!(setup.contains("record_authenticated(proof_ticket, token_hash, &user)"));

        let users = include_str!("users.rs");
        let update = users
            .split_once("pub async fn update(")
            .expect("user update")
            .1
            .split_once("pub async fn delete(")
            .expect("user update end")
            .0;
        assert!(
            update
                .find("ClusterCacheRevocation::begin_user")
                .expect("demotion revocation guard")
                < update
                    .find("demote_user_preserving_admin")
                    .expect("conditional admin Store mutation")
        );
        assert!(!update.contains("count_admins"));
        assert!(
            update
                .find("reset_password_and_revoke_tokens")
                .expect("atomic password and token revocation")
                < update
                    .rfind("proof_revocation.finish")
                    .expect("user peer revocation end"),
            "demotion and password reset must stay inside one peer bracket"
        );
        assert!(!update.contains("set_password(id"));
        assert!(!update.contains("delete_tokens_for_user"));
        assert!(
            update
                .matches("ClusterCacheRevocation::mutation_claim")
                .count()
                >= 2
        );
        let delete = users
            .split_once("pub async fn delete(")
            .expect("user delete")
            .1;
        assert!(
            delete
                .find("ClusterCacheRevocation::begin_user")
                .expect("delete revocation guard")
                < delete
                    .find("delete_user_preserving_admin")
                    .expect("conditional delete Store mutation")
        );
        assert!(!delete.contains("count_admins"));
        assert!(delete.contains("proof_revocation.mutation_claim()"));
        assert!(
            delete
                .find("delete_user_preserving_admin")
                .expect("conditional delete Store mutation")
                < delete
                    .find("proof_revocation.finish")
                    .expect("delete peer revocation end")
        );
    }

    // The routing rule the whole two-credential design rests on: the prefix
    // decides WHICH table to look in, before any lookup happens. Without it
    // a key and a token would have to be tried against each other's store,
    // and "not found" would stop meaning anything.
    #[test]
    fn the_prefix_tells_a_key_from_a_login_token() {
        let secret = auth::generate_api_key().expect("key");
        assert!(auth::is_api_key(&secret));
        let token = auth::generate_token().expect("token");
        assert!(
            !auth::is_api_key(&token),
            "a login token must never be mistaken for a key"
        );
    }

    #[test]
    fn scope_checks_are_exact_and_revocation_beats_all_of_them() {
        let k = key(&[scopes::SCAN_TRIGGER], false);
        assert!(k.allows(scopes::SCAN_TRIGGER));
        assert!(!k.allows(scopes::STATUS_READ));
        assert!(!k.allows("scan:trigger "), "no fuzzy matching on scopes");

        let revoked = key(&[scopes::SCAN_TRIGGER, scopes::STATUS_READ], true);
        for s in scopes::ALL {
            assert!(!revoked.allows(s), "a revoked key still allows {s}");
        }
    }

    #[test]
    fn key_activity_refresh_is_coalesced_without_affecting_authorization() {
        let mut k = key(&[scopes::SCAN_TRIGGER], false);
        assert!(key_activity_refresh_due(&k, Some(1_000)));

        k.last_used_at = Some(940);
        assert!(!key_activity_refresh_due(&k, Some(1_000)));

        k.last_used_at = Some(939);
        assert!(key_activity_refresh_due(&k, Some(1_000)));
        assert!(!key_activity_refresh_due(&k, None));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("hello"), "hello");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%2Fpath"), "/path");
    }
}
