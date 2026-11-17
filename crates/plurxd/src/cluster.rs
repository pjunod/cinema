//! The cluster facade: the one place feature code asks the cluster anything.
//!
//! Build contract: `docs/cluster/CLUSTER-FACADE-BUILD.md` (audit items B1 and
//! B2 of `docs/cluster/CLUSTER-LAYER-AUDIT-2026-11-14.md`).
//!
//! # The six questions
//!
//! Feature code — HLS, Live TV, sharing, jobs, artwork, the UI — asks the
//! cluster six questions, and asks them of `state.cluster`:
//!
//! | Question | Method | Slice |
//! |---|---|---|
//! | May this node serve or commit now? | `authority()` | 1 |
//! | Is this node a voter, a learner, standalone or fenced? | `role()` | 1 |
//! | Who can help, for this purpose? | `peers(purpose)` | 1 |
//! | Read this, applied or linearizable? | `read(class)` | B3, reserved |
//! | Tell me when this table changes | `watch(table)` | B4, reserved |
//! | Call that peer | `call(peer, ..)` | A5/B5, reserved |
//!
//! The reserved names have no body yet. A feature that needs one of them
//! today uses the existing route and is counted by the census below.
//!
//! # Who may touch the raw parts
//!
//! The `membership`, `replication` and `serving` handles, the Raft metrics
//! types, `hiqlite::` and `PeerTransport::new` belong to this module, the
//! serving fence and the peer transport. Every other production file is
//! counted by `cluster::census`: its reach-arounds are pinned per file and
//! per class, a new one fails the census, and the total (`CEILING`) may only
//! shrink (`tests/operations/test_cluster_facade_ratchet.py`). A file whose
//! count falls must lower its pin in the same change.
//!
//! # Staleness
//!
//! `peers()` answers from this node's applied replica: never ahead of what
//! it has applied, behind the leader by its apply lag. It does not consult
//! the serving fence, as the rosters it replaces did not — a node without a
//! serving proof still needs its roster to render status and to recover.
//! Callers that place work already hold `authority()`. Reachability and
//! progress freshness are functions of the clock, so they are evaluated at
//! every call, also on a cached roster.
//!
//! # What stays linearizable
//!
//! These are never `peers()`; each becomes an explicit facade method in a
//! later slice: the cache-admin revocation roster, the clock rosters, every
//! internal-peer authorizer, `local_node_is_active_voter`, the learner branch
//! of job execution, maintenance reconciliation, the sharing member and
//! custody observations, and every membership mutation.

use plurx_core::cluster::membership::directory::{
    roster_now, AppliedStanding, DirectoryReadCounts, RosterDirectory, RosterSnapshot,
};
use plurx_core::cluster::membership::{
    ActivityPeer, LocalServingRole, MembershipError, MembershipManager,
};

use crate::serving_fence::{ServingAuthority, ServingFence};

#[cfg(test)]
mod census;
mod operations {
    //! Typed administrative capabilities of the cluster facade.
    //!
    //! These delegates keep the existing mutation, refusal and freshness rules.
    //! Request-specific ownership and admission ordering stays with its current
    //! caller until the complete operation migration is qualified.

    use crate::serving_fence::{PlannedOutageFenceToken, RestartDrainStatus};
    use plurx_core::cluster::membership::{ClusterOperationCleanupOutcome, ClusterOperationLease};
    use std::time::Duration;

    use plurx_core::cluster::membership::{
        FinalizeJoinRequest, IssuedJoinToken, MembershipError, MembershipStatus, ProtocolChange,
        RedeemJoinRequest,
    };

    /// A control capability over the same mechanisms as the parent facade.
    /// Only the facade constructs it; callers receive explicit typed operations.
    #[derive(Clone)]
    pub(crate) struct Operations {
        cluster: super::Cluster,
    }

    impl Operations {
        pub(super) fn new(cluster: super::Cluster) -> Self {
            Self { cluster }
        }

        pub(crate) fn clock(&self) -> ClockOperations {
            ClockOperations::new(self.cluster.membership.clone())
        }

        pub(crate) fn revocation(&self) -> RevocationOperations {
            RevocationOperations {
                manager: self.cluster.membership.clone(),
            }
        }

        pub(crate) async fn issue_voter_token(
            &self,
            ttl: Duration,
        ) -> Result<IssuedJoinToken, MembershipError> {
            self.cluster.membership.issue_token(ttl).await
        }
        pub(crate) async fn issue_learner_join(
            &self,
            ttl: Duration,
        ) -> Result<IssuedJoinToken, MembershipError> {
            self.cluster.membership.issue_learner_token(ttl).await
        }
        pub(crate) async fn enable_learner_protocol(
            &self,
        ) -> Result<ProtocolChange, MembershipError> {
            self.cluster.membership.activate_learner_protocol().await
        }
        pub(crate) async fn disable_learner_protocol(
            &self,
        ) -> Result<ProtocolChange, MembershipError> {
            self.cluster.membership.deactivate_learner_protocol().await
        }
        pub(crate) async fn remove_member(
            &self,
            node_id: &str,
        ) -> Result<MembershipStatus, MembershipError> {
            self.cluster.membership.remove_node(node_id).await
        }
        pub(crate) async fn promote_member(
            &self,
            node_id: &str,
        ) -> Result<MembershipStatus, MembershipError> {
            self.cluster.membership.promote_learner(node_id).await
        }
        pub(crate) async fn resume_member(
            &self,
            node_id: &str,
        ) -> Result<MembershipStatus, MembershipError> {
            self.cluster.membership.exit_maintenance(node_id).await
        }
        pub(crate) async fn elect_member(&self) -> Result<MembershipStatus, MembershipError> {
            self.cluster.membership.force_election().await
        }
        pub(crate) async fn leave_member(&self) -> Result<(), MembershipError> {
            self.cluster.membership.leave_node().await
        }
        pub(crate) async fn redeem_voter(
            &self,
            request: &RedeemJoinRequest,
        ) -> Result<(), MembershipError> {
            self.cluster.membership.redeem(request).await
        }
        pub(crate) async fn redeem_learner_join(
            &self,
            request: &RedeemJoinRequest,
        ) -> Result<(), MembershipError> {
            self.cluster.membership.redeem_learner(request).await
        }
        pub(crate) async fn finalize_voter(
            &self,
            request: &FinalizeJoinRequest,
        ) -> Result<(), MembershipError> {
            self.cluster.membership.finalize(request).await
        }
        pub(crate) async fn finalize_learner_join(
            &self,
            request: &FinalizeJoinRequest,
        ) -> Result<(), MembershipError> {
            self.cluster.membership.finalize_learner(request).await
        }

