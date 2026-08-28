//! Admin cluster-membership API and the narrow join-token redemption path.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use plurx_core::cluster::membership::{
    FinalizeJoinRequest, IssuedJoinToken, MembershipError, MembershipStatus, ProtocolChange,
    RedeemJoinRequest,
};
use serde::Deserialize;

use super::cluster_operations::{collect_aggregate, local_owned_media_sessions};
use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use crate::state::AppState;

const MAINTENANCE_PREPARATION_DURATION: Duration = Duration::from_secs(3_600);

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct IssueJoinTokenRequest {
    /// Short by default because the token carries the complete authority to
    /// enter the cluster. Bounds prevent a typo from minting a near-permanent
    /// credential or one that expires before it can be copied.
    pub expires_in_seconds: Option<u64>,
}

#[derive(Deserialize)]
pub struct LeaveRequest {
    /// The node id shown by the roster that the operator confirmed. Requiring
    /// it binds the destructive POST to that backend when a non-sticky load
    /// balancer routes the confirmation and action separately.
    pub node_id: String,
}

pub async fn issue_join_token(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(request): Json<IssueJoinTokenRequest>,
) -> Result<Json<IssuedJoinToken>, ApiError> {
    let seconds = request.expires_in_seconds.unwrap_or(600).clamp(60, 3_600);
    state
        .membership
        .issue_token(Duration::from_secs(seconds))
        .await
        .map(Json)
        .map_err(api_error)
}

pub async fn issue_learner_join_token(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(request): Json<IssueJoinTokenRequest>,
) -> Result<Json<IssuedJoinToken>, ApiError> {
    let seconds = request.expires_in_seconds.unwrap_or(600).clamp(60, 3_600);
    state
        .membership
        .issue_learner_token(Duration::from_secs(seconds))
        .await
        .map(Json)
        .map_err(api_error)
}

