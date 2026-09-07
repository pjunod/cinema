//! Bounded authenticated propagation for process-local recovery auth proofs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::{
    ActivityPeer, CacheAdminRevocationCleanupOutcome, CacheAdminRevocationLease, MembershipManager,
    MAX_CACHE_ADMIN_REVOCATION_PEERS,
};
use plurx_core::store::CacheAdminMutationClaim;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::CacheOnlyAdminRevocation;
use super::peer_transport::{deadline_after, exact_auth_from_headers, PeerAuthMode, PeerTransport};
use crate::state::AppState;

pub(crate) const PATH: &str = "/api/v1/internal/auth/cache-revocation";
pub(crate) const MAX_REQUEST_BYTES: usize = 256;
const FANOUT_TIMEOUT: Duration = Duration::from_secs(2);
const FANOUT_CONCURRENCY: usize = 8;
const MAX_STABLE_ROSTER_PASSES: usize = MAX_CACHE_ADMIN_REVOCATION_PEERS + 2;
const WIRE_SCHEMA_VERSION: u32 = 2;
const MEMBERSHIP_EXCLUSION_DURATION: Duration = Duration::from_secs(15);
const EXCLUSION_CLEANUP_RETRY_INITIAL: Duration = Duration::from_millis(50);
const EXCLUSION_CLEANUP_RETRY_MAX: Duration = Duration::from_secs(2);
const LOCAL_CLAIM_APPLY_TIMEOUT: Duration = Duration::from_millis(1_500);
const LOCAL_CLAIM_APPLY_POLL: Duration = Duration::from_millis(10);
const ACTIVATION_RETRY_INTERVAL: Duration = Duration::from_secs(3);
/// A condition this loop cannot clear on its own — a stuck lifecycle fence, a
/// held exclusion — is not worth one Raft transaction and one warning line
/// every three seconds until someone notices. Back off toward this ceiling
/// while the reason stays the same, and reset the moment it changes.
const ACTIVATION_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(60);

type PeerIdentity = (String, u64, Option<String>);

fn peer_identity(peer: &ActivityPeer) -> PeerIdentity {
    (peer.node_id.clone(), peer.raft_id, peer.http_base.clone())
}

