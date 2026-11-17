//! Core-owned cluster domains for server composition.
//!
//! These services retain their original consistency boundaries. Local facts,
//! applied directories and fresh mutation authority are distinct evidence.
//! No service exposes the underlying manager or a generic callback into it.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use super::directory::{AppliedStanding, RosterDirectory};
use super::{
    ActivityPeer, CacheAdminExclusionAdmission, CacheAdminRevocationCleanupOutcome,
    CacheAdminRevocationLease, ClockLeadershipIdentity, ClockPeerRoster, InternalPeerAuth,
    LocalServingRole, MembershipError, MembershipManager,
};
use crate::cluster::clock::ClusterClockGuard;
use crate::cluster::coordination::ClusterJobAuthority;
use crate::store::background_jobs::JobKind;

/// Private-handle domain access, constructed only from the existing source.
#[derive(Clone)]
pub struct ClusterAccess {
    manager: MembershipManager,
}

impl ClusterAccess {
    pub(super) fn new(manager: MembershipManager) -> Self {
        Self { manager }
    }

    #[must_use]
    pub fn local(&self) -> LocalNode {
        LocalNode {
            manager: self.manager.clone(),
        }
    }

    #[must_use]
    pub fn directory(&self, mark: Option<hiqlite::AppliedMarkHandle>) -> RosterDirectory {
        RosterDirectory::new(self.manager.clone(), mark)
    }

    #[must_use]
    pub fn jobs(&self) -> JobControl {
        JobControl {
            manager: self.manager.clone(),
        }
    }

    #[must_use]
    pub fn active_voter_authority(&self) -> ActiveVoterAuthority {
        ActiveVoterAuthority {
            manager: self.manager.clone(),
        }
    }

    #[must_use]
    pub fn clock_protocol(&self) -> ClockProtocol {
        ClockProtocol {
            manager: self.manager.clone(),
        }
    }

    #[must_use]
    pub fn revocation_exclusion(&self) -> RevocationExclusion {
        RevocationExclusion {
            manager: self.manager.clone(),
        }
    }

    #[must_use]
    pub fn administrative(&self) -> AdministrativeObservation {
        AdministrativeObservation {
            manager: self.manager.clone(),
        }
    }

    #[must_use]
    pub fn names(&self) -> ClusterNames {
        ClusterNames {
            manager: self.manager.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deployment {
    Standalone,
    Replicated,
}

/// Separate applied-standing and heartbeat/maintenance observations. These
/// fields are not claimed to be an atomic or linearizable snapshot.
pub struct LocalFacts {
    pub standing: AppliedStanding,
    pub admission: LocalServingRole,
    pub maintenance: bool,
}

/// Membership from one current local Raft metrics read. This carries no
/// consistent removal proof; active-voter authority is a separate service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedMembership {
    pub member: bool,
    pub voter: bool,
}

#[derive(Clone)]
pub struct LocalNode {
    manager: MembershipManager,
}

impl LocalNode {
    #[must_use]
    pub fn deployment(&self) -> Deployment {
        if self.manager.is_replicated() {
            Deployment::Replicated
        } else {
            Deployment::Standalone
        }
    }

    #[must_use]
    pub fn facts(&self) -> LocalFacts {
        let standing = self.manager.applied_standing();
        let maintenance = self.manager.local_maintenance_active();
        let admission = self.manager.published_serving_role();
        LocalFacts {
            standing,
            admission,
            maintenance,
        }
    }

    #[must_use]
    pub fn maintenance_fence(&self) -> bool {
        self.manager.local_maintenance_active()
    }

    pub fn admission(
        &self,
    ) -> impl Future<Output = Result<LocalServingRole, MembershipError>> + Send + '_ {
        self.manager.local_serving_role()
    }

    pub async fn committed(&self) -> Result<CommittedMembership, MembershipError> {
        let inner = self.manager.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        let voter = super::lifecycle::committed_voter(
            metrics.membership_config.voter_ids(),
            inner.identity.raft_id,
        );
        let member = metrics
            .membership_config
            .nodes()
            .any(|(raft_id, _)| *raft_id == inner.identity.raft_id);
        Ok(CommittedMembership { member, voter })
    }
}

/// Every decision delegates the existing core algorithm at call time. A
/// directory or an applied-standing bit never substitutes for job authority.
#[derive(Clone)]
pub struct JobControl {
    manager: MembershipManager,
}

impl JobControl {
    #[must_use]
    pub fn clustered(&self) -> bool {
        self.manager.is_replicated()
    }

