//! The peer directory: one roster read serving every peer purpose (cluster
//! facade B2, `docs/cluster/CLUSTER-FACADE-BUILD.md` §4.3).
//!
//! [`RosterSnapshot`] is one local statement — the union of what
//! `activity_peers`, `media_peers` and `operations_peers` each read, without
//! their `LIMIT` — paired with the applied Raft membership it was read under.
//! Each purpose's eligibility is a pure function over it
//! ([`RosterSnapshot::voters`], [`RosterSnapshot::media_workers`],
//! [`RosterSnapshot::operations`], [`RosterSnapshot::voter_count`],
//! [`RosterSnapshot::local_is_voter`]) that reproduces its producer's rule
//! exactly, caps included. Reachability and progress freshness take the
//! clock as an argument, so a cached snapshot still ages.
//!
//! [`MembershipManager::roster_snapshot`] stamps a read with the rule
//! `AppliedView` uses: load the stamp `s0` ([`RosterStamp`]); refuse to
//! stamp if the image generation is odd, the membership is not committed, or
//! a table has no slot in the applied mark; read; reload `s1`; keep the stamp
//! only if `s0 == s1`. A cache of the snapshot is current only while
//! [`MembershipManager::roster_stamp`] still equals `s0`: [`PeerDirectory`]
//! keeps that cache. A miss is one local read. No timer, no TTL, no
//! background task.
//!
//! The stamp needs both signals. Roster rows change through ordinary log
//! entries (a removal fence is a row), and the membership changes through
//! Raft (a learner admitted or promoted moves no row the directory reads).

use super::*;

/// The tables the roster reads. Each has a slot in the applied mark
/// ([`crate::store::APPLIED_VIEW_TABLES`]).
pub const ROSTER_TABLES: [&str; 4] = [
    "cluster_nodes",
    "cluster_node_http",
    "cluster_node_removals",
    "cluster_node_progress",
];

/// Every remote, non-removed, unfenced node with its endpoint and progress,
/// in Raft id order. The `WHERE` is the one `activity_peers`, `media_peers`
/// and `operations_peers` share; their `LIMIT` and membership filters are
/// applied by the purpose functions below.
const ROSTER_SQL: &str = "SELECT node.node_id, node.raft_id, node.last_seen_at, node.role, \
            http.public_http_url, \
            COALESCE(progress.bounded_read_ready, 0) AS bounded_read_ready, \
            progress.observed_at \
     FROM cluster_nodes node \
     LEFT JOIN cluster_node_http http ON http.node_id = node.node_id \
     LEFT JOIN cluster_node_progress progress ON progress.node_id = node.node_id \
     WHERE node.node_id != $1 AND node.removed_at IS NULL \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
         WHERE removal.node_id = node.node_id) \
     ORDER BY node.raft_id";

/// What a cached roster was read under. IO-free to load: atomics of the
/// applied mark and the in-process Raft membership watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RosterStamp {
    image_generation: u64,
    /// Membership entry (leader term, leader node id, log index).
    membership_log: Option<(u64, u64, u64)>,
    /// One revision per [`ROSTER_TABLES`] entry, in that order.
    revisions: [u64; 4],
}

/// The applied roster of this node, as one read saw it.
pub struct RosterSnapshot {
    clustered: bool,
    local_raft_id: u64,
    members: BTreeSet<u64>,
    voters: BTreeSet<u64>,
    rows: Vec<MediaPeerRow>,
    stamp: Option<RosterStamp>,
}

/// This node's standing in the applied Raft membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppliedStanding {
    /// No replicated membership: this process is the whole server.
    Standalone,
    Voter,
    Learner,
    /// Clustered, but the applied membership does not list this node.
    Absent,
}

impl RosterSnapshot {
    /// The roster of an unclustered process: no peers, and it is the one
    /// voter.
    #[must_use]
    pub fn standalone() -> Self {
        Self {
            clustered: false,
            local_raft_id: 0,
            members: BTreeSet::new(),
            voters: BTreeSet::new(),
            rows: Vec::new(),
            stamp: None,
        }
    }

    #[must_use]
    pub fn is_clustered(&self) -> bool {
        self.clustered
    }

    /// The stamp this snapshot is cached under; `None` for an uncached read.
    #[must_use]
    pub fn stamp(&self) -> Option<RosterStamp> {
        self.stamp
    }

    /// The other committed voters, not removed or fenced, reachable or not
    /// (`activity_peers`): at most [`MAX_ACTIVITY_PEERS`], taken from the
    /// first [`MAX_ACTIVITY_PEERS`] rows in Raft id order.
    #[must_use]
    pub fn voters(&self, now: i64) -> Vec<ActivityPeer> {
        let voters = self
            .voters
            .iter()
            .copied()
            .take(MAX_ACTIVITY_PEERS.saturating_add(1))
            .collect::<BTreeSet<_>>();
        self.rows
            .iter()
            .take(MAX_ACTIVITY_PEERS)
            .filter(|row| voters.contains(&row.raft_id))
            .take(MAX_ACTIVITY_PEERS)
            .map(|row| peer(now, row))
            .collect()
    }