        /// Signed-in client failover uses the original artwork endpoint rule,
        /// with its caller's separate output bound. This is a routing hint,
        /// never mutation or peer-authentication authority.
        pub(crate) async fn ingress_targets(&self) -> Result<Vec<String>, MembershipError> {
            self.cluster.artwork().targets().await
        }
    }

    /// Clock authority also exists during startup, before the full application.
    /// A clone retains the original clock guard and membership identity.
    #[derive(Clone)]
    pub(crate) struct ClockOperations {
        manager: plurx_core::cluster::membership::MembershipManager,
    }

    impl ClockOperations {
        pub(super) fn new(manager: plurx_core::cluster::membership::MembershipManager) -> Self {
            Self { manager }
        }

        pub(crate) fn guard(
            &self,
        ) -> std::sync::Arc<plurx_core::cluster::clock::ClusterClockGuard> {
            self.manager.clock_guard()
        }

        pub(crate) fn same_source(&self, other: &Self) -> bool {
            std::sync::Arc::ptr_eq(&self.guard(), &other.guard())
        }

        pub(crate) fn leadership(
            &self,
        ) -> Option<plurx_core::cluster::membership::ClockLeadershipIdentity> {
            self.manager.clock_leadership_identity()
        }

        pub(crate) fn clustered(&self) -> bool {
            self.manager.is_replicated()
        }

        pub(crate) fn change_source(&self) -> crate::settings_changes::SettingsChangeSource {
            match self.manager.local_db_raft_metrics() {
                Some(applied) => crate::settings_changes::SettingsChangeSource::Applied(applied),
                None => crate::settings_changes::SettingsChangeSource::LocalOnly,
            }
        }

        pub(crate) async fn roster(
            &self,
        ) -> Result<plurx_core::cluster::membership::ClockPeerRoster, MembershipError> {
            self.manager.clock_peers().await
        }

        pub(crate) async fn roster_hint(
            &self,
        ) -> Result<plurx_core::cluster::membership::ClockPeerRoster, MembershipError> {
            self.manager.clock_peers_hint().await
        }

        pub(crate) async fn roster_after_auth(
            &self,
            auth: &plurx_core::cluster::membership::InternalPeerAuth,
        ) -> Result<plurx_core::cluster::membership::ClockPeerRoster, MembershipError> {
            self.manager
                .clock_peers_after_authenticated_request(auth)
                .await
        }

        pub(crate) async fn authorize_clock_request(
            &self,
            auth: &plurx_core::cluster::membership::InternalPeerAuth,
            method: &str,
            path: &str,
            body: &[u8],
        ) -> Result<bool, MembershipError> {
            self.manager
                .authorize_internal_peer_request(auth, method, path, body)
                .await
        }

        pub(crate) fn request_still_fresh(
            &self,
            auth: &plurx_core::cluster::membership::InternalPeerAuth,
        ) -> Result<bool, MembershipError> {
            self.manager.authenticated_clock_request_still_fresh(auth)
        }