    pub async fn reconciliation_members(
        &self,
        local: &str,
    ) -> Result<BTreeSet<String>, MembershipError> {
        let mut members = BTreeSet::from([local.to_owned()]);
        if self.clustered() {
            members.extend(
                self.manager
                    .operations_peers()
                    .await?
                    .into_iter()
                    .map(|peer| peer.node_id),
            );
        }
        Ok(members)
    }

    /// Artifact placement retains stale/unreachable members; callers apply
    /// their own existing reachability and endpoint requirements.
    pub fn artifact_workers(
        &self,
    ) -> impl Future<Output = Result<Vec<ActivityPeer>, MembershipError>> + Send + '_ {
        self.manager.operations_peers()
    }
}

#[async_trait::async_trait]
impl ClusterJobAuthority for JobControl {
    async fn may_run_cluster_jobs(&self) -> bool {
        ClusterJobAuthority::may_run_cluster_jobs(&self.manager).await
    }

    async fn may_execute_job(&self, kind: JobKind) -> bool {
        ClusterJobAuthority::may_execute_job(&self.manager, kind).await
    }
}

#[derive(Clone)]
pub struct ActiveVoterAuthority {
    manager: MembershipManager,
}

impl ActiveVoterAuthority {
    /// Fresh committed-voter and consistent removal checks, not an advisory
    /// standing observation.
    pub fn confirm(&self) -> impl Future<Output = Result<bool, MembershipError>> + Send + '_ {
        self.manager.local_node_is_active_voter()
    }
}

#[derive(Clone)]
pub struct ClockChanges {
    metrics: hiqlite::LocalDbRaftMetrics,
}

impl ClockChanges {
    #[must_use]
    pub fn applied_index(&self) -> Option<u64> {
        self.metrics.snapshot().last_applied_index
    }
    #[must_use]
    pub fn live(&self) -> bool {
        self.metrics.membership_snapshot().running
    }
    pub fn changed(&mut self) -> impl Future<Output = bool> + Send + '_ {
        self.metrics.wait_for_change()
    }
}

/// Guard, leadership, change observation and proof reads share one manager.
#[derive(Clone)]
pub struct ClockProtocol {
    manager: MembershipManager,
}

impl ClockProtocol {
    #[must_use]
    pub fn guard(&self) -> Arc<ClusterClockGuard> {
        self.manager.clock_guard()
    }
    #[must_use]
    pub fn leadership(&self) -> Option<ClockLeadershipIdentity> {
        self.manager.clock_leadership_identity()
    }
    #[must_use]
    pub fn deployment(&self) -> Deployment {
        if self.manager.is_replicated() {
            Deployment::Replicated
        } else {
            Deployment::Standalone
        }
    }
    #[must_use]
    pub fn changes(&self) -> Option<ClockChanges> {
        self.manager
            .local_db_raft_metrics()
            .map(|metrics| ClockChanges { metrics })
    }
    pub fn prove_roster(
        &self,
    ) -> impl Future<Output = Result<ClockPeerRoster, MembershipError>> + Send + '_ {
        self.manager.clock_peers()
    }
    pub fn applied_hint(
        &self,
    ) -> impl Future<Output = Result<ClockPeerRoster, MembershipError>> + Send + '_ {
        self.manager.clock_peers_hint()
    }
    pub fn after_authenticated_request<'a>(
        &'a self,
        auth: &'a InternalPeerAuth,
    ) -> impl Future<Output = Result<ClockPeerRoster, MembershipError>> + Send + 'a {
        self.manager.clock_peers_after_authenticated_request(auth)
    }
    pub fn authorize_request<'a>(
        &'a self,
        auth: &'a InternalPeerAuth,
        method: &'a str,
        path: &'a str,
        body: &'a [u8],
    ) -> impl Future<Output = Result<bool, MembershipError>> + Send + 'a {
        self.manager
            .authorize_internal_peer_request(auth, method, path, body)
    }
    pub fn request_still_fresh(&self, auth: &InternalPeerAuth) -> Result<bool, MembershipError> {
        self.manager.authenticated_clock_request_still_fresh(auth)
    }
    pub fn sign_reply(
        &self,
        target: &str,
        nonce: &str,
        path: &str,
        response: &[u8],
    ) -> Result<String, MembershipError> {
        self.manager
            .sign_internal_peer_response(target, nonce, path, response)
    }
}