    /// Committed voters including this node (`activity_voter_count`); one
    /// for an unclustered process.
    #[must_use]
    pub fn voter_count(&self) -> usize {
        if !self.clustered {
            return 1;
        }
        self.voters
            .iter()
            .take(MAX_ACTIVITY_PEERS.saturating_add(2))
            .count()
    }

    /// Ready media targets (`media_peers_for_route`): committed members with
    /// a ready progress row observed within the reachable window, learners
    /// only when `allow_learners`. A stored role this binary does not know
    /// fails the whole directory, as it always has.
    pub fn media_workers(
        &self,
        now: i64,
        allow_learners: bool,
    ) -> Result<Vec<ActivityPeer>, MembershipError> {
        let members = self
            .members
            .iter()
            .copied()
            .take(MAX_ACTIVITY_PEERS.saturating_add(1))
            .collect::<BTreeSet<_>>();
        let mut peers = Vec::new();
        for row in self.rows.iter().take(MAX_ACTIVITY_PEERS).filter(|row| {
            members.contains(&row.raft_id)
                && row.bounded_read_ready
                && node_is_reachable(now, row.observed_at.unwrap_or_default())
        }) {
            let role = ClusterRole::from_stored(row.admitted_role.as_deref())?;
            if !allow_learners && role == ClusterRole::Learner {
                continue;
            }
            peers.push(peer(now, row));
            if peers.len() == MAX_ACTIVITY_PEERS {
                break;
            }
        }
        Ok(peers)
    }

    /// Every other committed member, stale or not (`operations_peers`). A
    /// roster beyond the bounded operations limit fails closed.
    pub fn operations(&self, now: i64) -> Result<Vec<ActivityPeer>, MembershipError> {
        if !self.clustered {
            return Ok(Vec::new());
        }
        bounded_committed_raft_ids_json(&self.members)?;
        Ok(self
            .rows
            .iter()
            .filter(|row| self.members.contains(&row.raft_id))
            .map(|row| peer(now, row))
            .collect())
    }

    /// Whether this node is a committed voter
    /// (`local_node_is_committed_voter`); an unclustered process has no
    /// membership to be one of.
    pub fn local_is_voter(&self) -> Result<bool, MembershipError> {
        if !self.clustered {
            return Err(MembershipError::Unavailable);
        }
        Ok(lifecycle::committed_voter(
            self.voters.iter().copied(),
            self.local_raft_id,
        ))
    }
}

/// The wall clock the purposes are evaluated at, as the producers read it.
pub fn roster_now() -> Result<i64, MembershipError> {
    unix_ms()
}

fn peer(now: i64, row: &MediaPeerRow) -> ActivityPeer {
    ActivityPeer {
        http_base: row.http_base.clone(),
        node_id: row.node_id.clone(),
        raft_id: row.raft_id,
        reachable: node_is_reachable(now, row.last_seen_at),
    }
}

impl MembershipManager {
    /// The stamp of the roster as this node has applied it, IO-free; `None`
    /// for an unclustered process or when no read could be cached.
    #[must_use]
    pub fn roster_stamp(&self) -> Option<RosterStamp> {
        roster_stamp(self.inner.as_deref()?).map(|(stamp, _)| stamp)
    }

    /// One local read of the roster, stamped only when nothing moved under
    /// it. An unclustered process has the empty roster and reads nothing.
    pub async fn roster_snapshot(&self) -> Result<RosterSnapshot, MembershipError> {
        match self.inner.as_deref() {
            Some(inner) => read_roster(inner).await,
            None => Ok(RosterSnapshot::standalone()),
        }
    }
    /// This node's standing in the applied Raft membership — the source
    /// [`Self::local_node_is_committed_voter`] reads — with no IO or await.
    #[must_use]
    pub fn applied_standing(&self) -> AppliedStanding {
        let Some(inner) = self.inner.as_deref() else {
            return AppliedStanding::Standalone;
        };
        let membership = inner.local_metrics.membership_snapshot();
        let local = inner.identity.raft_id;
        if membership.voters.contains(&local) {
            AppliedStanding::Voter
        } else if membership.members.contains(&local) {
            AppliedStanding::Learner
        } else {
            AppliedStanding::Absent
        }
    }

