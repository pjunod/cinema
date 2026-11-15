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
    roster_now, AppliedStanding, DirectoryReadCounts, PeerDirectory, RosterSnapshot,
};
use plurx_core::cluster::membership::{
    ActivityPeer, LocalServingRole, MembershipError, MembershipManager,
};

use crate::serving_fence::{ServingAuthority, ServingFence};

#[cfg(test)]
mod census;

/// The facade. Cheap to clone; clones share one peer directory.
#[derive(Clone)]
pub(crate) struct Cluster {
    fence: ServingFence,
    membership: MembershipManager,
    directory: Directory,
}

impl Cluster {
    pub(crate) fn new(fence: ServingFence, membership: MembershipManager) -> Self {
        Self {
            directory: Directory::new(membership.clone()),
            fence,
            membership,
        }
    }

    /// May this node serve or commit now: the serving fence's authority.
    #[allow(dead_code)] // slice 2 moves the Live TV and DVR fence reads here
    pub(crate) fn authority(&self) -> ServingAuthority {
        self.fence.authority()
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

    /// Who can help, for `purpose`; see [`Directory::peers`].
    pub(crate) async fn peers(&self, purpose: Purpose) -> Result<Peers, ClusterError> {
        self.directory.peers(purpose).await
    }

    /// The shared directory, for a subsystem that keeps its own handle.
    pub(crate) fn directory(&self) -> Directory {
        self.directory.clone()
    }
}

/// This node's place in the cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalRole {
    pub(crate) standing: Standing,
    pub(crate) admission: Admission,
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

/// The peer directory (B2): one cached roster snapshot per process, kept
/// current by the applied mark and the applied Raft membership, never by a
/// timer. A hit costs atomic loads and the membership watch; a miss is one
/// local read.
#[derive(Clone)]
pub(crate) struct Directory {
    inner: PeerDirectory,
}

impl Directory {
    pub(crate) fn new(membership: MembershipManager) -> Self {
        Self {
            inner: PeerDirectory::new(membership),
        }
    }

    /// The peers for `purpose`, evaluated now against the current snapshot.
    pub(crate) async fn peers(&self, purpose: Purpose) -> Result<Peers, ClusterError> {
        let snapshot = self.inner.snapshot().await.map_err(ClusterError)?;
        let now = roster_now().map_err(ClusterError)?;
        let list = match purpose {
            Purpose::Voters => snapshot.voters(now),
            Purpose::MediaWorkers => snapshot.media_workers(now, true).map_err(ClusterError)?,
        };
        Ok(Peers::new(&snapshot, list))
    }

    pub(crate) fn reads(&self) -> DirectoryReadCounts {
        self.inner.reads()
    }
}

/// One purpose's peers, and the voter facts of the snapshot they came from.
pub(crate) struct Peers {
    list: Vec<ActivityPeer>,
    voter_count: usize,
    /// `None` for an unclustered process, which has no membership.
    local_is_voter: Option<bool>,
}

impl Peers {
    fn new(snapshot: &RosterSnapshot, list: Vec<ActivityPeer>) -> Self {
        Self {
            list,
            voter_count: snapshot.voter_count(),
            local_is_voter: snapshot.local_is_voter().ok(),
        }
    }

    /// Peers over a fixed list and voter facts.
    #[cfg(test)]
    pub(crate) fn for_test(
        list: Vec<ActivityPeer>,
        voter_count: usize,
        local_is_voter: Option<bool>,
    ) -> Self {
        Self {
            list,
            voter_count,
            local_is_voter,
        }
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

    /// Whether this node is a committed voter, on the same snapshot; an
    /// unclustered process has no membership to be one of.
    pub(crate) fn local_is_voter(&self) -> Result<bool, ClusterError> {
        self.local_is_voter
            .ok_or(ClusterError(MembershipError::Unavailable))
    }

    /// Whether every committed voter is visible: the other voters plus this
    /// node when it is one (audit F1). Meaningful for [`Purpose::Voters`];
    /// the three facts come from one snapshot, so a membership change cannot
    /// fall between them.
    pub(crate) fn complete(&self) -> Result<bool, ClusterError> {
        Ok(self
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
mod tests {
    use super::*;

    fn peer(node: &str, reachable: bool, base: bool) -> ActivityPeer {
        ActivityPeer {
            node_id: node.to_owned(),
            raft_id: 2,
            http_base: base.then(|| format!("http://{node}.invalid")),
            reachable,
        }
    }

    /// F1: "every voter is visible" counts this node only when it is a
    /// voter. Counting it unconditionally made the clause `n != n + 1` on a
    /// learner, in two files; it now exists once.
    #[test]
    fn complete_counts_this_node_only_when_it_votes() {
        let one = vec![peer("peer-a", true, true)];
        assert!(Peers::for_test(one.clone(), 2, Some(true))
            .complete()
            .expect("clustered"));
        assert!(!Peers::for_test(one.clone(), 3, Some(true))
            .complete()
            .expect("clustered"));
        assert!(Peers::for_test(one.clone(), 1, Some(false))
            .complete()
            .expect("clustered"));
        assert!(!Peers::for_test(one.clone(), 2, Some(false))
            .complete()
            .expect("clustered"));
        assert!(Peers::for_test(one, 1, None).complete().is_err());
    }

    #[test]
    fn find_names_only_a_reachable_peer_with_a_base() {
        let peers = Peers::for_test(
            vec![
                peer("up", true, true),
                peer("down", false, true),
                peer("baseless", true, false),
            ],
            4,
            Some(true),
        );
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
        );
        assert_eq!(
            cluster.role(),
            LocalRole {
                standing: Standing::Standalone,
                admission: Admission::Open
            }
        );
        let voters = cluster.peers(Purpose::Voters).await.expect("standalone");
        assert!(voters.all().is_empty());
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
