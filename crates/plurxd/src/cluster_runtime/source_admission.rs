//! Actual replicated Source observations, retained outside local admission.
use crate::source_authority::{SourceAuthorityProvider, SourceAuthoritySource};
use plurx_core::{
    cluster::membership::MembershipManager, error::StoreError,
    sharing_source_sessions::SourceAdmissionMembers,
};

struct ReplicatedSourceProvider {
    membership: MembershipManager,
}
#[async_trait::async_trait]
impl SourceAuthorityProvider for ReplicatedSourceProvider {
    async fn observe(&self) -> Result<Option<SourceAdmissionMembers>, StoreError> {
        self.membership
            .observe_source_admission_members()
            .await
            .map(|members| members.map(Into::into))
            .map_err(|error| StoreError::Task(error.to_string()))
    }
}
pub(crate) fn from_membership(membership: MembershipManager) -> SourceAuthoritySource {
    SourceAuthoritySource::new(ReplicatedSourceProvider { membership })
}
