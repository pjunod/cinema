//! `NodeLifecycle` as a checked projection (TRANSCODE-DECOMPOSITION-PLAN §3.8,
//! milestone M7).
//!
//! Membership state is not stored as one value. It is inferred, at each
//! decision, from replicated rows (`cluster_nodes`, `cluster_node_capabilities`,
//! `cluster_node_join_staging`, `cluster_node_removals` and its attempt
//! references, `cluster_node_promotions`, `cluster_node_maintenance`) and from
//! the committed Raft membership. The production decisions read those sources
//! directly: `capability_unready_node_predicate` and
//! `capability_ready_predicate` are SQL evaluated inside Raft transactions,
//! `node_is_tombstoned` is a consistent SQL count, and
//! `local_node_is_committed_voter` reads Raft metrics.
//!
//! This module computes the same facts from row-shaped inputs as a struct of
//! *independent* dimensions. It is deliberately not a flat state enum and not
//! a transition table: the assessment (F-sc-12) refused a flat enum as a
//! proven product state, so the projection has to agree with the predicates
//! first. `cluster::membership::tests::lifecycle_projection_agrees` loads every
//! row shape into the production table definitions and checks the projection
//! against the SQL statements and the Raft read production decides on, run
//! as production runs them:
//!
//! - readiness and `join`: `capability_unready_node_predicate` per node and
//!   `capability_ready_predicate` over the roster;
//! - `removal`: `NODE_TOMBSTONED_SQL` for the tombstone; for a removal in
//!   progress, the attempt set against the three reads production makes of
//!   it (`ROLLBACK_REMOVAL_FENCE_SQL`, which needs it empty,
//!   `BEGIN_REMOVAL_INTENT_SQL`, which needs one attempt in it, and
//!   `EXISTING_REMOVAL_ATTEMPT_SQL`, its least attempt). `Tombstoned` carries
//!   no attempt set: the references are left behind on purpose and no removal
//!   read consults them again;
//! - `durable_role`: `admitted_learner_nodes_sql`;
//! - `maintenance`: `NODE_MAINTENANCE_COUNT_SQL` for a request, and
//!   `EXIT_MAINTENANCE_SQL` (which deletes only an acknowledged request of an
//!   active node) for `Acknowledged` on an active node. Production reads a
//!   tombstoned node's acknowledgement nowhere, so it is not agreed there;
//! - `membership`: `committed_voter_ids!`, the read
//!   `local_node_is_committed_voter` applies to Raft metrics, over real
//!   `openraft` memberships including both joint shapes;
//! - `promotion`: only whether an audit row exists (`NODE_PROMOTION_COUNT_SQL`).
//!   The `Started` / `Barrier` / `Joint` split has **no production reader**:
//!   nothing reads `barrier_index` back. The test pins that split to §3.8's
//!   specification, which is not an agreement with production.
//!
//! Nothing in the daemon reads this projection yet; a transition function is
//! a later milestone, written only after the projection has agreed with
//! production for a release.

use std::collections::BTreeSet;

/// One `cluster_nodes` row, as the membership predicates read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNodeRow {
    pub node_id: String,
    pub raft_id: u64,
    pub last_seen_at: i64,
    pub removed_at: Option<i64>,
    /// The additive `role` column: `'learner'` until a promotion completes,
    /// then `'voter'`; `NULL` for a node that joined as a voter before the
    /// column existed.
    pub role: Option<String>,
}

/// One `cluster_node_capabilities` row for the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRow {
    pub capability: String,
    pub last_seen_at: i64,
}

/// The node's `cluster_node_removals` row with the attempt ids that
/// `cluster_node_removal_attempts` holds for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalRow {
    pub started_at: i64,
    pub attempt_ids: Vec<String>,
}

/// The node's `cluster_node_promotions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionRow {
    pub attempt_id: String,
    pub barrier_index: Option<i64>,
}

/// The node's `cluster_node_maintenance` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceRow {
    pub requested_at: i64,
    pub acknowledged_at: Option<i64>,
}

/// Every replicated row the lifecycle predicates read about one node.
#[derive(Debug, Clone, Copy)]
pub struct NodeRows<'a> {
    pub node: &'a ClusterNodeRow,
    pub capabilities: &'a [CapabilityRow],
    /// A `cluster_node_join_staging` row exists for the node.
    pub staged: bool,
    pub removal: Option<&'a RemovalRow>,
    pub promotion: Option<&'a PromotionRow>,
    pub maintenance: Option<&'a MaintenanceRow>,
}

/// The committed Raft membership, reduced to what the projection reads.
///
/// Built from the same `StoredMembership` accessors production calls
/// (`voter_ids()`, `nodes()`, `get_joint_config()`), so a joint configuration
/// is represented as it is committed rather than collapsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RaftMembershipView {
    voters: BTreeSet<u64>,
    members: BTreeSet<u64>,
    joint: bool,
}

impl RaftMembershipView {
    /// `voters` is `voter_ids()` (the union over a joint configuration),
    /// `members` is every id in `nodes()`, and `joint` is
    /// `get_joint_config().len() > 1`.
    pub fn new(
        voters: impl IntoIterator<Item = u64>,
        members: impl IntoIterator<Item = u64>,
        joint: bool,
    ) -> Self {
        let voters = voters.into_iter().collect::<BTreeSet<_>>();
        let mut members = members.into_iter().collect::<BTreeSet<_>>();
        members.extend(voters.iter().copied());
        Self {
            voters,
            members,
            joint,
        }
    }
}

