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
    ActivityPeer, InternalPeerAuth, LocalServingRole, MembershipError, MembershipManager,
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
    /// The facade over this node's fence and manager; `mark` is the applied
    /// mark the peer directory stamps with (`AppliedView::applied_mark`).
    pub(crate) fn new(
        fence: ServingFence,
        membership: MembershipManager,
        mark: Option<hiqlite::AppliedMarkHandle>,
    ) -> Self {
        Self {
            directory: Directory::with_mark(membership.clone(), mark),
            fence,
            membership,
        }
    }

    /// May this node serve or commit now: the serving fence's authority.
    pub(crate) fn authority(&self) -> ServingAuthority {
        self.fence.authority()
    }

    /// Resolves once, with the error, when this node's own database Raft
    /// core has quit on a fatal error; never for an ordinary stop or an
    /// unclustered process. The daemon drains and exits on it.
    pub(crate) fn core_failure(
        &self,
    ) -> impl std::future::Future<Output = String> + Send + 'static {
        self.membership.raft_core_failure()
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

    /// Exact member authentication remains a fresh membership proof, never a
    /// cached peer-directory lookup. Activity read and Stop use their
    /// existing, distinct authorization windows.
    pub(crate) async fn verify_member_read_request(
        &self,
        auth: &InternalPeerAuth,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<bool, ClusterError> {
        self.membership
            .authorize_internal_peer_read_request(auth, method, path, body)
            .await
            .map_err(ClusterError)
    }

    pub(crate) async fn verify_member_request(
        &self,
        auth: &InternalPeerAuth,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<bool, ClusterError> {
        self.membership
            .authorize_internal_peer_request(auth, method, path, body)
            .await
            .map_err(ClusterError)
    }

    pub(crate) fn sign_member_response(
        &self,
        target_node_id: &str,
        request_nonce: &str,
        path: &str,
        response: &[u8],
    ) -> Result<String, ClusterError> {
        self.membership
            .sign_internal_peer_response(target_node_id, request_nonce, path, response)
            .map_err(ClusterError)
    }
}

/// Process-local writer observations projected through the raw cluster owner.
/// These fixed-label counters need neither a Store nor serving authority.
pub(crate) struct WriterAbandonedReply {
    pub(crate) request: &'static str,
    pub(crate) ok: u64,
    pub(crate) error: u64,
}
pub(crate) fn writer_abandoned_replies() -> Vec<WriterAbandonedReply> {
    hiqlite::db_writer_abandoned_replies()
        .into_iter()
        .map(|row| WriterAbandonedReply {
            request: row.request,
            ok: row.ok,
            error: row.error,
        })
        .collect()
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
    /// Every other committed member, including learners and stale rows.
    Operations,
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
            ["node-2", "node-3", "node-4", "node-5"],
            "operations: every committed member, including stale voters and learners"
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