pub async fn nodes(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<MembershipStatus>, ApiError> {
    state.membership.status().await.map(Json).map_err(api_error)
}

/// GET /api/v1/cluster/ingress — the other ingresses a signed-in client may
/// retry a media capability through.
///
/// Deliberately its own route rather than a field on the public
/// `/api/v1/server`: that endpoint is what a client uses to *identify* an
/// unknown candidate before it has decided to trust it, so it never carries a
/// credential, and the cluster's ingress list is not something an anonymous
/// prober should be able to enumerate. Every signed-in household member may
/// read it — they already reach these hosts when a stream is placed there.
///
/// Origins only. This never names a node id, a Raft address, or a private
/// port; §7.2 forbids exposing an internal address to a client, and what an
/// operator publishes here is the same address a browser would already use.
pub async fn ingress(
    _viewer: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ClusterIngress>, ApiError> {
    Ok(Json(ClusterIngress {
        node_urls: state
            .membership
            .reachable_peer_http_urls()
            .await
            .unwrap_or_default()
            .into_iter()
            .take(MAX_ADVERTISED_INGRESSES)
            .collect(),
    }))
}

/// Bounded on the way out. A client walks this list one entry at a time on a
/// transport failure, so an unbounded cluster would turn one failed stream
/// into an unbounded retry ladder.
const MAX_ADVERTISED_INGRESSES: usize = 8;

#[derive(serde::Serialize)]
pub struct ClusterIngress {
    pub node_urls: Vec<String>,
}

/// Narrow the cluster onto the learner protocol.
///
/// This is the one step that makes a previously compatible binary unable to
/// boot here, so it is deliberately explicit, admin-only, and separate from
/// deploying the binary that supports it. `GET /cluster/nodes` reports the
/// active range and which nodes are not ready yet.
pub async fn activate_learner_protocol(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<ProtocolChange>, ApiError> {
    state
        .membership
        .activate_learner_protocol()
        .await
        .map(Json)
        .map_err(api_error)
}

/// Widen the cluster back to the pre-learner protocol for a degraded rollback.
pub async fn deactivate_learner_protocol(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<ProtocolChange>, ApiError> {
    state
        .membership
        .deactivate_learner_protocol()
        .await
        .map(Json)
        .map_err(api_error)
}

pub async fn remove_node(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<Json<MembershipStatus>, ApiError> {
    state
        .membership
        .remove_node(&node_id)
        .await
        .map(Json)
        .map_err(api_error)
}

pub async fn promote_node(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<Json<MembershipStatus>, ApiError> {
    state
        .membership
        .promote_learner(&node_id)
        .await
        .map(Json)
        .map_err(api_error)
}

pub async fn enter_maintenance(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<Json<MembershipStatus>, ApiError> {
    if node_id != state.node_id {
        // Preserve the unclustered API contract: SQLite has no maintenance
        // target at all, which is more fundamental than a route-id mismatch.
        state.membership.status().await.map_err(api_error)?;
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "maintenance_node_mismatch",
            "open the target node directly; maintenance must be prepared by the process being fenced",
        ));
    }
    if state.membership.local_maintenance_active() {
        return state
            .membership
            .enter_maintenance(&node_id)
            .await
            .map(Json)
            .map_err(api_error);
    }
    let preflight = collect_aggregate(&state).await?;
    let readiness = preflight
        .maintenance
        .iter()
        .find(|verdict| verdict.node_id == state.node_id)
        .ok_or_else(|| {
            ApiError::typed(
                StatusCode::CONFLICT,
                "maintenance_preflight_unsafe",
                "the target is absent from the direct cluster observations",
            )
        })?;
    if !readiness.safe_to_enter {
        let reason = readiness
            .blockers
            .first()
            .map(|finding| finding.message.as_str())
            .unwrap_or("the direct maintenance preflight did not prove this target safe");
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "maintenance_preflight_unsafe",
            format!("maintenance is not safe for this target: {reason}"),
        ));
    }
    state
        .serving
        .begin_restart_preparation(MAINTENANCE_PREPARATION_DURATION)
        .await;
    state.serving.wait_for_restart_admissions().await;
    match state.membership.enter_maintenance(&node_id).await {
        Ok(status) => Ok(Json(status)),
        Err(error) => {
            let active_sessions = local_owned_media_sessions(&state).await;
            state
                .serving
                .cancel_restart_preparation(active_sessions)
                .await;
            Err(api_error(error))
        }
    }
}

pub async fn exit_maintenance(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<Json<MembershipStatus>, ApiError> {
    if node_id != state.node_id {
        state.membership.status().await.map_err(api_error)?;
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "maintenance_node_mismatch",
            "maintenance can be cleared only by the running target process",
        ));
    }
    if !state.membership.local_maintenance_active() {
        let status = state.membership.status().await.map_err(api_error)?;
        if status
            .nodes
            .iter()
            .any(|node| node.node_id == node_id && node.maintenance)
        {
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "maintenance_resume_unsafe",
                "the running target has not acknowledged its maintenance fence",
            ));
        }
        return Ok(Json(status));
    }
    let active_sessions = local_owned_media_sessions(&state).await;
    let drain = state.serving.restart_drain_status(active_sessions).await;
    if !drain.drained {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "maintenance_resume_unsafe",
            "the running target still has local media work or an admission in flight",
        ));
    }
    let status = state
        .membership
        .exit_maintenance(&node_id)
        .await
        .map_err(api_error)?;
    state
        .serving
        .cancel_restart_preparation(active_sessions)
        .await;
    Ok(Json(status))
}

