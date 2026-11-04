//! Source admission observation, independent of any membership transport.
//! Native boot ownership and replicated observations are separate providers;
//! absence of the actual selected provider never grants admission.
use std::sync::Arc;

use plurx_core::{
    error::StoreError,
    sharing_source_sessions::{LocalSourceAuthority, SourceAdmissionMembers},
};

#[async_trait::async_trait]
pub(crate) trait SourceAuthorityProvider: Send + Sync {
    async fn observe(&self) -> Result<Option<SourceAdmissionMembers>, StoreError>;
}

#[derive(Clone)]
pub(crate) struct SourceAuthoritySource(Arc<dyn SourceAuthorityProvider>);
impl SourceAuthoritySource {
    pub(crate) fn new(provider: impl SourceAuthorityProvider + 'static) -> Self {
        Self(Arc::new(provider))
    }
    pub(crate) fn local(
        authority: Option<LocalSourceAuthority>,
        registry_boot_id: uuid::Uuid,
    ) -> Self {
        Self::new(LocalSourceProvider {
            authority,
            registry_boot_id,
        })
    }
    pub(crate) async fn observe(&self) -> Result<Option<SourceAdmissionMembers>, StoreError> {
        self.0.observe().await
    }
}

struct LocalSourceProvider {
    authority: Option<LocalSourceAuthority>,
    registry_boot_id: uuid::Uuid,
}
#[async_trait::async_trait]
impl SourceAuthorityProvider for LocalSourceProvider {
    async fn observe(&self) -> Result<Option<SourceAdmissionMembers>, StoreError> {
        self.authority
            .as_ref()
            .map(|authority| authority.admission(self.registry_boot_id))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::sharing_source_sessions::{SourceAdmissionAuthority, SourceOwnerIdentity};

    #[tokio::test]
    async fn missing_native_source_capability_is_unavailable_without_an_external_provider() {
        let registry = crate::sharing_connection_custody::AcceptedDriverRegistry::default();
        let provider = SourceAuthoritySource::local(None, registry.boot_id());
        assert!(provider.observe().await.expect("native provider").is_none());
    }

    #[tokio::test]
    async fn native_provider_retains_real_boot_registry_and_revocation_without_membership() {
        let root = crate::test_tempdir().expect("native Source directory");
        let handle = plurx_core::storage::open_native_store_at(
            root.path(),
            &root.path().join("credentials.key"),
        )
        .await
        .expect("actual native startup");
        let authority = handle
            .sqlite
            .prepare_native_source_before_serving(&handle.identity.node_id, &handle.credential_key)
            .await
            .expect("actual native Source factory");
        let registry = crate::sharing_connection_custody::AcceptedDriverRegistry::default();
        let provider = SourceAuthoritySource::local(Some(authority.clone()), registry.boot_id());
        let members = provider
            .observe()
            .await
            .expect("native observation")
            .expect("actual native capability");
        assert!(
            matches!(members.owner_identity(),SourceOwnerIdentity::Local(node) if node==handle.identity.node_id)
        );
        assert_eq!(members.local_registry_boot_id(), Some(registry.boot_id()));
        members
            .write_guard(crate::state::clock_ms(), 1, 2, 3)
            .expect("live local boot guard");
        authority.revoke();
        assert!(
            provider.observe().await.is_err(),
            "revoked local boot cannot fall back to another provider"
        );
        assert!(
            members
                .write_guard(crate::state::clock_ms(), 1, 2, 3)
                .is_err(),
            "retained local admission is revoked too"
        );
    }
}