        pub(crate) fn sign_clock_reply(
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

    /// Exact claim-bound revocation authority; never a cached directory roster.
    #[derive(Clone)]
    pub(crate) struct RevocationOperations {
        manager: plurx_core::cluster::membership::MembershipManager,
    }

    impl RevocationOperations {
        pub(crate) fn clustered(&self) -> bool {
            self.manager.is_replicated()
        }

        pub(crate) fn prepare_exclusion(
            &self,
            node_id: &str,
            duration: Duration,
        ) -> Result<plurx_core::cluster::membership::CacheAdminRevocationLease, MembershipError>
        {
            plurx_core::cluster::membership::MembershipManager::prepare_cache_admin_revocation_claim(
                node_id, duration,
            )
        }

        pub(crate) async fn admit_exclusion(
            &self,
            lease: &mut plurx_core::cluster::membership::CacheAdminRevocationLease,
            deadline: tokio::time::Instant,
        ) -> Result<plurx_core::cluster::membership::CacheAdminExclusionAdmission, MembershipError>
        {
            self.manager
                .admit_cache_admin_revocation_lease(lease, deadline)
                .await
        }

        pub(crate) async fn release_exclusion(
            &self,
            lease: &plurx_core::cluster::membership::CacheAdminRevocationLease,
        ) -> Result<
            plurx_core::cluster::membership::CacheAdminRevocationCleanupOutcome,
            MembershipError,
        > {
            self.manager
                .release_cache_admin_revocation_lease(lease)
                .await
        }

        pub(crate) async fn claim_roster(
            &self,
            claim_id: &str,
        ) -> Result<Vec<plurx_core::cluster::membership::ActivityPeer>, MembershipError> {
            self.manager.cache_admin_revocation_peers(claim_id).await
        }

        pub(crate) async fn blockers(&self) -> Result<Vec<String>, MembershipError> {
            self.manager.cache_admin_exclusion_blockers().await
        }

        pub(crate) async fn activation_ready(&self) -> Result<bool, MembershipError> {
            self.manager.cache_admin_revocation_activation_ready().await
        }

        pub(crate) async fn activated_locally(&self) -> Result<bool, MembershipError> {
            self.manager
                .cache_admin_revocation_activated_locally()
                .await
        }

        pub(crate) async fn claim_applied(&self, claim_id: &str) -> Result<bool, MembershipError> {
            self.manager
                .cache_admin_revocation_claim_applied(claim_id)
                .await
        }
    }

    impl Operations {
        pub(crate) fn claim_outage_operation(
            &self,
        ) -> Result<tokio::sync::OwnedMutexGuard<()>, &'static str> {
            self.cluster.fence.try_planned_outage_operation_guard()
        }
        pub(crate) async fn claim_outage_cleanup(&self) -> tokio::sync::OwnedMutexGuard<()> {
            self.cluster.fence.planned_outage_cleanup_guard().await
        }
        pub(crate) async fn prepare_restart_fence(
            &self,
            expires_at_unix_ms: u64,
        ) -> Option<PlannedOutageFenceToken> {
            self.cluster
                .fence
                .begin_restart_preparation_until(expires_at_unix_ms)
                .await
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        pub(crate) async fn prepare_desktop_fence(
            &self,
            expires_at_unix_ms: u64,
        ) -> Option<PlannedOutageFenceToken> {
            self.cluster
                .fence
                .begin_desktop_preparation_until(expires_at_unix_ms)
                .await
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        pub(crate) async fn commit_desktop_fence(
            &self,
            token: PlannedOutageFenceToken,
            active_sessions: usize,
        ) -> bool {
            self.cluster
                .fence
                .commit_desktop_shutdown(token, active_sessions)
                .await
        }
        pub(crate) async fn prepare_maintenance_fence(
            &self,
            expires_at_unix_ms: u64,
        ) -> Option<PlannedOutageFenceToken> {
            self.cluster
                .fence
                .begin_maintenance_preparation_until(expires_at_unix_ms)
                .await
        }
        pub(crate) fn retain_outage_fence(&self, token: PlannedOutageFenceToken) {
            self.cluster
                .fence
                .retain_planned_outage_preparation_until_cancelled(token)
        }
        pub(crate) fn resolve_outage_release(&self, token: PlannedOutageFenceToken) {
            self.cluster.fence.resolve_planned_outage_release(token)
        }
        pub(crate) async fn settle_outage_admissions(
            &self,
            token: PlannedOutageFenceToken,
            shutdown: &tokio_util::sync::CancellationToken,
        ) -> bool {
            self.cluster
                .fence
                .wait_for_restart_admissions_until(token, shutdown)
                .await
        }
        pub(crate) async fn cancel_outage_fence(
            &self,
            token: PlannedOutageFenceToken,
            active_sessions: usize,
        ) -> RestartDrainStatus {
            self.cluster
                .fence
                .cancel_planned_outage_preparation(token, active_sessions)
                .await
        }
        pub(crate) async fn cancel_restart_fence(
            &self,
            cancellation: PlannedOutageFenceToken,
            active_sessions: usize,
        ) -> RestartDrainStatus {
            self.cluster
                .fence
                .cancel_current_restart_preparation(cancellation, active_sessions)
                .await
        }
        pub(crate) async fn cancel_maintenance_fence(
            &self,
            active_sessions: usize,
        ) -> RestartDrainStatus {
            self.cluster
                .fence
                .cancel_current_maintenance_preparation(active_sessions)
                .await
        }
        pub(crate) async fn drain_status(&self, active_sessions: usize) -> RestartDrainStatus {
            self.cluster
                .fence
                .restart_drain_status(active_sessions)
                .await
        }
        pub(crate) fn retain_restart_cancel(&self) -> PlannedOutageFenceToken {
            self.cluster.fence.retain_restart_cancellation()
        }
        pub(crate) async fn outage_is_current(&self, token: PlannedOutageFenceToken) -> bool {
            self.cluster.fence.planned_outage_is_current(token).await
        }
        pub(crate) async fn commit_outage_claim(
            &self,
            lease: &ClusterOperationLease,
        ) -> Result<(), MembershipError> {
            self.cluster
                .membership
                .commit_cluster_operation_lease(lease)
                .await
        }
        pub(crate) async fn release_outage_claim(
            &self,
            lease: &ClusterOperationLease,
        ) -> Result<ClusterOperationCleanupOutcome, MembershipError> {
            self.cluster
                .membership
                .release_cluster_operation_lease(lease)
                .await
        }
        pub(crate) async fn release_restart_claim(
            &self,
            node_id: &str,
        ) -> Result<ClusterOperationCleanupOutcome, MembershipError> {
            self.cluster
                .membership
                .release_restart_preparation(node_id)
                .await
        }
        pub(crate) async fn commit_maintenance(
            &self,
            node_id: &str,
            lease: &ClusterOperationLease,
        ) -> Result<MembershipStatus, MembershipError> {
            self.cluster
                .membership
                .enter_maintenance(node_id, lease)
                .await
        }
        pub(crate) async fn reconcile_maintenance(&self) -> Result<bool, MembershipError> {
            self.cluster
                .membership
                .reconcile_local_maintenance_commit()
                .await
        }

        pub(crate) fn prepare_restart_claim(
            &self,
            node_id: &str,
            duration: Duration,
        ) -> Result<ClusterOperationLease, MembershipError> {
            plurx_core::cluster::membership::MembershipManager::prepare_restart_preparation_claim(
                node_id, duration,
            )
        }
        pub(crate) fn prepare_maintenance_claim(
            &self,
            node_id: &str,
            duration: Duration,
        ) -> Result<ClusterOperationLease, MembershipError> {
            plurx_core::cluster::membership::MembershipManager::prepare_maintenance_preparation_claim(
            node_id, duration,
        )
        }
        pub(crate) fn ready(&self) -> bool {
            self.cluster.ready()
        }
        pub(crate) fn clustered(&self) -> bool {
            self.cluster.membership.is_replicated()
        }
        pub(crate) async fn topology(&self) -> Result<MembershipStatus, MembershipError> {
            self.cluster.membership_status().await
        }
        /// Always fresh: mutation preflight must not use the cached directory.
        pub(crate) async fn fresh_targets(
            &self,
        ) -> Result<Vec<plurx_core::cluster::membership::ActivityPeer>, MembershipError> {
            self.cluster.membership.operations_peers().await
        }
        pub(crate) async fn revocation_capable(&self) -> Result<bool, MembershipError> {
            self.cluster.revocation_ready().await
        }
        #[cfg(test)]
        pub(crate) fn for_local_fence_test(fence: crate::serving_fence::ServingFence) -> Self {
            Self::new(super::Cluster::new(
                fence,
                plurx_core::cluster::membership::MembershipManager::unavailable(),
                plurx_core::cluster::migration::status::ReplicationMonitor::sqlite(),
                None,
            ))
        }
    }
}
pub(crate) use operations::{ClockOperations, Operations, RevocationOperations};

/// Pre-AppState clock authority shares the original manager's guard identity.
pub(crate) fn startup_clock(membership: MembershipManager) -> ClockOperations {
    ClockOperations::new(membership)
}

/// The facade. Cheap to clone; clones share one peer directory.
#[derive(Clone)]
pub(crate) struct Cluster {
    fence: ServingFence,
    membership: MembershipManager,
    directory: Directory,
    replication: plurx_core::cluster::migration::status::ReplicationMonitor,
}

impl Cluster {
    /// The facade over this node's fence and manager; `mark` is the applied
    /// mark the peer directory stamps with (`AppliedView::applied_mark`).
    pub(crate) fn new(
        fence: ServingFence,
        membership: MembershipManager,
        replication: plurx_core::cluster::migration::status::ReplicationMonitor,
        mark: Option<hiqlite::AppliedMarkHandle>,
    ) -> Self {
        Self {
            directory: Directory::with_mark(membership.clone(), mark),
            fence,
            membership,
            replication,
        }
    }

    /// May this node serve or commit now: the serving fence's authority.
    pub(crate) fn authority(&self) -> ServingAuthority {
        self.fence.authority()
    }

    /// Observe serving transitions through the same fence as admission.
    pub(crate) fn subscribe_authority(
        &self,
    ) -> tokio::sync::watch::Receiver<crate::serving_fence::ServingState> {
        self.fence.subscribe()
    }

    /// HTTP admission keeps the fence's restart-preparation semantics. The
    /// projected authority has a different admission rule for existing DVR
    /// work, so this must delegate to the fence itself.
    pub(crate) async fn try_restart_admission(
        &self,
    ) -> Option<crate::serving_fence::RestartAdmission> {
        self.fence.try_restart_admission().await
    }

    /// This node's standing and admission, from in-process state only: the
    /// applied Raft membership (the source `local_node_is_committed_voter`
    /// reads) and the two atomics the heartbeat publishes. No IO, no await.
    pub(crate) fn role(&self) -> LocalRole {
        let standing = match self.membership.applied_standing() {
            AppliedStanding::Standalone => Standing::Standalone,
            AppliedStanding::Voter => Standing::Voter,
            AppliedStanding::Learner => Standing::Learner,
            AppliedStanding::Absent => Standing::Absent,
        };
        let admission = if self.membership.local_maintenance_active() {
            Admission::Maintenance
        } else if self.membership.published_serving_role() == LocalServingRole::Fenced {
            Admission::Fenced
        } else {
            Admission::Open
        };
        LocalRole {
            standing,
            admission,
        }
    }

    /// The heartbeat-published role, preserving its standalone value independently
    /// of the combined standing/maintenance projection in `role()`.
    pub(crate) async fn admission_role(&self) -> Result<LocalServingRole, MembershipError> {
        self.membership.local_serving_role().await
    }

    pub(crate) fn maintenance_active(&self) -> bool {
        self.membership.local_maintenance_active()
    }

    pub(crate) fn deployment_enabled(&self, configured: bool) -> bool {
        self.membership.cluster_enabled(configured)
    }

    pub(crate) fn quorum_managed(&self) -> bool {
        self.fence.is_quorum_managed()
    }
    pub(crate) fn ready(&self) -> bool {
        self.fence.is_ready()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) async fn desktop_admission(&self) -> Option<crate::serving_fence::RestartAdmission> {
        self.fence.try_desktop_http_admission().await
    }

    pub(crate) fn http_refusal(
        &self,
        path: &str,
    ) -> Option<crate::serving_fence::ServingHttpPolicy> {
        self.fence.http_policy(path)
    }

    pub(crate) fn clock(&self) -> std::sync::Arc<plurx_core::cluster::clock::ClusterClockGuard> {
        self.membership.clock_guard()
    }

    /// Fresh confirmations keep the existing metrics and consistent removal
    /// checks. Directory projections never grant this authority.
    pub(crate) async fn confirm_active_voter(&self) -> Result<bool, MembershipError> {
        self.membership.local_node_is_active_voter().await
    }
    pub(crate) async fn confirm_voter(&self) -> Result<bool, MembershipError> {
        self.membership.local_node_is_committed_voter().await
    }
    pub(crate) async fn confirm_member(&self) -> Result<bool, MembershipError> {
        self.membership.local_node_is_committed_member().await
    }

    pub(crate) async fn singleton_jobs_allowed(&self) -> bool {
        self.membership.may_run_cluster_jobs().await
    }
    pub(crate) async fn job_allowed(
        &self,
        kind: plurx_core::store::background_jobs::JobKind,
    ) -> bool {
        use plurx_core::cluster::coordination::ClusterJobAuthority;
        self.membership.may_execute_job(kind).await
    }

    pub(crate) async fn membership_status(
        &self,
    ) -> Result<plurx_core::cluster::membership::MembershipStatus, MembershipError> {
        self.membership.status().await
    }
    pub(crate) async fn replication_status(
        &self,
    ) -> plurx_core::cluster::migration::status::ReplicationStatus {
        self.replication.status().await
    }
    pub(crate) async fn node_names(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, MembershipError> {
        self.membership.node_hostnames().await
    }
    pub(crate) async fn revocation_ready(&self) -> Result<bool, MembershipError> {
        self.membership.cache_admin_revocation_ready().await
    }
    pub(crate) async fn revocation_activated(&self) -> Result<bool, MembershipError> {
        self.membership
            .cache_admin_revocation_activated_locally()
            .await
    }
    pub(crate) async fn source_layout_available(&self) -> Result<bool, MembershipError> {
        self.membership.source_layout_ready().await
    }

    /// Preserve the existing roster result types at protocol boundaries while
    /// all purposes share the same stamped directory.
    pub(crate) async fn operation_targets(&self) -> Result<Vec<ActivityPeer>, MembershipError> {
        self.directory.targets(Purpose::Operations).await
    }
    pub(crate) async fn media_targets(&self) -> Result<Vec<ActivityPeer>, MembershipError> {
        self.directory.targets(Purpose::MediaWorkers).await
    }
    pub(crate) async fn voter_targets(&self) -> Result<Vec<ActivityPeer>, MembershipError> {
        self.directory.targets(Purpose::Voters).await
    }
    pub(crate) fn artwork(&self) -> Artwork {
        Artwork {
            manager: self.membership.clone(),
            directory: self.directory.clone(),
        }
    }
    pub(crate) fn observations(&self) -> Observations {
        Observations {
            monitor: self.replication.clone(),
            raft: self.replication.metrics_handle(),
            members: self.membership.metrics_handle(),
        }
    }

    /// Who can help, for `purpose`; see [`Directory::peers`].
    pub(crate) async fn peers(&self, purpose: Purpose) -> Result<Peers, ClusterError> {
        self.directory.peers(purpose).await
    }

    /// Typed control over this facade's existing mechanisms.
    pub(crate) fn operations(&self) -> Operations {
        Operations::new(self.clone())
    }

    /// The shared directory, for a subsystem that keeps its own handle.
    pub(crate) fn directory(&self) -> Directory {
        self.directory.clone()
    }
}

/// Existing artwork wire proofs and repair leases. These remain fresh
/// membership operations; the cached artwork roster is only a recovery hint.
#[derive(Clone)]
pub(crate) struct Artwork {
    manager: MembershipManager,
    directory: Directory,
}
impl Artwork {
    pub(crate) async fn targets(&self) -> Result<Vec<String>, MembershipError> {
        Ok(self
            .directory
            .targets(Purpose::Artwork)
            .await?
            .into_iter()
            .filter_map(|peer| peer.http_base)
            .collect())
    }

    pub(crate) fn sign(
        &self,
        name: &str,
    ) -> Result<plurx_core::cluster::membership::ArtworkPeerAuth, MembershipError> {
        self.manager.artwork_peer_auth(name)
    }
    pub(crate) async fn verify(
        &self,
        name: &str,
        proof: &plurx_core::cluster::membership::ArtworkPeerAuth,
    ) -> Result<bool, MembershipError> {
        self.manager.verify_artwork_peer_auth(name, proof).await
    }
    pub(crate) async fn claim_repair(
        &self,
        item: i64,
        lease: std::time::Duration,
    ) -> Result<Option<plurx_core::cluster::membership::ArtworkRepairClaim>, MembershipError> {
        self.manager.claim_artwork_source_repair(item, lease).await
    }
    pub(crate) async fn retire_repair(
        &self,
        fence: &plurx_core::store::ArtworkRepairFence,
    ) -> Result<bool, MembershipError> {
        self.manager.retire_artwork_source_repair(fence).await
    }
}

/// Plain histogram values, paired with the exact producer bucket bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadPoolWait {
    pub(crate) count: u64,
    pub(crate) sum_nanos: u64,
    pub(crate) buckets: [(u64, u64); hiqlite::DB_READ_POOL_WAIT_BOUNDS_NANOS.len()],
}
impl From<hiqlite::DbReadPoolWait> for ReadPoolWait {
    fn from(wait: hiqlite::DbReadPoolWait) -> Self {
        Self {
            count: wait.count,
            sum_nanos: wait.sum_nanos,
            buckets: std::array::from_fn(|i| {
                (
                    hiqlite::DB_READ_POOL_WAIT_BOUNDS_NANOS[i],
                    wait.cumulative_buckets[i],
                )
            }),
        }
    }
}

/// An already-observed writer reply outcome, with the producer's fixed label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WriterReplyObservation {
    pub(crate) request: &'static str,
    pub(crate) ok: u64,
    pub(crate) error: u64,
}
impl From<hiqlite::DbWriterAbandonedReplies> for WriterReplyObservation {
    fn from(row: hiqlite::DbWriterAbandonedReplies) -> Self {
        Self {
            request: row.request,
            ok: row.ok,
            error: row.error,
        }
    }
}

/// Canonical passive samples without exposing the raw metrics handles to
/// HTTP composition or request attribution.
#[derive(Clone)]
pub(crate) struct Observations {
    monitor: plurx_core::cluster::migration::status::ReplicationMonitor,
    raft: plurx_core::cluster::migration::status::PassiveRaftMetrics,
    members: plurx_core::cluster::membership::PassiveMembershipMetrics,
}
impl Observations {
    /// Process-wide atomics copied at scrape time, with no Store query.
    pub(crate) fn read_pool_wait(&self) -> ReadPoolWait {
        hiqlite::db_read_pool_wait().into()
    }

