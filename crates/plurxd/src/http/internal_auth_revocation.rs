//! Bounded authenticated propagation for process-local recovery auth proofs.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::{ActivityPeer, MAX_OPERATIONS_PEERS};
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::CacheOnlyAdminRevocation;
use super::peer_transport::{deadline_after, exact_auth_from_headers, PeerAuthMode, PeerTransport};
use crate::state::AppState;

pub(crate) const PATH: &str = "/api/v1/internal/auth/cache-revocation";
pub(crate) const MAX_REQUEST_BYTES: usize = 256;
const FANOUT_TIMEOUT: Duration = Duration::from_secs(2);
const FANOUT_CONCURRENCY: usize = 8;

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
    _local: CacheOnlyAdminRevocation,
    operation_id: String,
    peers: Vec<ActivityPeer>,
    transport: PeerTransport,
}

impl ClusterCacheRevocation {
    pub(crate) async fn begin_digest(state: &AppState, digest: &str) -> Result<Self, ApiError> {
        let local = state
            .cache_only_admin_proofs
            .begin_digest_revocation(digest);
        Self::begin(state, local).await
    }

    pub(crate) async fn begin_user(state: &AppState, user_id: i64) -> Result<Self, ApiError> {
        let local = state.cache_only_admin_proofs.begin_user_revocation(user_id);
        Self::begin(state, local).await
    }

    async fn begin(state: &AppState, local: CacheOnlyAdminRevocation) -> Result<Self, ApiError> {
        let peers = peer_directory(state).await?;
        let transport = PeerTransport::new(state.membership.clone());
        let operation_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let request = Request {
            schema_version: 1,
            operation_id: operation_id.clone(),
            phase: Phase::Begin,
        };
        if let Err(error) = fanout(&transport, &peers, &request).await {
            let cleanup = Request {
                phase: Phase::End,
                ..request
            };
            let _ = fanout(&transport, &peers, &cleanup).await;
            return Err(error);
        }
        Ok(Self {
            _local: local,
            operation_id,
            peers,
            transport,
        })
    }

    /// End only after the caller's durable Store mutation. Unioning a fresh
    /// roster with the begin roster gives concurrently added members the final
    /// global invalidation before the API reports success.
    pub(crate) async fn finish(self, state: &AppState) -> Result<(), ApiError> {
        let current = match peer_directory(state).await {
            Ok(peers) => peers,
            Err(error) => {
                let _ = fanout(&self.transport, &self.peers, &self.request(Phase::End)).await;
                return Err(error);
            }
        };
        let peers = self
            .peers
            .iter()
            .chain(current.iter())
            .map(|peer| (peer.node_id.clone(), peer.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect::<Vec<_>>();
        if peers.len() > MAX_OPERATIONS_PEERS {
            let _ = fanout(&self.transport, &self.peers, &self.request(Phase::End)).await;
            return Err(propagation_error());
        }
        fanout(&self.transport, &peers, &self.request(Phase::End)).await
    }

    fn request(&self, phase: Phase) -> Request {
        Request {
            schema_version: 1,
            operation_id: self.operation_id.clone(),
            phase,
        }
    }
}

async fn peer_directory(state: &AppState) -> Result<Vec<ActivityPeer>, ApiError> {
    let peers = state
        .membership
        .operations_peers()
        .await
        .map_err(|_| propagation_error())?;
    if peers.len() > MAX_OPERATIONS_PEERS {
        return Err(propagation_error());
    }
    Ok(peers)
}

async fn fanout(
    transport: &PeerTransport,
    peers: &[ActivityPeer],
    request: &Request,
) -> Result<(), ApiError> {
    let body = serde_json::to_vec(request).map_err(|_| propagation_error())?;
    let deadline = deadline_after(FANOUT_TIMEOUT);
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
    apply(&state.cache_only_admin_proofs, &request).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::NO_CONTENT)
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
    if request.schema_version == 1 && canonical_operation_id {
        Ok(())
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

#[cfg(test)]
mod tests {
    use super::{apply, Phase, Request};
    use crate::http::extract::CacheOnlyAdminProofCache;
    use plurx_core::auth;
    use plurx_core::domain::User;

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
                schema_version: 1,
                operation_id: operation_id.clone(),
                phase: Phase::Begin,
            },
        );
        assert!(node_b.authentication_ticket().is_none());
        node_b.record_authenticated(Some(in_flight_ticket), digest.clone(), &user);
        wire(
            &node_b,
            Request {
                schema_version: 1,
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
                schema_version: 1,
                operation_id: operation_id.clone(),
                phase: Phase::Begin,
            },
        );
        node_b.record_authenticated(Some(in_flight_ticket), digest.clone(), &user);
        wire(
            &node_b,
            Request {
                schema_version: 1,
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