/// The claim-bound exclusion protocol; leases retain their existing exact
/// identity, admission deadline, apply acknowledgement and cleanup outcomes.
#[derive(Clone)]
pub struct RevocationExclusion {
    manager: MembershipManager,
}

impl RevocationExclusion {
    #[must_use]
    pub fn deployment(&self) -> Deployment {
        if self.manager.is_replicated() {
            Deployment::Replicated
        } else {
            Deployment::Standalone
        }
    }
    pub fn prepare(
        &self,
        node: &str,
        duration: Duration,
    ) -> Result<CacheAdminRevocationLease, MembershipError> {
        MembershipManager::prepare_cache_admin_revocation_claim(node, duration)
    }
    pub fn admit<'a>(
        &'a self,
        lease: &'a mut CacheAdminRevocationLease,
        deadline: tokio::time::Instant,
    ) -> impl Future<Output = Result<CacheAdminExclusionAdmission, MembershipError>> + Send + 'a
    {
        self.manager
            .admit_cache_admin_revocation_lease(lease, deadline)
    }
    pub fn release<'a>(
        &'a self,
        lease: &'a CacheAdminRevocationLease,
    ) -> impl Future<Output = Result<CacheAdminRevocationCleanupOutcome, MembershipError>> + Send + 'a
    {
        self.manager.release_cache_admin_revocation_lease(lease)
    }
    pub fn claim_roster<'a>(
        &'a self,
        claim: &'a str,
    ) -> impl Future<Output = Result<Vec<ActivityPeer>, MembershipError>> + Send + 'a {
        self.manager.cache_admin_revocation_peers(claim)
    }
    pub fn blockers(
        &self,
    ) -> impl Future<Output = Result<Vec<String>, MembershipError>> + Send + '_ {
        self.manager.cache_admin_exclusion_blockers()
    }
    pub fn activation_ready(
        &self,
    ) -> impl Future<Output = Result<bool, MembershipError>> + Send + '_ {
        self.manager.cache_admin_revocation_activation_ready()
    }
    pub fn activated_locally(
        &self,
    ) -> impl Future<Output = Result<bool, MembershipError>> + Send + '_ {
        self.manager.cache_admin_revocation_activated_locally()
    }
    pub fn claim_applied<'a>(
        &'a self,
        claim: &'a str,
    ) -> impl Future<Output = Result<bool, MembershipError>> + Send + 'a {
        self.manager.cache_admin_revocation_claim_applied(claim)
    }
}

/// Naming and delivery origins deliberately include eligible non-Raft rows.
/// A voter-only directory is not a replacement for these local SQL rules.
#[derive(Clone)]
pub struct ClusterNames {
    manager: MembershipManager,
}

impl ClusterNames {
    pub fn delivery_origins(
        &self,
    ) -> impl Future<Output = Result<Vec<String>, MembershipError>> + Send + '_ {
        self.manager.reachable_peer_http_urls()
    }
    pub fn node_names(
        &self,
    ) -> impl Future<Output = Result<BTreeMap<String, String>, MembershipError>> + Send + '_ {
        self.manager.node_hostnames()
    }
}

/// Fresh administrative preflight observations. These do not replace the
/// caller's active-voter check, exact claim or original absolute deadline.
#[derive(Clone)]
pub struct AdministrativeObservation {
    manager: MembershipManager,
}
impl AdministrativeObservation {
    pub fn topology(
        &self,
    ) -> impl Future<Output = Result<super::MembershipStatus, MembershipError>> + Send + '_ {
        self.manager.status()
    }
    pub fn targets(
        &self,
    ) -> impl Future<Output = Result<Vec<ActivityPeer>, MembershipError>> + Send + '_ {
        self.manager.operations_peers()
    }
}