    /// Fixed-label writer counters; the underlying fixed array stays on stack.
    pub(crate) fn writer_replies(&self) -> impl ExactSizeIterator<Item = WriterReplyObservation> {
        hiqlite::db_writer_abandoned_replies()
            .map(WriterReplyObservation::from)
            .into_iter()
    }

    pub(crate) fn live_wal(
        &self,
    ) -> Option<plurx_core::cluster::migration::status::WalStatusSnapshot> {
        self.monitor.wal_status_snapshot()
    }
    pub(crate) fn local_transport(
        &self,
    ) -> Option<plurx_core::cluster::migration::status::SnapshotTransportStatus> {
        self.monitor.transport_status_snapshot()
    }
    pub(crate) async fn private_transport(
        &self,
        raft_id: u64,
    ) -> Result<Option<plurx_core::cluster::migration::status::SnapshotTransportStatus>, String>
    {
        self.monitor.peer_transport_status(raft_id).await
    }

    pub(crate) fn raft(&self) -> plurx_core::cluster::migration::status::PassiveRaftMetricsView {
        self.raft.snapshot()
    }
    pub(crate) fn members(&self) -> plurx_core::cluster::membership::PassiveMembershipMetricsView {
        self.members.snapshot()
    }
}

/// This node's place in the cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalRole {
    pub(crate) standing: Standing,
    pub(crate) admission: Admission,
}

impl LocalRole {
    /// Whether this process is part of a replicated cluster at all.
    pub(crate) fn is_clustered(self) -> bool {
        self.standing != Standing::Standalone
    }
}

/// Where the applied Raft membership puts this node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Unclustered: this process is the whole server.
    Standalone,
    Voter,
    Learner,
    /// Clustered, but the applied membership does not list this node.
    Absent,
}

