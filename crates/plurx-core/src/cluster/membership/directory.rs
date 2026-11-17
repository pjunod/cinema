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
//! `AppliedView` uses, against the node's applied mark (the handle the
//! applied view holds; the manager's counted client hands out no raw one):
//! load the stamp `s0` ([`RosterStamp`]); refuse to stamp if the image
//! generation is odd or the membership is not committed; read; reload `s1`;
//! keep the stamp only if `s0 == s1`. The stamp's revisions are the roster
//! tables' own when the mark registers them, else the applied index.
//! [`RosterDirectory`] keeps the stamped snapshot and answers from it only
//! while the current stamp still equals `s0`. A miss is one local read. No
//! timer, no TTL, no background task.
//!
//! The stamp needs both signals. Roster rows change through ordinary log
//! entries (a removal fence is a row), and the membership changes through
//! Raft (a learner admitted or promoted moves no row the directory reads).

use super::*;

/// The tables the roster reads. When each has a slot in the applied mark the
/// stamp tracks them table by table. Production registers all four; an
/// older or custom mark missing a slot falls back to the applied index,
/// which moves on every applied entry.
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

/// One roster row for [`RosterSnapshot::for_tests`], in Raft id order.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TestRosterRow {
    pub node_id: String,
    pub raft_id: u64,
    pub last_seen_at: i64,
    pub role: Option<String>,
    pub http_base: Option<String>,
    pub bounded_read_ready: bool,
    pub observed_at: Option<i64>,
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

    /// A clustered snapshot over fixed rows, for tests outside this crate
    /// that need a roster without a replicated node. Never cached.
    #[doc(hidden)]
    #[must_use]
    pub fn for_tests(
        local_raft_id: u64,
        members: &[u64],
        voters: &[u64],
        rows: Vec<TestRosterRow>,
    ) -> Self {
        Self {
            clustered: true,
            local_raft_id,
            members: members.iter().copied().collect(),
            voters: voters.iter().copied().collect(),
            rows: rows
                .into_iter()
                .map(|row| MediaPeerRow {
                    node_id: row.node_id,
                    admitted_role: row.role,
                    raft_id: row.raft_id,
                    last_seen_at: row.last_seen_at,
                    http_base: row.http_base,
                    bounded_read_ready: row.bounded_read_ready,
                    observed_at: row.observed_at,
                })
                .collect(),
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

    /// Artwork recovery's original local-SQL rule: all reachable, nonremoved
    /// remote endpoints, including uncommitted nodes, with no media peer cap.
    /// The shared SQL already excludes removal rows and the local node.
    #[must_use]
    pub fn artwork(&self, now: i64) -> Vec<ActivityPeer> {
        let mut rows = self
            .rows
            .iter()
            .filter(|row| row.http_base.is_some() && row.last_seen_at >= reachable_after(now))
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            b.last_seen_at
                .cmp(&a.last_seen_at)
                .then_with(|| a.raft_id.cmp(&b.raft_id))
        });
        rows.into_iter().map(|row| peer(now, row)).collect()
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
    /// One local read of the roster, stamped against `mark` (this node's
    /// applied mark) only when nothing moved under it; unstamped without a
    /// mark. An unclustered process has the empty roster and reads nothing.
    pub async fn roster_snapshot(
        &self,
        mark: Option<&hiqlite::AppliedMarkHandle>,
    ) -> Result<RosterSnapshot, MembershipError> {
        match self.inner.as_deref() {
            Some(inner) => {
                let mark = mark.cloned().and_then(RosterMark::of);
                read_roster(inner, mark.as_ref()).await
            }
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

/// Reads answered by a [`RosterDirectory`], by outcome: `hit` from the cached
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

/// The applied mark and the roster tables' four slots, resolved once.
#[derive(Clone)]
struct RosterMark {
    handle: hiqlite::AppliedMarkHandle,
    /// `None` when any roster table has no slot: the stamp then uses the
    /// applied index — coarser (any applied entry ends a hit), never stale.
    slots: Option<[hiqlite::AppliedSlot; 4]>,
}

impl RosterMark {
    fn of(handle: hiqlite::AppliedMarkHandle) -> Option<Self> {
        let slots = ROSTER_TABLES
            .iter()
            .map(|table| handle.slot(table))
            .collect::<Option<Vec<_>>>()
            .and_then(|slots| slots.try_into().ok());
        Some(Self { handle, slots })
    }

    /// The four revisions a stamp compares: one per roster table, or the
    /// applied index in each place when the tables have no slots.
    fn revisions(&self) -> [u64; 4] {
        match self.slots {
            Some(slots) => slots.map(|slot| self.handle.revision(slot)),
            None => [self.handle.applied_index(); 4],
        }
    }

    /// Whether the image and every roster table are still as `stamp` saw
    /// them: atomic loads only. The membership half is checked after this.
    fn tables_match(&self, stamp: &RosterStamp) -> bool {
        let generation = self.handle.generation();
        generation.is_multiple_of(2)
            && generation == stamp.image_generation
            && self.revisions() == stamp.revisions
    }
}

/// The cached roster of one membership manager. Clones share the cache.
#[derive(Clone)]
pub struct RosterDirectory {
    membership: MembershipManager,
    mark: Option<RosterMark>,
    cached: Arc<Mutex<Option<Arc<RosterSnapshot>>>>,
    standalone: Arc<RosterSnapshot>,
    reads: Arc<DirectoryReads>,
}

impl RosterDirectory {
    /// The directory of `membership`, stamped against `mark`, this node's
    /// applied mark (`AppliedView::applied_mark`). Without one it never
    /// caches: every read is a local read, as before the directory.
    #[must_use]
    pub fn new(membership: MembershipManager, mark: Option<hiqlite::AppliedMarkHandle>) -> Self {
        Self {
            mark: mark.and_then(RosterMark::of),
            membership,
            cached: Arc::default(),
            standalone: Arc::new(RosterSnapshot::standalone()),
            reads: Arc::default(),
        }
    }

    /// The cached snapshot if it is still current. The table revisions are
    /// compared first (atomic loads), the membership log only after them; a
    /// node with no applied mark never caches.
    fn current(&self, inner: &ReplicatedMembership) -> Option<Arc<RosterSnapshot>> {
        let mark = self.mark.as_ref()?;
        let cached = self
            .cached
            .lock()
            .expect("roster directory cache")
            .clone()?;
        let stamp = cached.stamp?;
        if !mark.tables_match(&stamp) {
            return None;
        }
        // The vendor watch has no IO-free accessor for the membership log
        // alone, so this builds the (at most 64-entry) member sets.
        let membership = inner.local_metrics.membership_snapshot();
        (membership.committed && membership.membership_log == stamp.membership_log)
            .then_some(cached)
    }

    /// The current applied roster. An unclustered process answers its empty
    /// roster with no read, counted as a hit.
    pub async fn snapshot(&self) -> Result<Arc<RosterSnapshot>, MembershipError> {
        let Some(inner) = self.membership.inner.as_deref() else {
            self.reads.hit.fetch_add(1, Ordering::Relaxed);
            return Ok(Arc::clone(&self.standalone));
        };
        if let Some(cached) = self.current(inner) {
            self.reads.hit.fetch_add(1, Ordering::Relaxed);
            return Ok(cached);
        }
        let snapshot = Arc::new(read_roster(inner, self.mark.as_ref()).await?);
        if snapshot.stamp.is_some() {
            *self.cached.lock().expect("roster directory cache") = Some(Arc::clone(&snapshot));
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
/// uncommitted (or not running) membership, no applied mark, or an odd image
/// generation.
fn roster_stamp(
    inner: &ReplicatedMembership,
    mark: Option<&RosterMark>,
) -> Option<(RosterStamp, hiqlite::LocalDbMembershipSnapshot)> {
    let mark = mark?;
    let membership = inner.local_metrics.membership_snapshot();
    if !membership.committed {
        return None;
    }
    let image_generation = mark.handle.generation();
    if !image_generation.is_multiple_of(2) {
        return None;
    }
    let revisions = mark.revisions();
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
async fn read_roster(
    inner: &ReplicatedMembership,
    mark: Option<&RosterMark>,
) -> Result<RosterSnapshot, MembershipError> {
    let before = roster_stamp(inner, mark);
    let (members, voters) = match &before {
        Some((_, membership)) => (membership.members.clone(), membership.voters.clone()),
        None => {
            let membership = inner.local_metrics.membership_snapshot();
            (membership.members, membership.voters)
        }
    };
    let rows = inner
        .client
        .query_map::<MediaPeerRow, _>(
            "read_roster",
            ROSTER_SQL,
            params!(inner.identity.node_id.as_str()),
        )
        .await?;
    let after = roster_stamp(inner, mark).map(|(stamp, _)| stamp);
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
