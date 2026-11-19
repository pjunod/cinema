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
                protocol: self.cluster.access.revocation_exclusion(),
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
        protocol: plurx_core::cluster::membership::ClockProtocol,
    }

    impl ClockOperations {
        /// Clock bootstrap uses the same pooled transport before AppState exists.
        pub(crate) fn calls(&self) -> super::calls::PeerTransport {
            super::calls::PeerTransport::new(self.manager.clone())
        }
        pub(super) fn new(manager: plurx_core::cluster::membership::MembershipManager) -> Self {
            Self {
                protocol: manager.cluster_access().clock_protocol(),
                manager,
            }
        }

        pub(crate) fn guard(
            &self,
        ) -> std::sync::Arc<plurx_core::cluster::clock::ClusterClockGuard> {
            self.protocol.guard()
        }

        pub(crate) fn same_source(&self, other: &Self) -> bool {
            std::sync::Arc::ptr_eq(&self.guard(), &other.guard())
        }

        pub(crate) fn leadership(
            &self,
        ) -> Option<plurx_core::cluster::membership::ClockLeadershipIdentity> {
            self.protocol.leadership()
        }

        pub(crate) fn clustered(&self) -> bool {
            matches!(
                self.protocol.deployment(),
                plurx_core::cluster::membership::Deployment::Replicated
            )
        }

        pub(crate) fn change_source(&self) -> crate::settings_changes::SettingsChangeSource {
            match self.protocol.changes() {
                Some(changes) => crate::settings_changes::SettingsChangeSource::Applied(
                    super::AppliedChanges::Clock(changes),
                ),
                None => crate::settings_changes::SettingsChangeSource::LocalOnly,
            }
        }

        pub(crate) async fn roster(
            &self,
        ) -> Result<plurx_core::cluster::membership::ClockPeerRoster, MembershipError> {
            self.protocol.prove_roster().await
        }

        pub(crate) async fn roster_hint(
            &self,
        ) -> Result<plurx_core::cluster::membership::ClockPeerRoster, MembershipError> {
            self.protocol.applied_hint().await
        }

        pub(crate) async fn roster_after_auth(
            &self,
            auth: &plurx_core::cluster::membership::InternalPeerAuth,
        ) -> Result<plurx_core::cluster::membership::ClockPeerRoster, MembershipError> {
            self.protocol.after_authenticated_request(auth).await
        }

        pub(crate) async fn authorize_clock_request(
            &self,
            auth: &plurx_core::cluster::membership::InternalPeerAuth,
            method: &str,
            path: &str,
            body: &[u8],
        ) -> Result<bool, MembershipError> {
            self.protocol
                .authorize_request(auth, method, path, body)
                .await
        }

        pub(crate) fn request_still_fresh(
            &self,
            auth: &plurx_core::cluster::membership::InternalPeerAuth,
        ) -> Result<bool, MembershipError> {
            self.protocol.request_still_fresh(auth)
        }

        pub(crate) fn sign_clock_reply(
            &self,
            target: &str,
            nonce: &str,
            path: &str,
            response: &[u8],
        ) -> Result<String, MembershipError> {
            self.protocol.sign_reply(target, nonce, path, response)
        }
    }

    /// Exact claim-bound revocation authority; never a cached directory roster.
    #[derive(Clone)]
    pub(crate) struct RevocationOperations {
        protocol: plurx_core::cluster::membership::RevocationExclusion,
    }

    impl RevocationOperations {
        pub(crate) fn clustered(&self) -> bool {
            matches!(
                self.protocol.deployment(),
                plurx_core::cluster::membership::Deployment::Replicated
            )
        }

        pub(crate) fn prepare_exclusion(
            &self,
            node_id: &str,
            duration: Duration,
        ) -> Result<plurx_core::cluster::membership::CacheAdminRevocationLease, MembershipError>
        {
            self.protocol.prepare(node_id, duration)
        }

        pub(crate) async fn admit_exclusion(
            &self,
            lease: &mut plurx_core::cluster::membership::CacheAdminRevocationLease,
            deadline: tokio::time::Instant,
        ) -> Result<plurx_core::cluster::membership::CacheAdminExclusionAdmission, MembershipError>
        {
            self.protocol.admit(lease, deadline).await
        }

        pub(crate) async fn release_exclusion(
            &self,
            lease: &plurx_core::cluster::membership::CacheAdminRevocationLease,
        ) -> Result<
            plurx_core::cluster::membership::CacheAdminRevocationCleanupOutcome,
            MembershipError,
        > {
            self.protocol.release(lease).await
        }

        pub(crate) async fn claim_roster(
            &self,
            claim_id: &str,
        ) -> Result<Vec<plurx_core::cluster::membership::ActivityPeer>, MembershipError> {
            self.protocol.claim_roster(claim_id).await
        }

        pub(crate) async fn blockers(&self) -> Result<Vec<String>, MembershipError> {
            self.protocol.blockers().await
        }

        pub(crate) async fn activation_ready(&self) -> Result<bool, MembershipError> {
            self.protocol.activation_ready().await
        }

        pub(crate) async fn activated_locally(&self) -> Result<bool, MembershipError> {
            self.protocol.activated_locally().await
        }

        pub(crate) async fn claim_applied(&self, claim_id: &str) -> Result<bool, MembershipError> {
            self.protocol.claim_applied(claim_id).await
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
        /// The drain's stop-time leadership handoff: the manager's one
        /// bounded handoff, which writes nothing and never fails.
        pub(crate) async fn hand_off_leadership_for_stop(
            &self,
        ) -> plurx_core::cluster::membership::ShutdownHandoff {
            self.cluster.membership.handoff_for_shutdown().await
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
        pub(crate) async fn current_outage(&self) -> Option<PlannedOutageFenceToken> {
            self.cluster.fence.current_planned_outage().await
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
            matches!(
                self.cluster.access.local().deployment(),
                plurx_core::cluster::membership::Deployment::Replicated
            )
        }
        pub(crate) async fn topology(&self) -> Result<MembershipStatus, MembershipError> {
            self.cluster.membership_status().await
        }
        /// Always fresh: mutation preflight must not use the cached directory.
        pub(crate) async fn fresh_targets(
            &self,
        ) -> Result<Vec<plurx_core::cluster::membership::ActivityPeer>, MembershipError> {
            self.cluster.access.administrative().targets().await
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

/// Boot-time sharing coordination through the manager's original
/// operations; see [`Cluster::sharing_boot`].
pub(crate) struct SharingBoot {
    membership: MembershipManager,
}

impl SharingBoot {
    pub(crate) fn record_custody_boot(&self, boot: Option<uuid::Uuid>) {
        self.membership.set_ingress_custody_boot(boot);
    }

    pub(crate) async fn announce_custody_boot(&self) -> Result<(), MembershipError> {
        self.membership.publish_ingress_custody_boot().await
    }

    pub(crate) async fn qualify_purpose_master(
        &self,
        master: std::sync::Arc<plurx_core::secrets::CredentialKey>,
    ) -> Result<(), MembershipError> {
        self.membership.prepare_purpose_master(master).await
    }

    pub(crate) async fn coordinate_purposes(&self) -> Result<(), MembershipError> {
        self.membership.coordinate_purpose_keys().await
    }
}

/// The selected backend's two cluster handles, carried from store
/// selection to composition without being named outside the facade.
pub(crate) struct ClusterParts {
    replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    membership: MembershipManager,
}

impl ClusterParts {
    pub(crate) fn of(selected: &plurx_core::cluster::migration::SelectedStore) -> Self {
        Self {
            replication: selected.replication_monitor(),
            membership: selected.membership_manager(),
        }
    }

    /// Parts a fixture already holds.
    #[cfg(test)]
    pub(crate) fn from_handles(
        replication: plurx_core::cluster::migration::status::ReplicationMonitor,
        membership: MembershipManager,
    ) -> Self {
        Self {
            replication,
            membership,
        }
    }

    /// SQLite with no membership: what unclustered tests compose.
    #[cfg(test)]
    pub(crate) fn standalone() -> Self {
        Self {
            replication: plurx_core::cluster::migration::status::ReplicationMonitor::sqlite(),
            membership: MembershipManager::unavailable(),
        }
    }
}

/// The startup clock observation socket before `AppState` exists:
/// plurx-core hands its observer the manager, and the facade turns it into
/// the clock capability at once.
pub(crate) trait StartupClock: Send + Sync {
    fn start(
        &self,
        clock: ClockOperations,
        node_id: String,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(), plurx_core::error::StoreError>> + Send + '_,
        >,
    >;
}

/// Adapts a [`StartupClock`] to plurx-core's startup observer.
pub(crate) struct StartupClockAdapter<'a, T: ?Sized>(pub(crate) &'a T);

impl<T: StartupClock + ?Sized> plurx_core::cluster::migration::StartupClockObserver
    for StartupClockAdapter<'_, T>
{
    fn start(
        &self,
        membership: MembershipManager,
        node_id: String,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(), plurx_core::error::StoreError>> + Send + '_,
        >,
    > {
        self.0.start(startup_clock(membership), node_id)
    }
}

/// The facade. Cheap to clone; clones share one peer directory.
#[derive(Clone)]
pub(crate) struct Cluster {
    fence: ServingFence,
    membership: MembershipManager,
    directory: Directory,
    replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    jobs: Jobs,
    access: plurx_core::cluster::membership::ClusterAccess,
}

impl Cluster {
    /// Test-only access to the raw parts, for fixtures that drive the
    /// fence, manager or monitor directly. Not compiled into production.
    #[cfg(test)]
    pub(crate) fn test_fence(&self) -> &ServingFence {
        &self.fence
    }

    #[cfg(test)]
    pub(crate) fn test_membership(&self) -> &MembershipManager {
        &self.membership
    }

    #[cfg(test)]
    pub(crate) fn test_replication(
        &self,
    ) -> &plurx_core::cluster::migration::status::ReplicationMonitor {
        &self.replication
    }

    /// This facade with only its replication monitor replaced, for fixtures
    /// that replace it: the fence, manager, directory, jobs and access stay
    /// the ones every subsystem was built with.
    #[cfg(test)]
    pub(crate) fn with_replication(
        &self,
        replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    ) -> Self {
        Self {
            replication,
            ..self.clone()
        }
    }

    /// The same fence and replication monitor over another membership
    /// manager, for fixtures that replace it.
    #[cfg(test)]
    pub(crate) fn with_membership(
        &self,
        membership: MembershipManager,
        mark: Option<hiqlite::AppliedMarkHandle>,
    ) -> Self {
        Self::new(
            self.fence.clone(),
            membership,
            self.replication.clone(),
            mark,
        )
    }

    /// Compose the facade from the selected backend's parts: the fence is
    /// built from the replication metrics handle, as `new_configured` did.
    pub(crate) fn compose(parts: ClusterParts, mark: Option<hiqlite::AppliedMarkHandle>) -> Self {
        let fence = ServingFence::new(parts.replication.metrics_handle());
        Self::new(fence, parts.membership, parts.replication, mark)
    }

    /// The facade over this node's fence and manager; `mark` is the applied
    /// mark the peer directory stamps with (`AppliedView::applied_mark`).
    pub(crate) fn new(
        fence: ServingFence,
        membership: MembershipManager,
        replication: plurx_core::cluster::migration::status::ReplicationMonitor,
        mark: Option<hiqlite::AppliedMarkHandle>,
    ) -> Self {
        let access = membership.cluster_access();
        Self {
            directory: Directory {
                inner: access.directory(mark),
            },
            jobs: access.jobs(),
            access,
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

    /// Resolves once, with the error, when this node's own database Raft
    /// core has quit on a fatal error; never for an ordinary stop or an
    /// unclustered process. The daemon drains and exits on it.
    pub(crate) fn core_failure(
        &self,
    ) -> impl std::future::Future<Output = String> + Send + 'static {
        self.membership.raft_core_failure()
    }

    /// Start the replication sampler and the fence monitor, in that order,
    /// as the daemon's background start always has.
    pub(crate) fn spawn_fence_and_metrics(&self, shutdown: tokio_util::sync::CancellationToken) {
        tokio::spawn(
            self.replication
                .clone()
                .passive_metrics_loop(shutdown.clone().cancelled_owned()),
        );
        tokio::spawn(self.fence.clone().monitor_loop(shutdown));
    }

    /// Advertise media-relay acquisition, then start the membership
    /// heartbeat that publishes it.
    pub(crate) fn start_heartbeat_with_relay(&self) {
        self.membership.register_media_relay_acquisition();
        tokio::spawn(self.membership.clone().heartbeat_loop());
    }

    /// Answer peers asking whether this node can read a package's source
    /// while a peer is removed.
    pub(crate) fn spawn_source_probe_answers(&self) {
        tokio::spawn(self.membership.clone().offline_source_probe_loop());
    }

    /// Boot-time sharing coordination: accepted-driver custody and purpose
    /// keys, each the manager's original operation.
    pub(crate) fn sharing_boot(&self) -> SharingBoot {
        SharingBoot {
            membership: self.membership.clone(),
        }
    }

    /// Whether `owner_node_id` advertises media-relay acquisition.
    pub(crate) async fn relay_supported_by(
        &self,
        owner_node_id: &str,
    ) -> Result<bool, MembershipError> {
        self.membership
            .media_relay_acquisition_supported(owner_node_id)
            .await
    }

    /// Nodes that have not yet taken the current Live TV protocol.
    pub(crate) async fn live_tv_pending_nodes(&self) -> Result<Vec<String>, MembershipError> {
        self.membership.live_tv_protocol_pending_nodes().await
    }

    /// This node's own hostname, when known.
    pub(crate) fn own_hostname(&self) -> Option<String> {
        self.membership.local_hostname()
    }

    /// This node's standing and admission, from in-process state only: the
    /// applied Raft membership (the source `local_node_is_committed_voter`
    /// reads) and the two atomics the heartbeat publishes. No IO, no await.
    pub(crate) fn role(&self) -> LocalRole {
        let facts = self.access.local().facts();
        let standing = match facts.standing {
            AppliedStanding::Standalone => Standing::Standalone,
            AppliedStanding::Voter => Standing::Voter,
            AppliedStanding::Learner => Standing::Learner,
            AppliedStanding::Absent => Standing::Absent,
        };
        let admission = if facts.maintenance {
            Admission::Maintenance
        } else if facts.admission == LocalServingRole::Fenced {
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
        self.access.local().admission().await
    }

    pub(crate) fn maintenance_active(&self) -> bool {
        self.access.local().maintenance_fence()
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
        self.access.clock_protocol().guard()
    }

    /// Fresh confirmations keep the existing metrics and consistent removal
    /// checks. Directory projections never grant this authority.
    pub(crate) async fn confirm_active_voter(&self) -> Result<bool, MembershipError> {
        self.access.active_voter_authority().confirm().await
    }
    pub(crate) async fn confirm_voter(&self) -> Result<bool, MembershipError> {
        self.access
            .local()
            .committed()
            .await
            .map(|observed| observed.voter)
    }
    pub(crate) async fn confirm_member(&self) -> Result<bool, MembershipError> {
        self.access
            .local()
            .committed()
            .await
            .map(|observed| observed.member)
    }

    pub(crate) async fn singleton_jobs_allowed(&self) -> bool {
        plurx_core::cluster::coordination::ClusterJobAuthority::may_run_cluster_jobs(&self.jobs)
            .await
    }
    pub(crate) async fn job_allowed(
        &self,
        kind: plurx_core::store::background_jobs::JobKind,
    ) -> bool {
        use plurx_core::cluster::coordination::ClusterJobAuthority;
        self.jobs.may_execute_job(kind).await
    }

    pub(crate) async fn membership_status(
        &self,
    ) -> Result<plurx_core::cluster::membership::MembershipStatus, MembershipError> {
        self.access.administrative().topology().await
    }
    pub(crate) async fn replication_status(
        &self,
    ) -> plurx_core::cluster::migration::status::ReplicationStatus {
        self.replication.status().await
    }
    pub(crate) async fn node_names(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, MembershipError> {
        self.access.names().node_names().await
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

    pub(crate) fn sharing(&self) -> Sharing {
        Sharing {
            manager: self.membership.clone(),
            local: self.access.local(),
            fence: self.fence.clone(),
        }
    }
    pub(crate) fn source_members(&self) -> SourceMembers {
        SourceMembers {
            manager: self.membership.clone(),
        }
    }
    pub(crate) fn receiver_members(&self) -> ReceiverMembers {
        ReceiverMembers {
            manager: self.membership.clone(),
        }
    }
    pub(crate) fn authentication(&self) -> calls::PeerAuthentication {
        calls::PeerAuthentication::new(self.membership.clone())
    }
    /// Retained restart admission uses the fence's expires-at predicate,
    /// which intentionally differs from projected authority admission.
    pub(crate) fn restart_admissions(&self) -> RestartAdmissions {
        RestartAdmissions {
            fence: self.fence.clone(),
        }
    }
    pub(crate) fn starts_mutable_media(method: &str, path: &str) -> bool {
        ServingFence::starts_mutable_media(method, path)
    }

    /// Preserve the fence's exact new-media admission, not a cached verdict.
    pub(crate) async fn admitting_new_media(&self) -> bool {
        self.fence.accepting_new_media().await
    }
    pub(crate) async fn media_targets_for_route(
        &self,
        allow_learners: bool,
    ) -> Result<Vec<ActivityPeer>, MembershipError> {
        self.directory.media_targets_for_route(allow_learners).await
    }

    /// An owned bounded peer-call capability over this manager's shared pool.
    pub(crate) fn calls(&self) -> calls::PeerTransport {
        calls::PeerTransport::new(self.membership.clone())
    }

    /// Typed control over this facade's existing mechanisms.
    pub(crate) fn operations(&self) -> Operations {
        Operations::new(self.clone())
    }

    /// Existing fresh job authority and reconciliation evidence.
    pub(crate) fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// The shared directory, for a subsystem that keeps its own handle.
    pub(crate) fn directory(&self) -> Directory {
        self.directory.clone()
    }
}

/// Core-owned fresh job authority and reconciliation service.
pub(crate) use plurx_core::cluster::membership::JobControl as Jobs;

/// Applied-index observation retains the original live Raft watch.
#[derive(Clone)]
pub(crate) enum AppliedChanges {
    Local(hiqlite::LocalDbRaftMetrics),
    Clock(plurx_core::cluster::membership::ClockChanges),
}
impl AppliedChanges {
    pub(crate) fn applied_index(&self) -> Option<u64> {
        match self {
            Self::Local(metrics) => metrics.snapshot().last_applied_index,
            Self::Clock(changes) => changes.applied_index(),
        }
    }
    pub(crate) fn live(&self) -> bool {
        match self {
            Self::Local(metrics) => metrics.membership_snapshot().running,
            Self::Clock(changes) => changes.live(),
        }
    }
    pub(crate) async fn changed(&mut self) -> bool {
        match self {
            Self::Local(metrics) => metrics.wait_for_change().await,
            Self::Clock(changes) => changes.changed().await,
        }
    }
}

#[derive(Clone)]
pub(crate) struct BackupSource {
    client: hiqlite::Client,
}
#[derive(Clone, Copy)]
pub(crate) struct BackupProgress {
    pub(crate) applied: Option<u64>,
    pub(crate) last_log: Option<u64>,
    pub(crate) snapshot: Option<u64>,
}
#[derive(Debug)]
pub(crate) struct BackupObservationError(String);
impl std::fmt::Display for BackupObservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
/// Bootstrap hands only the owned backup protocol to feature code.
pub(crate) fn backup_source(client: hiqlite::Client) -> BackupSource {
    BackupSource { client }
}
impl BackupSource {
    pub(crate) fn applied_changes(&self) -> Option<AppliedChanges> {
        self.client
            .local_db_raft_metrics()
            .ok()
            .map(AppliedChanges::Local)
    }
    pub(crate) async fn progress(&self) -> Result<BackupProgress, BackupObservationError> {
        let metrics = self
            .client
            .metrics_db()
            .await
            .map_err(|error| BackupObservationError(error.to_string()))?;
        Ok(BackupProgress {
            applied: metrics.last_applied.as_ref().map(|log| log.index),
            last_log: metrics.last_log_index,
            snapshot: metrics.snapshot.as_ref().map(|log| log.index),
        })
    }
    pub(crate) async fn commit_anchor(&self) -> Result<u64, plurx_core::error::StoreError> {
        use plurx_core::error::StoreError;
        // This no-row write is forwarded to the leader and acknowledges the
        // committed cut. The caller retains its single absolute lease budget.
        self.client
            .execute_acked(
                "UPDATE settings SET updated_at = updated_at WHERE 0",
                hiqlite::macros::params!(),
            )
            .await
            .map_err(|error| StoreError::Database(format!("anchoring the backup cut: {error}")))?
            .log_index
            .ok_or_else(|| {
                StoreError::Task(
                    "the leader did not report the backup anchor's log index".to_owned(),
                )
            })
    }
    pub(crate) async fn trigger_snapshot(&self) -> Result<(), plurx_core::error::StoreError> {
        self.client
            .trigger_local_db_snapshot()
            .await
            .map_err(|error| {
                plurx_core::error::StoreError::Database(format!(
                    "triggering database snapshot: {error}"
                ))
            })
    }
}

/// Owned admission for a subsystem that must retain the exact restart guard.
#[derive(Clone)]
pub(crate) struct RestartAdmissions {
    fence: ServingFence,
}
impl RestartAdmissions {
    pub(crate) fn try_admit(
        &self,
    ) -> impl std::future::Future<Output = Option<crate::serving_fence::RestartAdmission>> + Send + '_
    {
        self.fence.try_restart_admission()
    }
    #[cfg(test)]
    pub(crate) fn for_fence(fence: ServingFence) -> Self {
        Self { fence }
    }
}

/// Read-only sharing admission uses the original advisory cache and the
/// same fence epoch; an unreadable floor remains an unreadable observation.
#[derive(Clone)]
pub(crate) struct Sharing {
    manager: MembershipManager,
    local: plurx_core::cluster::membership::LocalNode,
    fence: ServingFence,
}
impl Sharing {
    pub(crate) fn is_clustered(&self) -> bool {
        matches!(
            self.local.deployment(),
            plurx_core::cluster::membership::Deployment::Replicated
        )
    }
    pub(crate) fn member_floor(
        &self,
        required: plurx_core::cluster::membership::SharingMemberFloor,
    ) -> impl std::future::Future<Output = Result<bool, MembershipError>> + Send + '_ {
        // Sample at call creation, just as the original argument expression
        // did before the caller's timeout future was constructed.
        self.manager
            .sharing_member_floor_advisory(required, self.fence.advisory_epoch())
    }
}

/// Reacquire the opaque Source write observation at every proof boundary.
/// This capability retains a source, never a previously observed permission.
#[derive(Clone)]
pub(crate) struct SourceMembers {
    manager: MembershipManager,
}
impl SourceMembers {
    pub(crate) fn observe(
        &self,
    ) -> impl std::future::Future<
        Output = Result<
            Option<plurx_core::cluster::membership::SourceAdmissionMembers>,
            MembershipError,
        >,
    > + Send
           + '_ {
        self.manager.observe_source_admission_members()
    }
}

/// Receiver custody has a distinct opaque proof from Source admission.
#[derive(Clone)]
pub(crate) struct ReceiverMembers {
    manager: MembershipManager,
}
impl ReceiverMembers {
    pub(crate) fn observe(
        &self,
    ) -> impl std::future::Future<
        Output = Result<
            Option<plurx_core::cluster::membership::IngressCustodyMembers>,
            MembershipError,
        >,
    > + Send
           + '_ {
        self.manager.observe_ingress_custody_members()
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

    #[cfg(test)]
    pub(crate) fn with_mark(
        membership: MembershipManager,
        mark: Option<hiqlite::AppliedMarkHandle>,
    ) -> Self {
        Self {
            inner: membership.cluster_access().directory(mark),
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

    /// The caller's route policy decides whether ready learners are eligible.
    pub(crate) async fn media_targets_for_route(
        &self,
        allow_learners: bool,
    ) -> Result<Vec<ActivityPeer>, MembershipError> {
        self.inner
            .snapshot()
            .await?
            .media_workers(roster_now()?, allow_learners)
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

// BEGIN PEER CALL IMPLEMENTATION
pub(crate) mod calls {
    //! Shared, bounded HTTP transport for voter-to-voter control requests.
    //!
    //! Peer origins come only from committed membership. Redirects are disabled,
    //! every response is read under the caller's common deadline and byte budget,
    //! and exact requests bind their raw body and route into the node signature.

    use std::future::Future;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use plurx_core::cluster::membership::{
        normalize_internal_http_base, ActivityPeerAuth, InternalPeerAuth, MembershipManager,
    };

    pub(crate) const NODE_HEADER: &str = "x-plurx-cluster-node";
    pub(crate) const TARGET_HEADER: &str = "x-plurx-cluster-target";
    pub(crate) const TIMESTAMP_HEADER: &str = "x-plurx-cluster-time-ms";
    pub(crate) const NONCE_HEADER: &str = "x-plurx-cluster-nonce";
    pub(crate) const SIGNATURE_HEADER: &str = "x-plurx-cluster-signature";
    pub(crate) const RESPONSE_SIGNATURE_HEADER: &str = "x-plurx-response-signature";

    #[derive(Clone, Copy)]
    pub(crate) enum PeerAuthMode {
        LegacyActivity,
        ExactRequest,
        /// A voter control response must prove its status and exact body too.
        ExactRequestAndResponse,
        /// A committed member, including a learner, must prove the response.
        ExactRequestAndMemberResponse,
    }

    #[derive(Debug)]
    pub(crate) struct PeerResponse {
        pub(crate) dv_processing_header: Option<String>,
        pub(crate) processing_skip_header: Option<String>,
        pub(crate) status: reqwest::StatusCode,
        pub(crate) body: Vec<u8>,
        pub(crate) clock_timing:
            Option<(i64, i64, plurx_core::cluster::clock::ClockDecisionTicket)>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum PeerTransportError {
        Unreachable,
        TimedOut,
        InvalidResponse,
    }

    /// Exact inbound authorization and response signing share the existing
    /// core algorithm, without requiring an outbound HTTP pool.
    #[derive(Clone)]
    pub(crate) struct PeerAuthentication {
        membership: MembershipManager,
    }
    impl PeerAuthentication {
        pub(super) fn new(membership: MembershipManager) -> Self {
            Self { membership }
        }
        pub(crate) async fn authorize_activity(
            &self,
            auth: &ActivityPeerAuth,
        ) -> Result<bool, plurx_core::cluster::membership::MembershipError> {
            self.membership.authorize_activity_request(auth).await
        }
        pub(crate) async fn authorize_read_request(
            &self,
            auth: &InternalPeerAuth,
            method: &str,
            path: &str,
            body: &[u8],
        ) -> Result<bool, plurx_core::cluster::membership::MembershipError> {
            self.membership
                .authorize_internal_peer_read_request(auth, method, path, body)
                .await
        }
        pub(crate) async fn authorize_voter_request(
            &self,
            auth: &InternalPeerAuth,
            method: &str,
            path: &str,
            body: &[u8],
        ) -> Result<bool, plurx_core::cluster::membership::MembershipError> {
            self.membership
                .authorize_internal_peer_voter_request(auth, method, path, body)
                .await
        }
        pub(crate) async fn authorize_exact_request(
            &self,
            auth: &InternalPeerAuth,
            method: &str,
            path: &str,
            body: &[u8],
        ) -> Result<bool, plurx_core::cluster::membership::MembershipError> {
            self.membership
                .authorize_internal_peer_request(auth, method, path, body)
                .await
        }
        pub(crate) fn sign_exact_response(
            &self,
            target_node_id: &str,
            nonce: &str,
            path: &str,
            response: &[u8],
        ) -> Result<String, plurx_core::cluster::membership::MembershipError> {
            self.membership
                .sign_internal_peer_response(target_node_id, nonce, path, response)
        }
    }

    #[derive(Clone)]
    pub(crate) struct PeerTransport {
        membership: MembershipManager,
        client: Result<reqwest::Client, String>,
    }

    impl PeerTransport {
        /// A transport over the membership manager's pooled client. Building one
        /// is cheap and opens nothing: every transport made from the same cluster
        /// membership shares one connection pool, however many callers make one.
        pub(super) fn new(membership: MembershipManager) -> Self {
            let client = membership.peer_http().into_result();
            Self { membership, client }
        }

        #[allow(clippy::too_many_arguments)]
        pub(crate) async fn request(
            &self,
            expected_node_id: &str,
            base: &str,
            method: reqwest::Method,
            path: &str,
            body: Vec<u8>,
            deadline: tokio::time::Instant,
            max_response_bytes: usize,
            auth_mode: PeerAuthMode,
        ) -> Result<PeerResponse, PeerTransportError> {
            self.request_with_optional_header(
                expected_node_id,
                base,
                method,
                path,
                body,
                deadline,
                max_response_bytes,
                auth_mode,
                None,
                None,
            )
            .await
            .map(|(response, _)| response)
        }

        /// `request`, plus the peer's `Retry-After` when it is a small
        /// delay-seconds value, so a relay can hand the owner's hint to the
        /// client instead of inventing one.
        #[allow(clippy::too_many_arguments)]
        pub(crate) async fn request_with_retry_after(
            &self,
            expected_node_id: &str,
            base: &str,
            method: reqwest::Method,
            path: &str,
            body: Vec<u8>,
            deadline: tokio::time::Instant,
            max_response_bytes: usize,
            auth_mode: PeerAuthMode,
        ) -> Result<(PeerResponse, Option<u32>), PeerTransportError> {
            self.request_with_optional_header(
                expected_node_id,
                base,
                method,
                path,
                body,
                deadline,
                max_response_bytes,
                auth_mode,
                None,
                None,
            )
            .await
        }

        #[allow(clippy::too_many_arguments)]
        pub(crate) async fn request_with_static_header(
            &self,
            expected_node_id: &str,
            base: &str,
            method: reqwest::Method,
            path: &str,
            body: Vec<u8>,
            deadline: tokio::time::Instant,
            max_response_bytes: usize,
            auth_mode: PeerAuthMode,
            header: (&'static str, &'static str),
        ) -> Result<PeerResponse, PeerTransportError> {
            self.request_with_optional_header(
                expected_node_id,
                base,
                method,
                path,
                body,
                deadline,
                max_response_bytes,
                auth_mode,
                Some(header),
                None,
            )
            .await
            .map(|(response, _)| response)
        }

        pub(crate) async fn clock_request(
            &self,
            expected_node_id: &str,
            base: &str,
        ) -> Result<PeerResponse, PeerTransportError> {
            let guard = self.membership.clock_guard();
            self.request_with_optional_header(
                expected_node_id,
                base,
                reqwest::Method::GET,
                crate::http::internal_clock::PATH,
                Vec::new(),
                deadline_after(Duration::from_secs(2)),
                1024,
                PeerAuthMode::ExactRequestAndMemberResponse,
                None,
                Some(&guard),
            )
            .await
            .map(|(response, _)| response)
        }

        #[allow(clippy::too_many_arguments)]
        async fn request_with_optional_header(
            &self,
            expected_node_id: &str,
            base: &str,
            method: reqwest::Method,
            path: &str,
            body: Vec<u8>,
            deadline: tokio::time::Instant,
            max_response_bytes: usize,
            auth_mode: PeerAuthMode,
            extra_header: Option<(&'static str, &'static str)>,
            clock_guard: Option<&plurx_core::cluster::clock::ClusterClockGuard>,
        ) -> Result<(PeerResponse, Option<u32>), PeerTransportError> {
            let Some(url) = peer_url(base, path) else {
                return Err(PeerTransportError::Unreachable);
            };
            let Some(client) = self.client.as_ref().ok() else {
                return Err(PeerTransportError::Unreachable);
            };
            let clock_ticket = clock_guard.map(|guard| guard.ticket());
            let timestamp_ms = unix_ms();
            let method_name = method.as_str();
            let auth = match auth_mode {
                PeerAuthMode::LegacyActivity => self
                    .membership
                    .sign_activity_request(expected_node_id, timestamp_ms)
                    .map(SignedPeerAuth::Activity),
                PeerAuthMode::ExactRequest
                | PeerAuthMode::ExactRequestAndResponse
                | PeerAuthMode::ExactRequestAndMemberResponse => self
                    .membership
                    .sign_internal_peer_request(
                        expected_node_id,
                        timestamp_ms,
                        &uuid::Uuid::new_v4().to_string(),
                        method_name,
                        path,
                        &body,
                    )
                    .map(SignedPeerAuth::Exact),
            }
            .map_err(|_| PeerTransportError::Unreachable)?;
            let response_binding = match &auth {
                SignedPeerAuth::Exact(auth) => match auth_mode {
                    PeerAuthMode::ExactRequestAndResponse => {
                        Some((auth.node_id.clone(), auth.nonce.clone(), false))
                    }
                    PeerAuthMode::ExactRequestAndMemberResponse => {
                        Some((auth.node_id.clone(), auth.nonce.clone(), true))
                    }
                    _ => None,
                },
                _ => None,
            };
            let mut request = client.request(method, url);
            request = match auth {
                SignedPeerAuth::Activity(auth) => signed_headers(request, &auth),
                SignedPeerAuth::Exact(auth) => signed_headers(request, &auth),
            };
            if let Some((name, value)) = extra_header {
                request = request.header(name, value);
            }
            if !body.is_empty() {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body);
            }
            let response = tokio::time::timeout_at(deadline, request.send())
                .await
                .map_err(|_| PeerTransportError::TimedOut)?
                .map_err(|error| {
                    if error.is_timeout() {
                        PeerTransportError::TimedOut
                    } else {
                        PeerTransportError::Unreachable
                    }
                })?;
            let signature = response
                .headers()
                .get(RESPONSE_SIGNATURE_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let retry_after = retry_after_seconds(response.headers());
            read_then_verify(
                response,
                deadline,
                max_response_bytes,
                timestamp_ms,
                clock_guard.zip(clock_ticket),
                |response| async move {
                    if let Some((target, nonce, member_scoped)) = response_binding {
                        #[cfg(test)]
                        if signature.is_none() {
                            eprintln!("Peer diagnostic stage=missing_response_signature path={path} status={}", response.status);
                        }
                        let signature = signature.ok_or(PeerTransportError::InvalidResponse)?;
                        let payload = signed_response_payload(response.status.as_u16(), &response.body);
                        let verified = if member_scoped {
                            tokio::time::timeout_at(
                                deadline,
                                self.membership.authorize_internal_peer_member_response(
                                    expected_node_id,
                                    &target,
                                    &nonce,
                                    path,
                                    &payload,
                                    &signature,
                                ),
                            )
                            .await
                            .map_err(|_| PeerTransportError::TimedOut)?
                            .map_err(|_| PeerTransportError::InvalidResponse)?
                        } else {
                            tokio::time::timeout_at(
                                deadline,
                                self.membership.authorize_internal_peer_response(
                                    expected_node_id,
                                    &target,
                                    &nonce,
                                    path,
                                    &payload,
                                    &signature,
                                ),
                            )
                            .await
                            .map_err(|_| PeerTransportError::TimedOut)?
                            .map_err(|_| PeerTransportError::InvalidResponse)?
                        };
                        if !verified {
                            #[cfg(test)]
                            eprintln!("Peer diagnostic stage=response_authentication path={path} status={}", response.status);
                            return Err(PeerTransportError::InvalidResponse);
                        }
                    }
                    Ok(response)
                },
            )
            .await
            .map(|response| (response, retry_after))
        }

        /// Send an authenticated peer request but leave the response body as a
        /// stream. Media segments can be much larger than control-plane JSON and
        /// must apply TCP backpressure instead of being accumulated at ingress.
        #[allow(clippy::too_many_arguments)]
        pub(crate) async fn request_stream(
            &self,
            expected_node_id: &str,
            base: &str,
            method: reqwest::Method,
            path: &str,
            body: Vec<u8>,
            deadline: tokio::time::Instant,
            auth_mode: PeerAuthMode,
        ) -> Result<reqwest::Response, PeerTransportError> {
            let Some(url) = peer_url(base, path) else {
                return Err(PeerTransportError::Unreachable);
            };
            let Some(client) = self.client.as_ref().ok() else {
                return Err(PeerTransportError::Unreachable);
            };
            let timestamp_ms = unix_ms();
            let method_name = method.as_str();
            let auth = match auth_mode {
                PeerAuthMode::LegacyActivity => self
                    .membership
                    .sign_activity_request(expected_node_id, timestamp_ms)
                    .map(SignedPeerAuth::Activity),
                // A stream has no complete body to authenticate at this layer.
                PeerAuthMode::ExactRequestAndResponse
                | PeerAuthMode::ExactRequestAndMemberResponse => {
                    return Err(PeerTransportError::InvalidResponse);
                }
                PeerAuthMode::ExactRequest => self
                    .membership
                    .sign_internal_peer_request(
                        expected_node_id,
                        timestamp_ms,
                        &uuid::Uuid::new_v4().to_string(),
                        method_name,
                        path,
                        &body,
                    )
                    .map(SignedPeerAuth::Exact),
            }
            .map_err(|_| PeerTransportError::Unreachable)?;
            let mut request = client.request(method, url);
            request = match auth {
                SignedPeerAuth::Activity(auth) => signed_headers(request, &auth),
                SignedPeerAuth::Exact(auth) => signed_headers(request, &auth),
            };
            if !body.is_empty() {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body);
            }
            tokio::time::timeout_at(deadline, request.send())
                .await
                .map_err(|_| PeerTransportError::TimedOut)?
                .map_err(|error| {
                    if error.is_timeout() {
                        PeerTransportError::TimedOut
                    } else {
                        PeerTransportError::Unreachable
                    }
                })
        }
    }

    /// A peer's `Retry-After` in delay-seconds form, bounded to one minute. An
    /// HTTP-date or anything larger is dropped rather than relayed verbatim.
    pub(crate) fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u32> {
        headers
            .get(reqwest::header::RETRY_AFTER)?
            .to_str()
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|seconds| *seconds <= 60)
    }

    pub(crate) fn signed_response_payload(status: u16, body: &[u8]) -> Vec<u8> {
        let mut payload = Vec::with_capacity(2 + body.len());
        payload.extend_from_slice(&status.to_be_bytes());
        payload.extend_from_slice(body);
        payload
    }

    enum SignedPeerAuth {
        Activity(ActivityPeerAuth),
        Exact(InternalPeerAuth),
    }

    trait PeerAuthHeaders {
        fn node_id(&self) -> &str;
        fn target_node_id(&self) -> &str;
        fn timestamp_ms(&self) -> i64;
        fn nonce(&self) -> Option<&str> {
            None
        }
        fn signature(&self) -> &str;
    }

    impl PeerAuthHeaders for ActivityPeerAuth {
        fn node_id(&self) -> &str {
            &self.node_id
        }

        fn target_node_id(&self) -> &str {
            &self.target_node_id
        }

        fn timestamp_ms(&self) -> i64 {
            self.timestamp_ms
        }

        fn signature(&self) -> &str {
            &self.signature
        }
    }

    impl PeerAuthHeaders for InternalPeerAuth {
        fn node_id(&self) -> &str {
            &self.node_id
        }

        fn target_node_id(&self) -> &str {
            &self.target_node_id
        }

        fn timestamp_ms(&self) -> i64 {
            self.timestamp_ms
        }

        fn nonce(&self) -> Option<&str> {
            Some(&self.nonce)
        }

        fn signature(&self) -> &str {
            &self.signature
        }
    }

    fn signed_headers<T: PeerAuthHeaders>(
        request: reqwest::RequestBuilder,
        auth: &T,
    ) -> reqwest::RequestBuilder {
        let request = request
            .header(NODE_HEADER, auth.node_id())
            .header(TARGET_HEADER, auth.target_node_id())
            .header(TIMESTAMP_HEADER, auth.timestamp_ms())
            .header(SIGNATURE_HEADER, auth.signature());
        match auth.nonce() {
            Some(nonce) => request.header(NONCE_HEADER, nonce),
            None => request,
        }
    }

    pub(crate) fn exact_auth_from_headers(
        headers: &axum::http::HeaderMap,
    ) -> Option<InternalPeerAuth> {
        let node_id = headers.get(NODE_HEADER)?.to_str().ok()?;
        let target_node_id = headers.get(TARGET_HEADER)?.to_str().ok()?;
        let nonce = headers.get(NONCE_HEADER)?.to_str().ok()?;
        let signature = headers.get(SIGNATURE_HEADER)?.to_str().ok()?;
        let nonce_is_canonical = nonce.len() == 36
            && uuid::Uuid::parse_str(nonce)
                .is_ok_and(|parsed| parsed.hyphenated().to_string() == nonce);
        if node_id.is_empty()
            || node_id.len() > 256
            || target_node_id.is_empty()
            || target_node_id.len() > 256
            || !nonce_is_canonical
            || signature.len() != 128
            || !signature.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        Some(InternalPeerAuth {
            node_id: node_id.to_owned(),
            target_node_id: target_node_id.to_owned(),
            timestamp_ms: headers.get(TIMESTAMP_HEADER)?.to_str().ok()?.parse().ok()?,
            nonce: nonce.to_owned(),
            signature: signature.to_owned(),
        })
    }

    pub(crate) fn peer_url(base: &str, path: &str) -> Option<reqwest::Url> {
        if !path.starts_with('/') || path.contains('?') || path.contains('#') {
            return None;
        }
        let normalized = normalize_internal_http_base(base)?;
        let mut url = reqwest::Url::parse(&normalized).ok()?;
        url.set_path(path);
        Some(url)
    }

    async fn read_then_verify<F, Fut>(
        response: reqwest::Response,
        deadline: tokio::time::Instant,
        max_response_bytes: usize,
        timestamp_ms: i64,
        clock: Option<(
            &plurx_core::cluster::clock::ClusterClockGuard,
            plurx_core::cluster::clock::ClockDecisionTicket,
        )>,
        verify: F,
    ) -> Result<PeerResponse, PeerTransportError>
    where
        F: FnOnce(PeerResponse) -> Fut,
        Fut: Future<Output = Result<PeerResponse, PeerTransportError>>,
    {
        let mut response = read_bounded(response, deadline, max_response_bytes).await?;
        // Capture t4 before even constructing the verification future.
        let received_ms = unix_ms();
        if let Some((guard, ticket)) = clock {
            let current = guard.ticket();
            if current.clock_generation != ticket.clock_generation
                || current.state_generation != ticket.state_generation
            {
                #[cfg(test)]
                eprintln!(
                    "Peer diagnostic stage=clock_ticket_changed status={}",
                    response.status
                );
                return Err(PeerTransportError::InvalidResponse);
            }
            response.clock_timing = Some((timestamp_ms, received_ms, ticket));
        }
        tokio::time::timeout_at(deadline, verify(response))
            .await
            .map_err(|_| PeerTransportError::TimedOut)?
    }

    pub(crate) async fn read_bounded(
        mut response: reqwest::Response,
        deadline: tokio::time::Instant,
        max_response_bytes: usize,
    ) -> Result<PeerResponse, PeerTransportError> {
        if response
            .content_length()
            .is_some_and(|length| length > u64::try_from(max_response_bytes).unwrap_or(u64::MAX))
        {
            return Err(PeerTransportError::InvalidResponse);
        }
        let status = response.status();
        let dv_processing_header = response
            .headers()
            .get("x-plurx-dv-processing")
            .filter(|value| value.as_bytes().len() <= 4096)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let processing_skip_header = response
            .headers()
            .get("x-plurx-processing-skip")
            .filter(|value| value.as_bytes().len() <= 4096)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        loop {
            let chunk = tokio::time::timeout_at(deadline, response.chunk())
                .await
                .map_err(|_| PeerTransportError::TimedOut)?
                .map_err(|error| {
                    if error.is_timeout() {
                        PeerTransportError::TimedOut
                    } else {
                        PeerTransportError::Unreachable
                    }
                })?;
            let Some(chunk) = chunk else {
                break;
            };
            if body.len().saturating_add(chunk.len()) > max_response_bytes {
                return Err(PeerTransportError::InvalidResponse);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(PeerResponse {
            dv_processing_header,
            processing_skip_header,
            status,
            body,
            clock_timing: None,
        })
    }

    pub(crate) fn deadline_after(timeout: Duration) -> tokio::time::Instant {
        tokio::time::Instant::now() + timeout
    }

    fn unix_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .unwrap_or_default()
    }

    // END PEER CALL PRODUCTION
    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every transport must take the membership manager's pooled client. A
        /// client built here is a connection pool per caller, which is what made
        /// node-to-node requests redial on every use.
        #[test]
        fn a_transport_never_builds_its_own_http_client() {
            let source = include_str!("cluster.rs");
            let production = source
                .split_once("// BEGIN PEER CALL IMPLEMENTATION")
                .expect("call owner section")
                .1
                .split_once("// END PEER CALL PRODUCTION")
                .expect("production boundary")
                .0;
            assert!(
                production.contains("membership.peer_http()"),
                "PeerTransport::new must take MembershipManager::peer_http()"
            );
            for built_here in ["Client::builder", "Client::new", "ClientBuilder"] {
                assert!(
                    !production.contains(built_here),
                    "peer_transport.rs builds an HTTP client with {built_here}; take \
                     MembershipManager::peer_http() so the connection pool is shared"
                );
            }
        }
        use std::convert::Infallible;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        use axum::body::Body;
        use axum::http::StatusCode;
        use axum::response::Redirect;
        use axum::routing::get;
        use axum::Router;
        use bytes::Bytes;
        use futures_util::{stream, StreamExt};

        #[test]
        fn peer_retry_after_is_bounded_delay_seconds_only() {
            let header = |value: &str| {
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    reqwest::header::RETRY_AFTER,
                    value.parse().expect("header value"),
                );
                headers
            };
            assert_eq!(retry_after_seconds(&header("2")), Some(2));
            assert_eq!(retry_after_seconds(&header("61")), None);
            assert_eq!(
                retry_after_seconds(&header("Wed, 21 Oct 2015 07:28:00 GMT")),
                None
            );
            assert_eq!(
                retry_after_seconds(&reqwest::header::HeaderMap::new()),
                None
            );
        }

        #[test]
        fn exact_header_parser_requires_a_canonical_signed_nonce() {
            let mut household = axum::http::HeaderMap::new();
            household.insert(
                axum::http::header::AUTHORIZATION,
                "Bearer household-session-token"
                    .parse()
                    .expect("household bearer"),
            );
            assert!(
                exact_auth_from_headers(&household).is_none(),
                "internal media routes must not widen a household bearer into peer authority"
            );

            let mut headers = axum::http::HeaderMap::new();
            headers.insert(NODE_HEADER, "node-a".parse().expect("node"));
            headers.insert(TARGET_HEADER, "node-b".parse().expect("target"));
            headers.insert(TIMESTAMP_HEADER, "42".parse().expect("time"));
            headers.insert(
                NONCE_HEADER,
                "123e4567-e89b-42d3-a456-426614174000"
                    .parse()
                    .expect("nonce"),
            );
            headers.insert(
                SIGNATURE_HEADER,
                "a".repeat(128).parse().expect("signature"),
            );
            assert!(exact_auth_from_headers(&headers).is_some());

            headers.insert(NONCE_HEADER, "not-a-uuid".parse().expect("invalid nonce"));
            assert!(exact_auth_from_headers(&headers).is_none());
        }

        #[tokio::test]
        async fn clock_t4_excludes_verification_delay_and_failed_proof_discards_metadata() {
            let app = Router::new().route("/clock", get(|| async { "clock body" }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("clock timing listener");
            let address = listener.local_addr().expect("clock timing address");
            let server = tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("clock timing server");
            });
            let client = reqwest::Client::new();
            let guard = plurx_core::cluster::clock::ClusterClockGuard::new(false);
            let stamp = guard.ticket();
            let signed_t1 = unix_ms();
            let response = client
                .get(format!("http://{address}/clock"))
                .send()
                .await
                .expect("clock response");
            let response = read_then_verify(
                response,
                deadline_after(Duration::from_secs(2)),
                1024,
                signed_t1,
                Some((&guard, stamp)),
                |response| async move {
                    let (_, t4, _) = response
                        .clock_timing
                        .expect("timing captured before verification");
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    assert!(
                        unix_ms() - t4 >= 50,
                        "verification delay does not become t4"
                    );
                    Ok(response)
                },
            )
            .await
            .expect("verified clock response");
            let (t1, _, returned_stamp) = response.clock_timing.expect("verified timing");
            assert_eq!(t1, signed_t1);
            assert_eq!(returned_stamp, stamp);
            let response = client
                .get(format!("http://{address}/clock"))
                .send()
                .await
                .expect("bad proof response");
            assert_eq!(
                read_then_verify(
                    response,
                    deadline_after(Duration::from_secs(2)),
                    1024,
                    signed_t1,
                    Some((&guard, stamp)),
                    |_| async { Err(PeerTransportError::InvalidResponse) }
                )
                .await
                .expect_err("failed proof discards timing"),
                PeerTransportError::InvalidResponse
            );
            server.abort();
        }

        #[tokio::test]
        async fn client_refuses_redirects_and_both_declared_and_chunked_oversized_bodies() {
            let target_hits = Arc::new(AtomicUsize::new(0));
            let hits = Arc::clone(&target_hits);
            let app = Router::new()
                .route(
                    "/redirect",
                    get(|| async { Redirect::temporary("/target") }),
                )
                .route(
                    "/target",
                    get(move || {
                        let hits = Arc::clone(&hits);
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            StatusCode::OK
                        }
                    }),
                )
                .route("/declared-oversized", get(|| async { vec![b'x'; 1025] }))
                .route(
                    "/oversized",
                    get(|| async {
                        let chunks = stream::iter([
                            Ok::<Bytes, Infallible>(Bytes::from(vec![b'x'; 1024])),
                            Ok::<Bytes, Infallible>(Bytes::from_static(b"x")),
                        ]);
                        Body::from_stream(chunks)
                    }),
                )
                .route(
                    "/stalled",
                    get(|| async {
                        let chunks = stream::once(async {
                            Ok::<Bytes, Infallible>(Bytes::from_static(b"x"))
                        })
                        .chain(stream::pending::<Result<Bytes, Infallible>>());
                        Body::from_stream(chunks)
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind peer transport fixture");
            let address = listener.local_addr().expect("fixture address");
            let server = tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve peer transport fixture");
            });
            let transport = PeerTransport::new(MembershipManager::unavailable());
            let client = transport.client.as_ref().expect("peer client");
            let redirect = client
                .get(format!("http://{address}/redirect"))
                .send()
                .await
                .expect("request redirect fixture");
            assert_eq!(redirect.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(target_hits.load(Ordering::SeqCst), 0);

            let declared_oversized = client
                .get(format!("http://{address}/declared-oversized"))
                .send()
                .await
                .expect("request declared oversized fixture");
            assert_eq!(declared_oversized.content_length(), Some(1025));
            assert_eq!(
                read_bounded(
                    declared_oversized,
                    deadline_after(Duration::from_secs(1)),
                    1024,
                )
                .await
                .expect_err("declared body above the exact budget is rejected before streaming"),
                PeerTransportError::InvalidResponse
            );

            let oversized = client
                .get(format!("http://{address}/oversized"))
                .send()
                .await
                .expect("request oversized fixture");
            assert_eq!(
                read_bounded(oversized, deadline_after(Duration::from_secs(1)), 1024)
                    .await
                    .expect_err("chunked body above the exact budget is rejected"),
                PeerTransportError::InvalidResponse
            );

            let stalled = client
                .get(format!("http://{address}/stalled"))
                .send()
                .await
                .expect("request stalled fixture");
            assert_eq!(
                read_bounded(stalled, deadline_after(Duration::from_millis(50)), 1024,)
                    .await
                    .expect_err("a stalled response body must consume no time past its deadline"),
                PeerTransportError::TimedOut
            );
            server.abort();
        }
    }
}

#[cfg(test)]
pub(crate) fn test_calls(membership: MembershipManager) -> calls::PeerTransport {
    calls::PeerTransport::new(membership)
}