/// Whether this node admits new work. `Maintenance` is the published drain
/// (`local_maintenance_active`); `Fenced` the published serving role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Open,
    Maintenance,
    Fenced,
}

/// What the caller wants peers for. Each purpose is its old producer's rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// The other committed voters, not removed or fenced, reachable or not
    /// (`activity_peers`).
    Voters,
    /// Ready media targets, learners included (`media_peers`).
    MediaWorkers,
    /// All committed members for remote control and Activity starts, stale or not.
    Operations,
    /// Reachable artwork recovery endpoints, without voter or readiness filtering.
    Artwork,
}

/// A failed roster read. Displays exactly as the membership error it wraps,
/// so callers that report it keep their messages.
#[derive(Debug)]
pub(crate) struct ClusterError(MembershipError);

impl std::fmt::Display for ClusterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ClusterError {}

impl ClusterError {
    /// A roster read that failed, for tests of how callers report one.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self(MembershipError::Unavailable)
    }
}

/// The peer directory (B2): one cached roster snapshot per process, kept
/// current by the applied mark and the applied Raft membership, never by a
/// timer. A hit costs atomic loads and the membership watch; a miss is one
/// local read.
#[derive(Clone)]
pub(crate) struct Directory {
    inner: RosterDirectory,
}

impl Directory {
    /// A directory with no applied mark: correct, never cached. For a
    /// subsystem a test wires to its own manager.
    #[cfg(test)]
    pub(crate) fn new(membership: MembershipManager) -> Self {
        Self::with_mark(membership, None)
    }

