//! Admin cluster-membership API and the narrow join-token redemption path.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use plurx_core::cluster::membership::{
    FinalizeJoinRequest, IssuedJoinToken, MembershipError, MembershipStatus, RedeemJoinRequest,
};
use serde::Deserialize;

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;

#[derive(Deserialize, Default)]
#[serde(default)]
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

pub async fn nodes(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<MembershipStatus>, ApiError> {
    state.membership.status().await.map(Json).map_err(api_error)
}

pub async fn remove_node(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<Json<MembershipStatus>, ApiError> {
    require_no_owned_media_sessions(&state, &node_id).await?;
    state
        .membership
        .remove_voter(&node_id)
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
    require_no_owned_media_sessions(&state, &state.node_id).await?;
    state.membership.leave_voter().await.map_err(api_error)?;
    state.shutdown.cancel();
    Ok(Json(serde_json::json!({
        "leaving": true,
        "node_id": state.node_id,
    })))
}

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
        | MembershipError::RemovalPending(_)
        | MembershipError::LeaderRemoval
        | MembershipError::SelfRemovalRequiresLeave
        | MembershipError::LeaveNodeMismatch
        | MembershipError::LocalNodeNotActive
        | MembershipError::QuorumLoss
        | MembershipError::ActiveMediaSessions
        | MembershipError::OfflineWork(_) => StatusCode::CONFLICT,
        MembershipError::NodeNotFound => StatusCode::NOT_FOUND,
        MembershipError::Internal(_) => StatusCode::SERVICE_UNAVAILABLE,
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
        let root =
            std::env::temp_dir().join(format!("plurx-cluster-route-{}", uuid::Uuid::new_v4()));
        AppState::new(
            "test".to_owned(),
            Arc::new(SqliteStore::open_in_memory().expect("session store")),
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
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
}
