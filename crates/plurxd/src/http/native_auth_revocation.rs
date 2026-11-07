//! Native credential revocation owns the real local recovery-proof fence.
//! No peer roster, replicated exclusion or cluster transport is constructed.
use super::{
    error::ApiError,
    extract::{record_revocation_complete, CacheOnlyAdminRevocation, RevocationAdmission},
};
use crate::state::AppState;
use axum::http::StatusCode;
use plurx_core::store::CacheAdminMutationClaim;

pub(crate) struct NativeCacheRevocation {
    local: Option<CacheOnlyAdminRevocation>,
    _operation: RevocationAdmission,
}

impl NativeCacheRevocation {
    async fn begin(
        state: &AppState,
        local: impl FnOnce() -> CacheOnlyAdminRevocation,
    ) -> Result<Self, ApiError> {
        let operation = state
            .cache_only_admin_proofs
            .acquire_revocation_operation()
            .await
            .map_err(|_| {
                ApiError::typed(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "admin_revocation_propagation_failed",
                    "the local credential revocation operation is unavailable",
                )
            })?;
        let mut local = local();
        // Cancellation or an uncertain durable write keeps recovery proofs
        // fenced for the cache's existing bounded ambiguity lifetime.
        local.arm_ambiguity();
        Ok(Self {
            local: Some(local),
            _operation: operation,
        })
    }

    pub(crate) async fn begin_digest(state: &AppState, digest: &str) -> Result<Self, ApiError> {
        let result = Self::begin(state, || {
            state
                .cache_only_admin_proofs
                .begin_digest_revocation(digest)
        })
        .await?;
        #[cfg(test)]
        if let Some(barrier) = &state.cache_revocation_test_barrier {
            barrier.wait().await;
            tokio::time::sleep(std::time::Duration::from_secs(40)).await;
        }
        Ok(result)
    }

    pub(crate) async fn begin_user(state: &AppState, user_id: i64) -> Result<Self, ApiError> {
        Self::begin(state, || {
            state.cache_only_admin_proofs.begin_user_revocation(user_id)
        })
        .await
    }

    pub(crate) async fn finish(mut self, _state: &AppState) -> Result<(), ApiError> {
        self.local
            .take()
            .expect("native revocation owns its local fence")
            .complete();
        record_revocation_complete();
        Ok(())
    }

    pub(crate) fn mutation_claim(&self) -> Option<&CacheAdminMutationClaim> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_revocation_brackets_real_store_write_without_membership_claim() {
        let state = super::super::tests::test_state_without_router();
        assert!(!state.is_replicated());
        let admin = state
            .store
            .create_user("native-admin", "hash", true)
            .await
            .expect("native admin");
        let digest = plurx_core::auth::hash_token("native-admin-token");
        state
            .store
            .create_token(&digest, admin.id, None)
            .await
            .expect("native login");
        let cache = &state.cache_only_admin_proofs;
        let before = cache.authentication_ticket();
        cache.record_authenticated(before, digest.clone(), &admin);
        assert!(cache.authenticate(&digest));

        let revocation = NativeCacheRevocation::begin_digest(&state, &digest)
            .await
            .expect("native local fence");
        assert!(revocation.mutation_claim().is_none());
        assert!(!cache.authenticate(&digest));
        assert!(cache.authentication_ticket().is_none());
        state
            .store
            .delete_token_with_cache_admin_claim(&digest, revocation.mutation_claim())
            .await
            .expect("real durable native logout");
        revocation
            .finish(&state)
            .await
            .expect("definite local completion");
        assert!(cache.authentication_ticket().is_some());
        cache.record_authenticated(before, digest.clone(), &admin);
        assert!(
            !cache.authenticate(&digest),
            "an old authentication cannot republish the revoked proof"
        );
        assert!(state
            .store
            .user_for_token(&digest)
            .await
            .expect("native token read")
            .is_none());
    }

    #[tokio::test]
    async fn cancelled_native_revocation_retains_ambiguity_and_releases_operation_owner() {
        let state = super::super::tests::test_state_without_router();
        let revocation = NativeCacheRevocation::begin_user(&state, 7)
            .await
            .expect("native operation owner");
        assert!(state
            .cache_only_admin_proofs
            .authentication_ticket()
            .is_none());
        drop(revocation);
        assert!(
            state
                .cache_only_admin_proofs
                .authentication_ticket()
                .is_none(),
            "an uncertain Store outcome keeps recovery proofs fenced"
        );
        let next = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.cache_only_admin_proofs.acquire_revocation_operation(),
        )
        .await
        .expect("cancelled operation does not strand its mutex")
        .expect("next native operation");
        drop(next);
    }
}