    pub(crate) fn with_mark(
        membership: MembershipManager,
        mark: Option<hiqlite::AppliedMarkHandle>,
    ) -> Self {
        Self {
            inner: RosterDirectory::new(membership, mark),
        }
    }

    /// The peers for `purpose`, evaluated now against the current snapshot.
    pub(crate) async fn peers(&self, purpose: Purpose) -> Result<Peers, ClusterError> {
        let snapshot = self.inner.snapshot().await.map_err(ClusterError)?;
        Peers::of(&snapshot, purpose, roster_now().map_err(ClusterError)?)
    }

    /// Existing protocol consumers keep their original membership error type.
    pub(crate) async fn targets(
        &self,
        purpose: Purpose,
    ) -> Result<Vec<ActivityPeer>, MembershipError> {
        self.peers(purpose)
            .await
            .map(|peers| peers.list)
            .map_err(|error| error.0)
    }

    /// The other committed voters with the voter facts of the same snapshot.
    pub(crate) async fn voters(&self) -> Result<Voters, ClusterError> {
        let snapshot = self.inner.snapshot().await.map_err(ClusterError)?;
        Ok(Voters::of(&snapshot, roster_now().map_err(ClusterError)?))
    }

    pub(crate) fn reads(&self) -> DirectoryReadCounts {
        self.inner.reads()
    }
}