fn merge_peers<'a>(peers: impl IntoIterator<Item = &'a ActivityPeer>) -> Vec<ActivityPeer> {
    peers
        .into_iter()
        .map(|peer| (peer_identity(peer), peer.clone()))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

/// Requires the exact committed roster to be observed unchanged after every
/// member in it has acknowledged Begin. Membership can move while the first
/// fanout is in flight; Store mutation is not admitted until a subsequent
/// consistent read has no unfenced identity.
#[derive(Default)]
struct StableBeginRoster {
    previous: Option<BTreeSet<PeerIdentity>>,
    acknowledged: BTreeMap<PeerIdentity, ActivityPeer>,
}

/// Cancellation-safe owner of the replicated membership exclusion. A Store
/// mutation carries the embedded exact claim, so a proposal that reaches Raft
/// only after Drop cleanup becomes a no-op rather than crossing the exclusion.
struct CacheAdminMembershipExclusion {
    lease: Option<CacheAdminRevocationLease>,
    operation_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    membership: MembershipManager,
    shutdown: tokio_util::sync::CancellationToken,
    release_runtime: tokio::runtime::Handle,
}

impl CacheAdminMembershipExclusion {
    fn prepare(
        state: &AppState,
        operation_guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<Self, ApiError> {
        let lease = state
            .membership
            .is_replicated()
            .then(|| {
                MembershipManager::prepare_cache_admin_revocation_claim(
                    &state.node_id,
                    MEMBERSHIP_EXCLUSION_DURATION,
                )
            })
            .transpose()
            .map_err(|_| propagation_error())?;
        Ok(Self {
            lease,
            operation_guard: Some(operation_guard),
            membership: state.membership.clone(),
            shutdown: state.shutdown.clone(),
            release_runtime: tokio::runtime::Handle::current(),
        })
    }

    async fn commit(&mut self) -> Result<(), ApiError> {
        let Some(lease) = self.lease.as_mut() else {
            return Ok(());
        };
        self.membership
            .commit_cache_admin_revocation_lease(lease)
            .await
            .map_err(|_| propagation_error())
    }

    fn mutation_claim(&self) -> Option<&CacheAdminMutationClaim> {
        self.lease
            .as_ref()
            .map(CacheAdminRevocationLease::mutation_claim)
    }

    fn operation_id(&self) -> String {
        self.lease
            .as_ref()
            .map(|lease| lease.claim_id().to_owned())
            .unwrap_or_else(|| uuid::Uuid::new_v4().hyphenated().to_string())
    }

    async fn release(&mut self) -> Result<(), ApiError> {
        let Some(lease) = self.lease.as_ref() else {
            return Ok(());
        };
        release_membership_exclusion_with_retry(&self.membership, lease, &self.shutdown).await?;
        self.lease.take();
        self.operation_guard.take();
        Ok(())
    }
}

impl Drop for CacheAdminMembershipExclusion {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        let operation_guard = self.operation_guard.take();
        if self.shutdown.is_cancelled() {
            // Replicated heartbeat expiry is the bounded crash cleanup.
            return;
        }
        let membership = self.membership.clone();
        let shutdown = self.shutdown.clone();
        self.release_runtime.spawn(async move {
            // The fail-fast process gate remains owned until exact cleanup is
            // definitive. An ambiguous release therefore coalesces all later
            // local requests into immediate refusals instead of spawning more
            // claims and retry tasks.
            let _operation_guard = operation_guard;
            if let Err(error) =
                release_membership_exclusion_with_retry(&membership, &lease, &shutdown).await
            {
                tracing::warn!(?error, "cache-admin membership exclusion cleanup stopped");
            }
        });
    }
}

impl StableBeginRoster {
    fn observe(&mut self, peers: Vec<ActivityPeer>) -> (Vec<ActivityPeer>, bool) {
        let current = peers
            .into_iter()
            .map(|peer| (peer_identity(&peer), peer))
            .collect::<BTreeMap<_, _>>();
        let identities = current.keys().cloned().collect::<BTreeSet<_>>();
        let additions = current
            .into_iter()
            .filter(|(identity, _)| !self.acknowledged.contains_key(identity))
            .map(|(_, peer)| peer)
            .collect::<Vec<_>>();
        let stable = self.previous.as_ref() == Some(&identities);
        self.previous = Some(identities);
        (additions, stable)
    }

    fn acknowledge(&mut self, peers: &[ActivityPeer]) {
        self.acknowledged
            .extend(peers.iter().map(|peer| (peer_identity(peer), peer.clone())));
    }

    fn peers(&self) -> Vec<ActivityPeer> {
        self.acknowledged.values().cloned().collect()
    }

    fn len(&self) -> usize {
        self.acknowledged.len()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Begin,
    End,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    operation_id: String,
    phase: Phase,
}

/// Owns the local target-specific fence and the peers that acknowledged a
/// conservative global fence. The wire carries no digest or user identity.
/// Cancellation leaves remote fences active until their bounded TTL because
/// clearing them while the Store commit outcome is unknown would be unsafe.
pub(crate) struct ClusterCacheRevocation {
    local: Option<CacheOnlyAdminRevocation>,
    membership_exclusion: CacheAdminMembershipExclusion,
    operation_id: String,
    peers: Vec<ActivityPeer>,
    transport: PeerTransport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheAdminRevocationActivation {
    Pending,
    Complete,
}

impl ClusterCacheRevocation {
    /// Permanently activate the current revocation protocol as soon as every
    /// committed member advertises it. This uses the same exclusion, stable
    /// roster, and global Begin/End invalidation as a credential mutation, but
    /// deliberately performs no Store write.
    pub(crate) async fn activate_if_ready(
        state: &AppState,
    ) -> Result<CacheAdminRevocationActivation, ApiError> {
        if !state.membership.is_replicated() {
            return Ok(CacheAdminRevocationActivation::Pending);
        }
        if state
            .membership
            .cache_admin_revocation_activated_locally()
            .await
            .map_err(|_| propagation_error())?
        {
            return Ok(CacheAdminRevocationActivation::Complete);
        }
        if !state
            .membership
            .cache_admin_revocation_activation_ready()
            .await
            .map_err(|_| propagation_error())?
        {
            return Ok(CacheAdminRevocationActivation::Pending);
        }
        let operation_guard = state
            .cache_only_admin_proofs
            .try_acquire_revocation_operation()
            .map_err(|_| propagation_error())?;
        let local = state.cache_only_admin_proofs.begin_global_revocation();
        Self::begin(state, local, operation_guard)
            .await?
            .finish(state)
            .await?;
        Ok(CacheAdminRevocationActivation::Complete)
    }

    pub(crate) async fn begin_digest(state: &AppState, digest: &str) -> Result<Self, ApiError> {
        let operation_guard = state
            .cache_only_admin_proofs
            .try_acquire_revocation_operation()
            .map_err(|_| propagation_error())?;
        let local = state
            .cache_only_admin_proofs
            .begin_digest_revocation(digest);
        Self::begin(state, local, operation_guard).await
    }

    pub(crate) async fn begin_user(state: &AppState, user_id: i64) -> Result<Self, ApiError> {
        let operation_guard = state
            .cache_only_admin_proofs
            .try_acquire_revocation_operation()
            .map_err(|_| propagation_error())?;
        let local = state.cache_only_admin_proofs.begin_user_revocation(user_id);
        Self::begin(state, local, operation_guard).await
    }

    async fn begin(
        state: &AppState,
        mut local: CacheOnlyAdminRevocation,
        operation_guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<Self, ApiError> {
        let mut membership_exclusion =
            CacheAdminMembershipExclusion::prepare(state, operation_guard)?;
        membership_exclusion.commit().await?;
        let transport = PeerTransport::new(state.membership.clone());
        // The wire operation ID is the exact replicated claim. A peer only
        // acknowledges Begin after this same row is visible in its local
        // applied SQL, causally ordering any later absence after the acquire.
        let operation_id = membership_exclusion.operation_id();
        let membership = state.membership.clone();
        let claim_id = operation_id.clone();
        if wait_for_exact_local_claim_with(&state.shutdown, move || {
            let membership = membership.clone();
            let claim_id = claim_id.clone();
            async move {
                membership
                    .cache_admin_revocation_claim_applied(&claim_id)
                    .await
                    .map_err(|_| "cache-admin exclusion local apply read failed")
            }
        })
        .await
        .is_err()
        {
            membership_exclusion.release().await?;
            return Err(propagation_error());
        }
        let request = Request {
            schema_version: WIRE_SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            phase: Phase::Begin,
        };
        let deadline = deadline_after(FANOUT_TIMEOUT);
        let mut roster = StableBeginRoster::default();
        for _ in 0..MAX_STABLE_ROSTER_PASSES {
            let current = match peer_directory(state, &operation_id, deadline).await {
                Ok(peers) => peers,
                Err(error) => {
                    cleanup_begin(&transport, &roster.peers(), &request).await;
                    membership_exclusion.release().await?;
                    return Err(error);
                }
            };
            let (unfenced, stable) = roster.observe(current);
            if !unfenced.is_empty() {
                if let Err(error) = fanout(&transport, &unfenced, &request, deadline).await {
                    let cleanup = merge_peers(roster.peers().iter().chain(unfenced.iter()));
                    cleanup_begin(&transport, &cleanup, &request).await;
                    membership_exclusion.release().await?;
                    return Err(error);
                }
                roster.acknowledge(&unfenced);
                if !revocation_peer_count_is_bounded(roster.len()) {
                    cleanup_begin(&transport, &roster.peers(), &request).await;
                    membership_exclusion.release().await?;
                    return Err(propagation_error());
                }
            }
            if stable {
                // No Store mutation can start before every peer in two
                // consecutive committed-roster reads has acknowledged Begin.
                local.arm_ambiguity();
                return Ok(Self {
                    local: Some(local),
                    membership_exclusion,
                    operation_id,
                    peers: roster.peers(),
                    transport,
                });
            }
        }
        cleanup_begin(&transport, &roster.peers(), &request).await;
        membership_exclusion.release().await?;
        Err(propagation_error())
    }

    /// End only after the caller's durable Store mutation. Unioning a fresh
    /// roster with the begin roster gives concurrently added members the final
    /// global invalidation before the API reports success.
    pub(crate) async fn finish(mut self, state: &AppState) -> Result<(), ApiError> {
        let deadline = deadline_after(FANOUT_TIMEOUT);
        let current = match peer_directory(state, &self.operation_id, deadline).await {
            Ok(peers) => peers,
            Err(error) => {
                let _ = fanout(
                    &self.transport,
                    &self.peers,
                    &self.request(Phase::End),
                    deadline_after(FANOUT_TIMEOUT),
                )
                .await;
                return Err(error);
            }
        };
        let acknowledged = self
            .peers
            .iter()
            .map(peer_identity)
            .collect::<BTreeSet<_>>();
        let late = current
            .iter()
            .filter(|peer| !acknowledged.contains(&peer_identity(peer)))
            .cloned()
            .collect::<Vec<_>>();
        let peers = merge_peers(self.peers.iter().chain(current.iter()));
        if !revocation_peer_count_is_bounded(peers.len()) {
            let _ = fanout(
                &self.transport,
                &self.peers,
                &self.request(Phase::End),
                deadline_after(FANOUT_TIMEOUT),
            )
            .await;
            return Err(propagation_error());
        }
        // A member that appeared after begin stabilization still receives a
        // Begin before the terminal invalidation. This does not replace the
        // pre-Store stability barrier; it closes authority immediately if a
        // membership commit raced the Store call itself.
        if !late.is_empty() {
            fanout(
                &self.transport,
                &late,
                &self.request(Phase::Begin),
                deadline,
            )
            .await?;
        }
        fanout(&self.transport, &peers, &self.request(Phase::End), deadline).await?;
        // Membership remains excluded until the committed Store mutation has
        // completed and every fenced member has cleared its cached proof. An
        // exact release failure leaves both this lease and the local ambiguity
        // fence armed for bounded fail-closed cleanup.
        self.membership_exclusion.release().await?;
        self.local
            .take()
            .expect("cluster cache revocation owns its local fence")
            .complete();
        Ok(())
    }

    /// Exact Store predicate for the credential mutation inside this guard.
    /// Standalone SQLite has no membership writer and therefore returns None.
    pub(crate) fn mutation_claim(&self) -> Option<&CacheAdminMutationClaim> {
        self.membership_exclusion.mutation_claim()
    }

    fn request(&self, phase: Phase) -> Request {
        Request {
            schema_version: WIRE_SCHEMA_VERSION,
            operation_id: self.operation_id.clone(),
            phase,
        }
    }
}

/// Own automatic one-time protocol activation independently of membership
/// status publication. A stuck definitive exclusion cleanup must keep its
/// fail-closed authority without making the five-second diagnostics cache go
/// stale during the same quorum-loss incident.
pub(crate) async fn cache_admin_revocation_activation_loop(
    state: AppState,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if !state.membership.is_replicated() {
        return;
    }
    cache_admin_revocation_activation_loop_with(shutdown, move || {
        let state = state.clone();
        async move {
            match ClusterCacheRevocation::activate_if_ready(&state).await {
                Ok(outcome) => Ok(outcome),
                // Name the precondition. Until an administrator can read why
                // this never completes, its only symptom is a cluster status
                // route answering 401 to a valid session — which reaches them
                // as a login page, and reaches nobody as a cause.
                Err(error) => Err(
                    match state.membership.cache_admin_exclusion_blockers().await {
                        Ok(blockers) if !blockers.is_empty() => {
                            format!("{error:?}; blocked by: {}", blockers.join(", "))
                        }
                        _ => format!("{error:?}"),
                    },
                ),
            }
        }
    })
    .await;
}

pub(crate) async fn cache_admin_revocation_activation_loop_with<Activate, ActivateFuture>(
    shutdown: tokio_util::sync::CancellationToken,
    mut activate: Activate,
) where
    Activate: FnMut() -> ActivateFuture,
    ActivateFuture: std::future::Future<Output = Result<CacheAdminRevocationActivation, String>>,
{
    let mut delay = ACTIVATION_RETRY_INTERVAL;
    let mut last_error: Option<String> = None;
    loop {
        let result = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            result = activate() => result,
        };
        match result {
            Ok(CacheAdminRevocationActivation::Complete) => break,
            Ok(CacheAdminRevocationActivation::Pending) => {
                delay = ACTIVATION_RETRY_INTERVAL;
                last_error = None;
            }
            Err(error) => {
                // Repeat the line only when the reason changes. A permanent
                // condition that logs identically every three seconds buries
                // itself: the fleet this was written for produced 28,800
                // copies a day of one sentence that named nothing.
                if last_error.as_deref() != Some(error.as_str()) {
                    tracing::warn!(%error, "cache admin revocation activation attempt failed");
                    last_error = Some(error);
                    delay = ACTIVATION_RETRY_INTERVAL;
                } else {
                    delay = (delay * 2).min(ACTIVATION_RETRY_MAX_INTERVAL);
                }
            }
        }
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(delay) => {}
        }
    }
}

async fn cleanup_begin(transport: &PeerTransport, peers: &[ActivityPeer], request: &Request) {
    let cleanup = Request {
        phase: Phase::End,
        ..request.clone()
    };
    let _ = fanout(transport, peers, &cleanup, deadline_after(FANOUT_TIMEOUT)).await;
}

async fn release_membership_exclusion_with_retry(
    membership: &MembershipManager,
    lease: &CacheAdminRevocationLease,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<(), ApiError> {
    let mut delay = EXCLUSION_CLEANUP_RETRY_INITIAL;
    loop {
        match membership.release_cache_admin_revocation_lease(lease).await {
            Ok(CacheAdminRevocationCleanupOutcome::Confirmed) => return Ok(()),
            Ok(CacheAdminRevocationCleanupOutcome::Ambiguous) | Err(_) => {
                tokio::select! {
                    () = shutdown.cancelled() => return Err(propagation_error()),
                    () = tokio::time::sleep(delay) => {}
                }
                delay = delay.saturating_mul(2).min(EXCLUSION_CLEANUP_RETRY_MAX);
            }
        }
    }
}

async fn peer_directory(
    state: &AppState,
    claim_id: &str,
    deadline: tokio::time::Instant,
) -> Result<Vec<ActivityPeer>, ApiError> {
    let peers = tokio::time::timeout_at(
        deadline,
        state.membership.cache_admin_revocation_peers(claim_id),
    )
    .await
    .map_err(|_| propagation_error())?
    .map_err(|_| propagation_error())?;
    if !revocation_peer_count_is_bounded(peers.len()) {
        return Err(propagation_error());
    }
    Ok(peers)
}

fn revocation_peer_count_is_bounded(peer_count: usize) -> bool {
    peer_count <= MAX_CACHE_ADMIN_REVOCATION_PEERS
}

async fn fanout(
    transport: &PeerTransport,
    peers: &[ActivityPeer],
    request: &Request,
    deadline: tokio::time::Instant,
) -> Result<(), ApiError> {
    let body = serde_json::to_vec(request).map_err(|_| propagation_error())?;
    let outcomes = stream::iter(peers.iter().cloned().map(|peer| {
        let transport = transport.clone();
        let body = body.clone();
        async move {
            let Some(base) = peer.http_base.as_deref() else {
                return false;
            };
            if !peer.reachable {
                return false;
            }
            transport
                .request(
                    &peer.node_id,
                    base,
                    reqwest::Method::POST,
                    PATH,
                    body,
                    deadline,
                    256,
                    PeerAuthMode::ExactRequest,
                )
                .await
                .is_ok_and(|response| response.status == reqwest::StatusCode::NO_CONTENT)
        }
    }))
    .buffer_unordered(FANOUT_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    outcomes
        .iter()
        .all(|success| *success)
        .then_some(())
        .ok_or_else(propagation_error)
}

fn propagation_error() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "admin_revocation_propagation_failed",
        "the credential change was not acknowledged by every committed cluster member",
    )
}

pub(crate) async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    let auth = exact_auth_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if !state
        .membership
        .authorize_internal_peer_request(&auth, "POST", PATH, &body)
        .await
        .unwrap_or(false)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let request: Request = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate(&request)?;
    let membership = state.membership.clone();
    let claim_id = request.operation_id.clone();
    apply_and_wait_for_local_claim_with(
        &state.cache_only_admin_proofs,
        &request,
        &state.shutdown,
        move || {
            let membership = membership.clone();
            let claim_id = claim_id.clone();
            async move {
                membership
                    .cache_admin_revocation_claim_applied(&claim_id)
                    .await
                    .map_err(|_| "cache-admin exclusion local apply read failed")
            }
        },
    )
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn apply_and_wait_for_local_claim_with<Check, CheckFuture>(
    cache: &super::extract::CacheOnlyAdminProofCache,
    request: &Request,
    shutdown: &tokio_util::sync::CancellationToken,
    check: Check,
) -> Result<(), &'static str>
where
    Check: FnMut() -> CheckFuture,
    CheckFuture: std::future::Future<Output = Result<bool, &'static str>>,
{
    // Install the memory fence first. A timeout or read error deliberately
    // leaves it active until the origin's End cleanup or its bounded TTL.
    apply(cache, request)?;
    if !matches!(request.phase, Phase::Begin) {
        return Ok(());
    }

    wait_for_exact_local_claim_with(shutdown, check).await
}

async fn wait_for_exact_local_claim_with<Check, CheckFuture>(
    shutdown: &tokio_util::sync::CancellationToken,
    mut check: Check,
) -> Result<(), &'static str>
where
    Check: FnMut() -> CheckFuture,
    CheckFuture: std::future::Future<Output = Result<bool, &'static str>>,
{
    tokio::time::timeout(LOCAL_CLAIM_APPLY_TIMEOUT, async {
        loop {
            if check().await? {
                return Ok(());
            }
            tokio::select! {
                () = shutdown.cancelled() => {
                    return Err("shutdown before cache-admin exclusion applied");
                }
                () = tokio::time::sleep(LOCAL_CLAIM_APPLY_POLL) => {}
            }
        }
    })
    .await
    .map_err(|_| "cache-admin exclusion local apply timed out")?
}

fn apply(
    cache: &super::extract::CacheOnlyAdminProofCache,
    request: &Request,
) -> Result<(), &'static str> {
    match request.phase {
        Phase::Begin => cache.begin_remote_revocation(&request.operation_id),
        Phase::End => cache.end_remote_revocation(&request.operation_id),
    }
}

fn validate(request: &Request) -> Result<(), StatusCode> {
    let canonical_operation_id = uuid::Uuid::parse_str(&request.operation_id)
        .is_ok_and(|id| id.hyphenated().to_string() == request.operation_id);
    if request.schema_version == WIRE_SCHEMA_VERSION && canonical_operation_id {
        Ok(())
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply, apply_and_wait_for_local_claim_with, validate, wait_for_exact_local_claim_with,
        Phase, Request, StableBeginRoster, WIRE_SCHEMA_VERSION,
    };
    use crate::http::extract::CacheOnlyAdminProofCache;
    use plurx_core::auth;
    use plurx_core::cluster::membership::{
        ActivityPeer, MAX_CACHE_ADMIN_REVOCATION_PEERS, MAX_OPERATIONS_PEERS,
    };
    use plurx_core::domain::User;

    #[test]
    fn credential_revocation_uses_the_exact_committed_security_roster() {
        let source = include_str!("internal_auth_revocation.rs")
            .split_once("async fn peer_directory(")
            .expect("revocation peer directory")
            .1
            .split_once("async fn fanout(")
            .expect("revocation peer directory end")
            .0;
        assert!(source.contains("cache_admin_revocation_peers(claim_id)"));
        assert!(!source.contains("operations_peers()"));
    }

    #[test]
    fn automatic_activation_checks_the_permanent_marker_before_taking_the_gate() {
        let source = include_str!("internal_auth_revocation.rs")
            .split_once("pub(crate) async fn activate_if_ready")
            .expect("automatic activation entry")
            .1
            .split_once("pub(crate) async fn begin_digest")
            .expect("automatic activation entry end")
            .0;
        assert!(
            source
                .find("cache_admin_revocation_activated_locally()")
                .expect("local permanent-marker stop")
                < source
                    .find("cache_admin_revocation_activation_ready()")
                    .expect("marker-aware quorum preflight")
        );
        assert!(
            source
                .find("cache_admin_revocation_activation_ready()")
                .expect("marker-aware quorum preflight")
                < source
                    .find("try_acquire_revocation_operation()")
                    .expect("local operation gate")
        );
    }

    #[test]
    fn replicated_membership_exclusion_spans_final_roster_read_and_peer_end() {
        let source = include_str!("internal_auth_revocation.rs");
        let begin = source
            .split_once("async fn begin(")
            .expect("cluster revocation begin")
            .1
            .split_once("pub(crate) async fn finish")
            .expect("cluster revocation begin end")
            .0;
        assert!(
            begin
                .find("membership_exclusion.commit().await?")
                .expect("replicated exclusion commit")
                < begin
                    .find("wait_for_exact_local_claim_with")
                    .expect("origin exact-claim apply wait")
        );
        assert!(
            begin
                .find("wait_for_exact_local_claim_with")
                .expect("origin exact-claim apply wait")
                < begin
                    .find("peer_directory(state, &operation_id, deadline).await")
                    .expect("committed roster read")
        );
        assert!(begin.contains("membership_exclusion,"));

        let digest_entry = source
            .split_once("pub(crate) async fn begin_digest")
            .expect("digest revocation entry")
            .1
            .split_once("pub(crate) async fn begin_user")
            .expect("digest revocation entry end")
            .0;
        assert!(
            digest_entry
                .find("try_acquire_revocation_operation()")
                .expect("fail-fast process gate")
                < digest_entry
                    .find("begin_digest_revocation(digest)")
                    .expect("local invalidation"),
            "the bounded gate must be owned before any per-request revocation state exists"
        );

        let finish = source
            .split_once("pub(crate) async fn finish")
            .expect("cluster revocation finish")
            .1
            .split_once("fn request(&self")
            .expect("cluster revocation finish end")
            .0;
        assert!(
            finish.find("self.request(Phase::End)").expect("peer end")
                < finish
                    .rfind("self.membership_exclusion.release().await?")
                    .expect("replicated exclusion release")
        );
        let drop_owner = source
            .split_once("impl Drop for CacheAdminMembershipExclusion")
            .expect("membership exclusion Drop")
            .1
            .split_once("impl StableBeginRoster")
            .expect("membership exclusion Drop end")
            .0;
        assert!(drop_owner.contains("let operation_guard = self.operation_guard.take()"));
        assert!(drop_owner.contains("let _operation_guard = operation_guard"));
        assert!(drop_owner.contains("release_membership_exclusion_with_retry"));
    }

    fn admin(id: i64) -> User {
        User {
            id,
            username: format!("admin-{id}"),
            password_hash: String::new(),
            is_admin: true,
            created_at: 1,
        }
    }

    fn record(cache: &CacheOnlyAdminProofCache, digest: &str, user: &User) {
        let ticket = cache.authentication_ticket();
        cache.record_authenticated(ticket, digest.to_owned(), user);
    }

    fn wire(cache: &CacheOnlyAdminProofCache, request: Request) {
        let encoded = serde_json::to_vec(&request).expect("encode peer fence");
        let decoded = serde_json::from_slice(&encoded).expect("decode peer fence");
        apply(cache, &decoded).expect("peer acknowledgement");
    }

    fn peer(node_id: &str, raft_id: u64) -> ActivityPeer {
        ActivityPeer {
            node_id: node_id.to_owned(),
            raft_id,
            http_base: Some(format!("http://{node_id}:32400")),
            reachable: true,
        }
    }

    #[test]
    fn local_apply_ack_wire_version_rejects_pre_barrier_receivers() {
        let operation_id = uuid::Uuid::new_v4().hyphenated().to_string();
        assert!(validate(&Request {
            schema_version: WIRE_SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            phase: Phase::Begin,
        })
        .is_ok());
        assert_eq!(
            validate(&Request {
                schema_version: WIRE_SCHEMA_VERSION - 1,
                operation_id,
                phase: Phase::Begin,
            }),
            Err(axum::http::StatusCode::BAD_REQUEST),
            "an old receiver's unconditional 204 must not satisfy the new Begin contract"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn begin_ack_installs_memory_fence_before_waiting_for_exact_local_apply() {
        let cache = CacheOnlyAdminProofCache::default();
        let digest = auth::hash_token("apply-lag-admin-token");
        record(&cache, &digest, &admin(46));
        let request = Request {
            schema_version: WIRE_SCHEMA_VERSION,
            operation_id: uuid::Uuid::new_v4().hyphenated().to_string(),
            phase: Phase::Begin,
        };
        let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = checks.clone();
        apply_and_wait_for_local_claim_with(
            &cache,
            &request,
            &tokio_util::sync::CancellationToken::new(),
            || {
                assert!(
                    !cache.authenticate(&digest),
                    "the memory fence must precede every local-applied claim check"
                );
                let attempt = observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async move { Ok(attempt == 2) }
            },
        )
        .await
        .expect("third exact-claim observation acknowledges Begin");
        assert_eq!(checks.load(std::sync::atomic::Ordering::Relaxed), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_local_apply_wait_leaves_peer_memory_fence_closed() {
        let cache = CacheOnlyAdminProofCache::default();
        let digest = auth::hash_token("cancelled-apply-wait-admin");
        record(&cache, &digest, &admin(47));
        let request = Request {
            schema_version: WIRE_SCHEMA_VERSION,
            operation_id: uuid::Uuid::new_v4().hyphenated().to_string(),
            phase: Phase::Begin,
        };
        let shutdown = tokio_util::sync::CancellationToken::new();
        shutdown.cancel();
        assert!(
            apply_and_wait_for_local_claim_with(&cache, &request, &shutdown, || async {
                Ok(false)
            })
            .await
            .is_err()
        );
        assert!(cache.authentication_ticket().is_none());
        assert!(!cache.authenticate(&digest));
    }

    #[tokio::test(start_paused = true)]
    async fn origin_waits_for_exact_claim_apply_before_observing_added_member() {
        let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = checks.clone();
        wait_for_exact_local_claim_with(&tokio_util::sync::CancellationToken::new(), move || {
            let attempt = observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async move { Ok(attempt == 2) }
        })
        .await
        .expect("the exact exclusion eventually applies after the member addition");

        let mut roster = StableBeginRoster::default();
        let current = vec![peer("node-b", 2), peer("node-c", 3), peer("node-d", 4)];
        let (unfenced, stable) = roster.observe(current.clone());
        assert!(!stable);
        assert_eq!(
            unfenced, current,
            "the post-claim roster must include node-d"
        );
        assert_eq!(checks.load(std::sync::atomic::Ordering::Relaxed), 3);
    }

    #[test]
    fn membership_added_between_begin_passes_is_fenced_before_store_admission() {
        let node_b = CacheOnlyAdminProofCache::default();
        let node_c = CacheOnlyAdminProofCache::default();
        let digest = auth::hash_token("admin-token");
        let user = admin(45);
        record(&node_b, &digest, &user);
        record(&node_c, &digest, &user);
        let operation_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let begin = Request {
            schema_version: WIRE_SCHEMA_VERSION,
            operation_id,
            phase: Phase::Begin,
        };
        let peer_b = peer("node-b", 2);
        let peer_c = peer("node-c", 3);
        let mut roster = StableBeginRoster::default();

        let (first, stable) = roster.observe(vec![peer_b.clone()]);
        assert!(!stable, "one observation cannot admit the Store mutation");
        assert_eq!(first, vec![peer_b.clone()]);
        wire(&node_b, begin.clone());
        roster.acknowledge(&first);
        assert!(!node_b.authenticate(&digest));
        assert!(node_c.authenticate(&digest));

        // C commits into membership after B's Begin acknowledgement. The next
        // production roster pass must discover and fence C, not treat a final
        // End as sufficient revocation coverage.
        let (second, stable) = roster.observe(vec![peer_b.clone(), peer_c.clone()]);
        assert!(!stable, "membership movement must postpone Store admission");
        assert_eq!(second, vec![peer_c.clone()]);
        wire(&node_c, begin);
        roster.acknowledge(&second);
        assert!(!node_b.authenticate(&digest));
        assert!(!node_c.authenticate(&digest));

        let (third, stable) = roster.observe(vec![peer_b, peer_c]);
        assert!(third.is_empty());
        assert!(stable, "only an unchanged fully fenced roster admits Store");
    }

    #[test]
    fn credential_revocation_accepts_more_than_the_diagnostics_probe_limit() {
        let peers = (1..=MAX_OPERATIONS_PEERS + 1)
            .map(|index| peer(&format!("node-{index}"), index as u64 + 1))
            .collect::<Vec<_>>();
        let mut roster = StableBeginRoster::default();

        let (unfenced, stable) = roster.observe(peers);
        assert!(!stable);
        assert_eq!(unfenced.len(), MAX_OPERATIONS_PEERS + 1);
        roster.acknowledge(&unfenced);
        assert!(
            super::revocation_peer_count_is_bounded(roster.len()),
            "the security barrier must admit every peer in a supported committed roster"
        );
        assert!(super::revocation_peer_count_is_bounded(
            MAX_CACHE_ADMIN_REVOCATION_PEERS
        ));
        assert!(!super::revocation_peer_count_is_bounded(
            MAX_CACHE_ADMIN_REVOCATION_PEERS + 1
        ));
        assert_eq!(super::FANOUT_CONCURRENCY, MAX_OPERATIONS_PEERS);
    }

    #[test]
    fn digest_and_user_revocation_on_a_invalidate_primed_b_before_success() {
        let node_a = CacheOnlyAdminProofCache::default();
        let node_b = CacheOnlyAdminProofCache::default();
        let digest = auth::hash_token("node-b-token-one");
        let second_digest = auth::hash_token("node-b-token-two");
        let user = admin(44);
        record(&node_a, &digest, &user);
        record(&node_b, &digest, &user);
        let in_flight_ticket = node_b
            .authentication_ticket()
            .expect("node B ticket before peer begin");
        let operation_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let local_digest_fence = node_a.begin_digest_revocation(&digest);
        wire(
            &node_b,
            Request {
                schema_version: WIRE_SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                phase: Phase::Begin,
            },
        );
        assert!(node_b.authentication_ticket().is_none());
        node_b.record_authenticated(Some(in_flight_ticket), digest.clone(), &user);
        wire(
            &node_b,
            Request {
                schema_version: WIRE_SCHEMA_VERSION,
                operation_id,
                phase: Phase::End,
            },
        );
        // The origin can report digest-revocation success only after this end
        // acknowledgement; both independent caches are already invalid.
        drop(local_digest_fence);
        assert!(!node_a.authenticate(&digest));
        assert!(!node_b.authenticate(&digest));

        record(&node_a, &digest, &user);
        record(&node_a, &second_digest, &user);
        record(&node_b, &digest, &user);
        record(&node_b, &second_digest, &user);
        let in_flight_ticket = node_b
            .authentication_ticket()
            .expect("node B ticket before user-wide peer begin");
        let operation_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let local_user_fence = node_a.begin_user_revocation(user.id);
        wire(
            &node_b,
            Request {
                schema_version: WIRE_SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                phase: Phase::Begin,
            },
        );
        node_b.record_authenticated(Some(in_flight_ticket), digest.clone(), &user);
        wire(
            &node_b,
            Request {
                schema_version: WIRE_SCHEMA_VERSION,
                operation_id,
                phase: Phase::End,
            },
        );
        drop(local_user_fence);

        assert!(!node_a.authenticate(&digest));
        assert!(!node_a.authenticate(&second_digest));
        assert!(!node_b.authenticate(&digest));
        assert!(!node_b.authenticate(&second_digest));
        assert!(node_b.authentication_ticket().is_some());
    }
}