    /// The heartbeat-published serving role, as
    /// [`Self::local_serving_role`] answers it, without the `async`.
    #[must_use]
    pub fn published_serving_role(&self) -> LocalServingRole {
        self.inner
            .as_deref()
            .map_or(LocalServingRole::Unclustered, |inner| {
                LocalServingRole::from_encoded(inner.local_serving_role.load(Ordering::Acquire))
            })
    }
}

/// Reads answered by a [`PeerDirectory`], by outcome: `hit` from the cached
/// snapshot, `reload` a read that replaced it, `uncached` a read that could
/// not be cached (no stamp, or the roster moved during the read).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectoryReadCounts {
    pub hit: u64,
    pub reload: u64,
    pub uncached: u64,
}

#[derive(Default)]
struct DirectoryReads {
    hit: AtomicU64,
    reload: AtomicU64,
    uncached: AtomicU64,
}

/// The cached roster of one membership manager. Clones share the cache.
#[derive(Clone)]
pub struct PeerDirectory {
    membership: MembershipManager,
    cached: Arc<Mutex<Option<Arc<RosterSnapshot>>>>,
    standalone: Arc<RosterSnapshot>,
    reads: Arc<DirectoryReads>,
}

impl PeerDirectory {
    #[must_use]
    pub fn new(membership: MembershipManager) -> Self {
        Self {
            membership,
            cached: Arc::default(),
            standalone: Arc::new(RosterSnapshot::standalone()),
            reads: Arc::default(),
        }
    }

    /// The current applied roster. An unclustered process answers its empty
    /// roster with no read, counted as a hit.
    pub async fn snapshot(&self) -> Result<Arc<RosterSnapshot>, MembershipError> {
        if !self.membership.is_replicated() {
            self.reads.hit.fetch_add(1, Ordering::Relaxed);
            return Ok(Arc::clone(&self.standalone));
        }
        if let Some(current) = self.membership.roster_stamp() {
            let cached = self.cached.lock().expect("peer directory cache").clone();
            if let Some(cached) = cached.filter(|cached| cached.stamp == Some(current)) {
                self.reads.hit.fetch_add(1, Ordering::Relaxed);
                return Ok(cached);
            }
        }
        let snapshot = Arc::new(self.membership.roster_snapshot().await?);
        if snapshot.stamp.is_some() {
            *self.cached.lock().expect("peer directory cache") = Some(Arc::clone(&snapshot));
            self.reads.reload.fetch_add(1, Ordering::Relaxed);
        } else {
            self.reads.uncached.fetch_add(1, Ordering::Relaxed);
        }
        Ok(snapshot)
    }

    #[must_use]
    pub fn reads(&self) -> DirectoryReadCounts {
        DirectoryReadCounts {
            hit: self.reads.hit.load(Ordering::Relaxed),
            reload: self.reads.reload.load(Ordering::Relaxed),
            uncached: self.reads.uncached.load(Ordering::Relaxed),
        }
    }
}

/// The IO-free half: the stamp of the roster as this node has applied it,
/// with the membership it names. `None` when no read can be cached: an
/// uncommitted (or not running) membership, no local applied mark, an odd
/// image generation, or a roster table without a slot.
fn roster_stamp(
    inner: &ReplicatedMembership,
) -> Option<(RosterStamp, hiqlite::LocalDbMembershipSnapshot)> {
    let membership = inner.local_metrics.membership_snapshot();
    if !membership.committed {
        return None;
    }
    let mark = inner.client.local_db_applied_mark().ok()?;
    let image_generation = mark.generation();
    if image_generation % 2 == 1 {
        return None;
    }
    let mut revisions = [0; 4];
    for (revision, table) in revisions.iter_mut().zip(ROSTER_TABLES) {
        *revision = mark.revision(mark.slot(table)?);
    }
    Some((
        RosterStamp {
            image_generation,
            membership_log: membership.membership_log,
            revisions,
        },
        membership,
    ))
}

/// One local read of the roster, stamped only when nothing moved under it.
async fn read_roster(inner: &ReplicatedMembership) -> Result<RosterSnapshot, MembershipError> {
    let before = roster_stamp(inner);
    let (members, voters) = match &before {
        Some((_, membership)) => (membership.members.clone(), membership.voters.clone()),
        None => {
            let membership = inner.local_metrics.membership_snapshot();
            (membership.members, membership.voters)
        }
    };
    let rows = inner
        .client
        .query_map::<MediaPeerRow, _>(ROSTER_SQL, params!(inner.identity.node_id.as_str()))
        .await?;
    let after = roster_stamp(inner).map(|(stamp, _)| stamp);
    let stamp = before
        .map(|(stamp, _)| stamp)
        .filter(|stamp| after == Some(*stamp));
    Ok(RosterSnapshot {
        clustered: true,
        local_raft_id: inner.identity.raft_id,
        members,
        voters,
        rows,
        stamp,
    })
}

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "hiqlite-contract-tests"))]
mod invalidation_tests;