/// One purpose's peers.
pub(crate) struct Peers {
    list: Vec<ActivityPeer>,
}

impl Peers {
    /// `purpose`'s rule over `snapshot` at `now`.
    fn of(snapshot: &RosterSnapshot, purpose: Purpose, now: i64) -> Result<Self, ClusterError> {
        let list = match purpose {
            Purpose::Voters => snapshot.voters(now),
            Purpose::MediaWorkers => snapshot.media_workers(now, true).map_err(ClusterError)?,
            Purpose::Operations => snapshot.operations(now).map_err(ClusterError)?,
            Purpose::Artwork => snapshot.artwork(now),
        };
        Ok(Self { list })
    }

    /// Peers over a fixed list.
    #[cfg(test)]
    pub(crate) fn for_test(list: Vec<ActivityPeer>) -> Self {
        Self { list }
    }

    /// Every peer the purpose admits, reachable or not, in roster order.
    pub(crate) fn all(&self) -> &[ActivityPeer] {
        &self.list
    }

    /// Peers that are reachable and have an HTTP base to reach them at.
    pub(crate) fn reachable(&self) -> impl Iterator<Item = &ActivityPeer> {
        self.list
            .iter()
            .filter(|peer| peer.reachable && peer.http_base.is_some())
    }

    /// One reachable peer with an HTTP base, by node id: `(node_id, base)`.
    pub(crate) fn find(&self, node_id: &str) -> Option<(String, String)> {
        self.reachable()
            .find(|peer| peer.node_id == node_id)
            .and_then(|peer| {
                peer.http_base
                    .clone()
                    .map(|base| (peer.node_id.clone(), base))
            })
    }
}

/// The other committed voters ([`Purpose::Voters`]) and the voter facts of
/// the snapshot they came from. Only a voter list can say whether every
/// voter is visible, so only this type has [`Voters::complete`].
pub(crate) struct Voters {
    peers: Peers,
    voter_count: usize,
    /// `None` for an unclustered process, which has no membership.
    local_is_voter: Option<bool>,
}

impl Voters {
    fn of(snapshot: &RosterSnapshot, now: i64) -> Self {
        Self {
            peers: Peers {
                list: snapshot.voters(now),
            },
            voter_count: snapshot.voter_count(),
            local_is_voter: snapshot.local_is_voter().ok(),
        }
    }

    /// Voters over a fixed list and voter facts.
    #[cfg(test)]
    pub(crate) fn for_test(
        list: Vec<ActivityPeer>,
        voter_count: usize,
        local_is_voter: Option<bool>,
    ) -> Self {
        Self {
            peers: Peers { list },
            voter_count,
            local_is_voter,
        }
    }

    /// The other voters.
    pub(crate) fn peers(&self) -> &Peers {
        &self.peers
    }

    /// Whether this process is clustered at all.
    pub(crate) fn is_clustered(&self) -> bool {
        self.local_is_voter.is_some()
    }

    /// Whether this node is a committed voter, on the same snapshot; an
    /// unclustered process has no membership to be one of.
    pub(crate) fn local_is_voter(&self) -> Result<bool, ClusterError> {
        self.local_is_voter
            .ok_or(ClusterError(MembershipError::Unavailable))
    }

    /// Whether every committed voter is visible: the other voters plus this
    /// node when it is one (audit F1). The three facts come from one
    /// snapshot, so a membership change cannot fall between them.
    pub(crate) fn complete(&self) -> Result<bool, ClusterError> {
        Ok(self
            .peers
            .list
            .len()
            .saturating_add(usize::from(self.local_is_voter()?))
            == self.voter_count)
    }