/// Whether `raft_id` is a voter of the committed configuration.
///
/// The one rule `MembershipManager::local_node_is_committed_voter` applies to
/// Raft metrics; the projection's [`Membership::Voter`] is defined by it.
pub(crate) fn committed_voter(voter_ids: impl IntoIterator<Item = u64>, raft_id: u64) -> bool {
    voter_ids.into_iter().any(|voter| voter == raft_id)
}

/// Raft membership of the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    Absent,
    Learner,
    Voter,
}

/// The durable `role` column, read the way every predicate in `membership.rs`
/// reads it: a node that is not a learner is a voter (`role IS NOT 'learner'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableRole {
    Learner,
    Voter,
}

/// Whether the node has redeemed a join and not yet heartbeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    Settled,
    Staged,
}

/// Whether the binary running on the node now has proven one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    NotReady,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Maintenance {
    None,
    Requested,
    Acknowledged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    None,
    /// A replicated removal fence exists and the final tombstone has not been
    /// written. The attempt references are kept because each concurrent or
    /// ambiguous attempt owns one.
    InProgress {
        attempts: BTreeSet<String>,
    },
    /// `removed_at` is set.
    Tombstoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promotion {
    None,
    /// The audit row exists and no apply barrier has been recorded yet.
    Started,
    /// The audit row records the target-local apply barrier.
    Barrier {
        index: i64,
    },
    /// The audit row exists while the committed Raft configuration is joint:
    /// the interval between the barrier and Hiqlite's uniform voter set that
    /// the promotion table exists for. Never collapsed into `Barrier`.
    Joint,
}

/// Independent lifecycle dimensions of one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleView {
    pub node_id: String,
    pub membership: Membership,
    pub durable_role: DurableRole,
    pub join: Join,
    /// Capabilities this node's running binary has proven; any other
    /// capability is [`Readiness::NotReady`].
    ready_capabilities: BTreeSet<String>,
    pub maintenance: Maintenance,
    pub removal: Removal,
    pub promotion: Promotion,
}

impl LifecycleView {
    pub fn readiness(&self, capability: &str) -> Readiness {
        if self.ready_capabilities.contains(capability) {
            Readiness::Ready
        } else {
            Readiness::NotReady
        }
    }

    /// This node holds back cluster-wide readiness for `capability`: it is
    /// active (not tombstoned), it is not mid-join, and it has not proven the
    /// capability. `capability_unready_node_predicate` as a projection.
    pub fn holds_back(&self, capability: &str) -> bool {
        self.removal != Removal::Tombstoned
            && self.join == Join::Settled
            && self.readiness(capability) == Readiness::NotReady
    }

    /// `node_is_tombstoned`: removal has started (the fence exists) or has
    /// finished (`removed_at` is set).
    pub fn is_tombstoned(&self) -> bool {
        self.removal != Removal::None
    }

    /// The node is on `admitted_learner_nodes_sql`'s roster.
    pub fn is_admitted_learner(&self) -> bool {
        self.durable_role == DurableRole::Learner && self.removal != Removal::Tombstoned
    }

    /// `local_node_is_committed_voter` for this node.
    pub fn is_committed_voter(&self) -> bool {
        self.membership == Membership::Voter
    }
}

/// Every active node proves `capability`: `capability_ready_predicate` as a
/// projection over the whole roster.
pub fn capability_ready(views: &[LifecycleView], capability: &str) -> bool {
    !views.iter().any(|view| view.holds_back(capability))
}

/// Project one node's rows and the committed Raft membership into
/// independent lifecycle dimensions.
pub fn observed_lifecycle(rows: NodeRows<'_>, raft: &RaftMembershipView) -> LifecycleView {
    let node = rows.node;
    let membership = if committed_voter(raft.voters.iter().copied(), node.raft_id) {
        Membership::Voter
    } else if raft.members.contains(&node.raft_id) {
        Membership::Learner
    } else {
        Membership::Absent
    };
    let durable_role = if node.role.as_deref() == Some("learner") {
        DurableRole::Learner
    } else {
        DurableRole::Voter
    };
    let join = if rows.staged {
        Join::Staged
    } else {
        Join::Settled
    };
    let ready_capabilities = rows
        .capabilities
        .iter()
        .filter(|row| row.last_seen_at == node.last_seen_at)
        .map(|row| row.capability.clone())
        .collect();
    let maintenance = match rows.maintenance {
        None => Maintenance::None,
        Some(MaintenanceRow {
            acknowledged_at: None,
            ..
        }) => Maintenance::Requested,
        Some(MaintenanceRow {
            acknowledged_at: Some(_),
            ..
        }) => Maintenance::Acknowledged,
    };
    let removal = match (node.removed_at, rows.removal) {
        (Some(_), _) => Removal::Tombstoned,
        (None, Some(removal)) => Removal::InProgress {
            attempts: removal.attempt_ids.iter().cloned().collect(),
        },
        (None, None) => Removal::None,
    };
    let promotion = match rows.promotion {
        None => Promotion::None,
        Some(_) if raft.joint => Promotion::Joint,
        Some(PromotionRow {
            barrier_index: Some(index),
            ..
        }) => Promotion::Barrier { index: *index },
        Some(PromotionRow {
            barrier_index: None,
            ..
        }) => Promotion::Started,
    };
    LifecycleView {
        node_id: node.node_id.clone(),
        membership,
        durable_role,
        join,
        ready_capabilities,
        maintenance,
        removal,
        promotion,
    }
}