pub async fn force_election(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<MembershipStatus>, ApiError> {
    state
        .membership
        .force_election()
        .await
        .map(Json)
        .map_err(api_error)
}

/// Permanently remove this voter, then ask the daemon to drain and exit.
/// Membership removal is synchronous; shutdown is signalled only after the
/// Raft change has committed, so a refused leave keeps serving normally.
pub async fn leave(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(request): Json<LeaveRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if request.node_id != state.node_id {
        return Err(api_error(MembershipError::LeaveNodeMismatch));
    }
    state.membership.leave_node().await.map_err(api_error)?;
    state.shutdown.cancel();
    Ok(Json(serde_json::json!({
        "leaving": true,
        "node_id": state.node_id,
    })))
}

#[cfg(test)]
async fn require_no_owned_media_sessions(state: &AppState, node_id: &str) -> Result<(), ApiError> {
    let now_ms = crate::media_sessions::unix_ms();
    state.store.maintain_media_sessions(now_ms).await?;
    if state
        .store
        .owned_media_sessions(node_id, now_ms)
        .await?
        .is_empty()
    {
        Ok(())
    } else {
        Err(ApiError::typed(
            StatusCode::CONFLICT,
            "media_sessions_active",
            "release or move this node's active media sessions before removing it",
        ))
    }
}

/// The join token's SHA-256 digest is the credential for this route. The fresh
/// node decodes the full bearer locally and sends only proof of possession, so
/// the cluster secrets inside it never cross the public HTTP listener.
/// Requiring an account token would make a fresh node impossible to admit;
/// accepting either would widen the admin token into a node credential.
pub async fn redeem_join(
    State(state): State<AppState>,
    Json(request): Json<RedeemJoinRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .membership
        .redeem(&request)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(api_error)
}

pub async fn redeem_learner_join(
    State(state): State<AppState>,
    Json(request): Json<RedeemJoinRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .membership
        .redeem_learner(&request)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(api_error)
}

pub async fn finalize_join(
    State(state): State<AppState>,
    Json(request): Json<FinalizeJoinRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .membership
        .finalize(&request)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(api_error)
}

pub async fn finalize_learner_join(
    State(state): State<AppState>,
    Json(request): Json<FinalizeJoinRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .membership
        .finalize_learner(&request)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(api_error)
}

fn api_error(error: MembershipError) -> ApiError {
    let status = match error {
        MembershipError::Unavailable => StatusCode::CONFLICT,
        MembershipError::InvalidToken
        | MembershipError::Incompatible
        | MembershipError::InvalidHttpEndpoint => StatusCode::BAD_REQUEST,
        MembershipError::HttpEndpointInUse | MembershipError::NodeIdentityInUse => {
            StatusCode::CONFLICT
        }
        MembershipError::ExpiredToken => StatusCode::GONE,
        MembershipError::ReusedToken
        | MembershipError::ReservedToken
        | MembershipError::MembershipUpgradeRequired
        // Not "you asked the wrong node" and not "already active": a specific
        // set of nodes is behind, and the message names them.
        | MembershipError::LearnerProtocolUpgradeRequired(_)
        | MembershipError::LearnerProtocolInUse { .. }
        // Same shape: a named set of nodes is in the way, and none of them
        // will be in the way forever.
        | MembershipError::JoinInFlight(_)
        | MembershipError::LearnerProtocolNodeAbsent { .. }
        // A compare-and-swap this caller lost. Nothing is broken and the
        // request is worth repeating against a re-read status.
        | MembershipError::ProtocolRangeChanged
        // The node is real and the roster lists it; this release has no
        // removal path for a member with no vote. Not a 404.
        | MembershipError::NonVoterRemovalUnsupported(_)
        | MembershipError::PromotionRequiresLearner(_)
        | MembershipError::LearnerNotReady(_)
        | MembershipError::VoterStoragePreflight { .. }
        | MembershipError::LearnerLifecyclePending(_)
        // The cluster is fine and the request is well formed; the protocol
        // that admits a learner has simply not been activated yet.
        | MembershipError::LearnerProtocolInactive
        | MembershipError::RemovalPending(_)
        | MembershipError::LeaderRemoval
        | MembershipError::SelfRemovalRequiresLeave
        | MembershipError::LeaveNodeMismatch
        | MembershipError::LocalNodeNotActive
        | MembershipError::QuorumLoss
        | MembershipError::MaintenanceConflict(_)
        | MembershipError::MaintenanceWouldLoseQuorum(_)
        | MembershipError::MaintenanceResumeUnsafe(_)
        | MembershipError::ElectionQuorumUnavailable
        | MembershipError::ElectionCandidateUnavailable
        | MembershipError::ElectionLifecyclePending
        | MembershipError::ActiveMediaSessions
        | MembershipError::OfflineWork(_) => StatusCode::CONFLICT,
        MembershipError::NodeNotFound => StatusCode::NOT_FOUND,
        // Distinct from the roster refusals above: nothing is wrong with the
        // request, there is simply no leader to commit it right now.
        MembershipError::LeaderUnavailable
        | MembershipError::LeaderChanged(_)
        | MembershipError::Internal(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    ApiError::typed(status, error.code(), error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use plurx_core::domain::MediaSessionActivation;
    use plurx_core::store::SqliteStore;

    use super::*;

    fn state() -> AppState {
        let root = crate::test_temp_path(format!("plurx-cluster-route-{}", uuid::Uuid::new_v4()));
        AppState::new(
            "test".to_owned(),
            Arc::new(SqliteStore::open_in_memory().expect("session store")),
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
                runtime_cache: root.join("runtime"),
                renditions: root.join("renditions"),
            },
            "test-node".to_owned(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        )
    }

    #[tokio::test]
    async fn node_removal_is_blocked_while_it_owns_a_live_media_session() {
        let state = state();
        assert!(require_no_owned_media_sessions(&state, "test-node")
            .await
            .is_ok());
        let user = state
            .store
            .create_user("session-owner", "hash", false)
            .await
            .expect("create session owner");
        let now_ms = crate::media_sessions::unix_ms();
        state
            .store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: "00000000-0000-4000-8000-0000000000c1".to_owned(),
                session_id: "00000000-0000-4000-8000-0000000000d1".to_owned(),
                user_id: user.id,
                playback_id: "player-a".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "test-node".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms,
                lease_expires_at_ms: now_ms.saturating_add(12_000),
            })
            .await
            .expect("activate media route")
            .expect("media route outcome");

        match require_no_owned_media_sessions(&state, "test-node")
            .await
            .expect_err("owned session must fence removal")
        {
            ApiError::Typed { status, code, .. } => {
                assert_eq!(status, StatusCode::CONFLICT);
                assert_eq!(code, "media_sessions_active");
            }
            error => panic!("unexpected removal error: {error:?}"),
        }
    }

    #[test]
    fn maintenance_composes_the_direct_restart_fence_and_local_inventory() {
        let source = include_str!("cluster.rs")
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("test module")
            .0;
        let enter = source
            .split_once("pub async fn enter_maintenance(")
            .expect("enter maintenance")
            .1
            .split_once("pub async fn exit_maintenance(")
            .expect("enter maintenance end")
            .0;
        let begin = enter
            .find("begin_restart_preparation")
            .expect("restart fence begins");
        let wait = enter
            .find("wait_for_restart_admissions")
            .expect("in-flight admissions settle");
        let commit = enter
            .rfind(".enter_maintenance(&node_id)")
            .expect("durable maintenance commit");
        assert!(begin < wait && wait < commit);
        assert!(enter.contains("cancel_restart_preparation(active_sessions)"));
        assert!(enter.contains("node_id != state.node_id"));
        assert!(enter.contains(".maintenance"));
        assert!(!enter.contains("candidate_node_id"));
        assert!(!enter.contains("safe_to_restart_one"));

        let exit = source
            .split_once("pub async fn exit_maintenance(")
            .expect("exit maintenance")
            .1
            .split_once("pub async fn force_election(")
            .expect("exit maintenance end")
            .0;
        assert!(exit.contains("local_maintenance_active"));
        assert!(
            exit.find("local_owned_media_sessions") < exit.find(".exit_maintenance(&node_id)"),
            "local direct streams and offline work must drain before the durable fence clears"
        );
        assert!(exit.contains("node_id != state.node_id"));

        let inventory = include_str!("cluster_operations.rs")
            .split_once("pub(crate) async fn local_owned_media_sessions")
            .expect("local work inventory")
            .1
            .split_once("fn media_drain_status")
            .expect("local work inventory end")
            .0;
        for owner in ["state.transcode", "state.streams", "state.offline"] {
            assert!(
                inventory.contains(owner),
                "missing {owner} from direct drain"
            );
        }
    }
}