    /// Committed voters including this node (`activity_voter_count`).
    pub(crate) fn voter_count(&self) -> usize {
        self.voter_count
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use plurx_core::cluster::membership::directory::TestRosterRow;

    pub(crate) const NOW: i64 = 1_800_000_000_000;

    fn peer(node: &str, reachable: bool, base: bool) -> ActivityPeer {
        ActivityPeer {
            node_id: node.to_owned(),
            raft_id: 2,
            http_base: base.then(|| format!("http://{node}.invalid")),
            reachable,
        }
    }

    /// A clustered roster row: `(raft id, role, reachable, has base, ready)`.
    pub(crate) fn row(
        raft_id: u64,
        role: &str,
        reachable: bool,
        base: bool,
        ready: bool,
    ) -> TestRosterRow {
        let seen = if reachable { NOW - 1_000 } else { NOW - 60_000 };
        TestRosterRow {
            node_id: format!("node-{raft_id}"),
            raft_id,
            last_seen_at: seen,
            role: Some(role.to_owned()),
            http_base: base.then(|| format!("http://node-{raft_id}.invalid")),
            bounded_read_ready: ready,
            observed_at: Some(NOW - 1_000),
        }
    }

    /// Local node 1. Voters 1, 2, 3, 4; learner 5; 6 a row that Raft does
    /// not list.
    fn roster(local: u64) -> RosterSnapshot {
        RosterSnapshot::for_tests(
            local,
            &[1, 2, 3, 4, 5],
            &[1, 2, 3, 4],
            [
                row(1, "voter", true, true, true),
                row(2, "voter", true, true, true),
                row(3, "voter", false, true, true),
                row(4, "voter", true, false, false),
                row(5, "learner", true, true, true),
                row(6, "voter", true, true, true),
            ]
            .into_iter()
            .filter(|row| row.raft_id != local)
            .collect(),
        )
    }

    fn ids(peers: &Peers) -> Vec<String> {
        peers
            .all()
            .iter()
            .map(|peer| peer.node_id.clone())
            .collect()
    }

    /// Each purpose maps to its own rule on a clustered snapshot, not to
    /// another purpose's.
    #[test]
    fn each_purpose_maps_to_its_rule_on_a_clustered_snapshot() {
        let snapshot = roster(1);
        assert_eq!(
            ids(&Peers::of(&snapshot, Purpose::Voters, NOW).expect("voters")),
            ["node-2", "node-3", "node-4"],
            "voters: every other committed voter, reachable or not"
        );
        assert_eq!(
            ids(&Peers::of(&snapshot, Purpose::MediaWorkers, NOW).expect("roles")),
            ["node-2", "node-3", "node-5"],
            "media workers: ready members, learners included"
        );
        assert_eq!(
            ids(&Peers::of(&snapshot, Purpose::Operations, NOW).expect("operations")),
            ["node-2", "node-3", "node-4", "node-5"]
        );
        assert_eq!(
            ids(&Peers::of(&snapshot, Purpose::Artwork, NOW).expect("artwork")),
            ["node-2", "node-5", "node-6"],
            "artwork includes a noncommitted endpoint, without stale or missing endpoints"
        );
    }

    /// `Voters` carries the voter facts of the same snapshot.
    #[test]
    fn voters_take_their_facts_from_the_snapshot() {
        let voter = Voters::of(&roster(1), NOW);
        assert_eq!(voter.voter_count(), 4);
        assert!(voter.local_is_voter().expect("clustered"));
        assert!(voter.is_clustered());
        assert!(
            voter.complete().expect("clustered"),
            "3 others + this voter"
        );
        let learner = Voters::of(&roster(5), NOW);
        assert!(!learner.local_is_voter().expect("clustered"));
        assert!(learner.complete().expect("clustered"), "4 others, no self");
        let standalone = Voters::of(&RosterSnapshot::standalone(), NOW);
        assert!(!standalone.is_clustered());
        assert!(standalone.complete().is_err());
    }

    /// F1: "every voter is visible" counts this node only when it is a
    /// voter. Counting it unconditionally made the clause `n != n + 1` on a
    /// learner, in two files; it now exists once.
    #[test]
    fn complete_counts_this_node_only_when_it_votes() {
        let one = vec![peer("peer-a", true, true)];
        let complete = |count, local| {
            Voters::for_test(one.clone(), count, local)
                .complete()
                .expect("clustered")
        };
        assert!(complete(2, Some(true)));
        assert!(!complete(3, Some(true)));
        assert!(complete(1, Some(false)));
        assert!(!complete(2, Some(false)));
        assert!(Voters::for_test(one, 1, None).complete().is_err());
    }

    #[test]
    fn find_names_only_a_reachable_peer_with_a_base() {
        let peers = Peers::for_test(vec![
            peer("up", true, true),
            peer("down", false, true),
            peer("baseless", true, false),
        ]);
        assert_eq!(
            peers.find("up"),
            Some(("up".to_owned(), "http://up.invalid".to_owned()))
        );
        assert_eq!(peers.find("down"), None);
        assert_eq!(peers.find("baseless"), None);
        assert_eq!(peers.find("missing"), None);
        assert_eq!(peers.reachable().count(), 1);
    }

    #[tokio::test]
    async fn an_unclustered_process_is_standalone_open_and_peerless() {
        let cluster = Cluster::new(
            ServingFence::new(
                plurx_core::cluster::migration::status::ReplicationMonitor::sqlite()
                    .metrics_handle(),
            ),
            MembershipManager::unavailable(),
            plurx_core::cluster::migration::status::ReplicationMonitor::sqlite(),
            None,
        );
        assert_eq!(
            cluster.role(),
            LocalRole {
                standing: Standing::Standalone,
                admission: Admission::Open
            }
        );
        assert!(!cluster.role().is_clustered());
        let voters = cluster.directory().voters().await.expect("standalone");
        assert!(voters.peers().all().is_empty());
        assert_eq!(voters.voter_count(), 1);
        assert!(voters.local_is_voter().is_err());
        assert!(cluster
            .peers(Purpose::MediaWorkers)
            .await
            .expect("standalone")
            .all()
            .is_empty());
        assert_eq!(cluster.directory().reads().hit, 2);
    }
}
