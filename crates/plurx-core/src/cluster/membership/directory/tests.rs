//! The directory's purposes against the producers they replace.
//!
//! Each old producer is copied here — its SQL text and its Rust filter — so
//! the comparison survives the producers' deletion in later slices. One row
//! set goes through both, for a matrix of row states, and every purpose
//! must answer exactly as its producer did.

use super::*;

const NOW: i64 = 1_800_000_000_000;
const FRESH: i64 = NOW - 1_000;
const STALE: i64 = NOW - NODE_REACHABLE_WINDOW_MS - 1_000;
const LOCAL: &str = "node-local";

// --- The producers as they were at `dd76bb88e`. -----------------------------

const OLD_ACTIVITY_SQL: &str = "SELECT node.node_id, node.raft_id, node.last_seen_at, \
            http.public_http_url \
     FROM cluster_nodes node \
     LEFT JOIN cluster_node_http http ON http.node_id = node.node_id \
     WHERE node.node_id != $1 AND node.removed_at IS NULL \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
         WHERE removal.node_id = node.node_id) \
     ORDER BY node.raft_id LIMIT $2";

const OLD_MEDIA_SQL: &str = "SELECT node.node_id, node.raft_id, node.last_seen_at, node.role, \
            http.public_http_url, \
            COALESCE(progress.bounded_read_ready, 0) AS bounded_read_ready, \
            progress.observed_at \
     FROM cluster_nodes node \
     LEFT JOIN cluster_node_http http ON http.node_id = node.node_id \
     LEFT JOIN cluster_node_progress progress ON progress.node_id = node.node_id \
     WHERE node.node_id != $1 AND node.removed_at IS NULL \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
         WHERE removal.node_id = node.node_id) \
     ORDER BY node.raft_id LIMIT $2";

const OLD_OPERATIONS_SQL: &str = "SELECT node.node_id, node.raft_id, node.last_seen_at, \
            http.public_http_url \
     FROM cluster_nodes node \
     LEFT JOIN cluster_node_http http ON http.node_id = node.node_id \
     WHERE node.node_id != $1 AND node.removed_at IS NULL \
       AND node.raft_id IN (SELECT CAST(value AS INTEGER) FROM json_each($2)) \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
         WHERE removal.node_id = node.node_id) \
     ORDER BY node.raft_id";

/// `activity_peers`.
fn old_activity_peers(db: &Fixture) -> Vec<ActivityPeer> {
    let voters = db
        .voters
        .iter()
        .copied()
        .take(MAX_ACTIVITY_PEERS.saturating_add(1))
        .collect::<BTreeSet<_>>();
    let rows = db.activity_rows(OLD_ACTIVITY_SQL, Some(MAX_ACTIVITY_PEERS as i64));
    rows.into_iter()
        .filter(|row| voters.contains(&row.raft_id))
        .take(MAX_ACTIVITY_PEERS)
        .map(|row| ActivityPeer {
            http_base: row.http_base,
            node_id: row.node_id,
            raft_id: row.raft_id,
            reachable: node_is_reachable(NOW, row.last_seen_at),
        })
        .collect()
}

/// `activity_voter_count`.
fn old_activity_voter_count(db: &Fixture) -> usize {
    db.voters
        .iter()
        .take(MAX_ACTIVITY_PEERS.saturating_add(2))
        .count()
}

/// `media_peers_for_route` with `media_peer_directory`.
fn old_media_peers(db: &Fixture, allow_learners: bool) -> Result<Vec<ActivityPeer>, String> {
    let members = db
        .members
        .iter()
        .copied()
        .take(MAX_ACTIVITY_PEERS.saturating_add(1))
        .collect::<BTreeSet<_>>();
    let rows = db.media_rows(OLD_MEDIA_SQL, Some(MAX_ACTIVITY_PEERS as i64));
    let mut peers = Vec::new();
    for row in rows.into_iter().filter(|row| {
        members.contains(&row.raft_id)
            && row.bounded_read_ready
            && node_is_reachable(NOW, row.observed_at.unwrap_or_default())
    }) {
        let role =
            ClusterRole::from_stored(row.admitted_role.as_deref()).map_err(|e| e.to_string())?;
        if !allow_learners && role == ClusterRole::Learner {
            continue;
        }
        peers.push(ActivityPeer {
            http_base: row.http_base,
            node_id: row.node_id,
            raft_id: row.raft_id,
            reachable: node_is_reachable(NOW, row.last_seen_at),
        });
        if peers.len() == MAX_ACTIVITY_PEERS {
            break;
        }
    }
    Ok(peers)
}

/// `operations_peers` with `operations_peer_directory`.
fn old_operations_peers(db: &Fixture) -> Result<Vec<ActivityPeer>, String> {
    let members_json = bounded_committed_raft_ids_json(&db.members).map_err(|e| e.to_string())?;
    let rows = db.activity_rows_json(OLD_OPERATIONS_SQL, &members_json);
    Ok(rows
        .into_iter()
        .filter(|row| db.members.contains(&row.raft_id))
        .map(|row| ActivityPeer {
            http_base: row.http_base,
            node_id: row.node_id,
            raft_id: row.raft_id,
            reachable: node_is_reachable(NOW, row.last_seen_at),
        })
        .collect())
}

/// `local_node_is_committed_voter`.
fn old_local_is_voter(db: &Fixture) -> bool {
    lifecycle::committed_voter(db.voters.iter().copied(), db.local_raft_id)
}

// --- The fixture. -----------------------------------------------------------

struct Fixture {
    db: rusqlite::Connection,
    local_raft_id: u64,
    members: BTreeSet<u64>,
    voters: BTreeSet<u64>,
}

/// One node's row state.
#[derive(Clone)]
struct NodeRow {
    raft_id: u64,
    role: Option<&'static str>,
    last_seen_at: i64,
    removed: bool,
    fenced: bool,
    http: bool,
    /// `(bounded_read_ready, observed_at)`, or no progress row.
    progress: Option<(bool, i64)>,
}

impl NodeRow {
    fn ready(raft_id: u64) -> Self {
        Self {
            raft_id,
            role: Some("voter"),
            last_seen_at: FRESH,
            removed: false,
            fenced: false,
            http: true,
            progress: Some((true, FRESH)),
        }
    }
}

fn node_id(raft_id: u64) -> String {
    format!("node-{raft_id:03}")
}

impl Fixture {
    fn new(local_raft_id: u64, members: &[u64], voters: &[u64], rows: &[NodeRow]) -> Self {
        let db = rusqlite::Connection::open_in_memory().expect("roster fixture");
        db.execute_batch(
            "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL UNIQUE, \
                 last_seen_at INTEGER NOT NULL, removed_at INTEGER, role TEXT) STRICT;
             CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, \
                 started_at INTEGER NOT NULL) STRICT;
             CREATE TABLE cluster_node_http (node_id TEXT PRIMARY KEY, \
                 public_http_url TEXT NOT NULL) STRICT;
             CREATE TABLE cluster_node_progress (node_id TEXT PRIMARY KEY, \
                 bounded_read_ready INTEGER NOT NULL, observed_at INTEGER NOT NULL) STRICT;",
        )
        .expect("roster schema");
        let local = NodeRow::ready(local_raft_id);
        for row in rows.iter().chain(std::iter::once(&local)) {
            let id = if row.raft_id == local_raft_id {
                LOCAL.to_owned()
            } else {
                node_id(row.raft_id)
            };
            db.execute(
                "INSERT INTO cluster_nodes VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    id,
                    row.raft_id as i64,
                    row.last_seen_at,
                    row.removed.then_some(NOW),
                    row.role
                ],
            )
            .expect("node row");
            if row.fenced {
                db.execute(
                    "INSERT INTO cluster_node_removals VALUES (?1, ?2)",
                    rusqlite::params![id, NOW],
                )
                .expect("fence row");
            }
            if row.http {
                db.execute(
                    "INSERT INTO cluster_node_http VALUES (?1, ?2)",
                    rusqlite::params![id, format!("http://{id}.invalid")],
                )
                .expect("http row");
            }
            if let Some((ready, observed_at)) = row.progress {
                db.execute(
                    "INSERT INTO cluster_node_progress VALUES (?1, ?2, ?3)",
                    rusqlite::params![id, i64::from(ready), observed_at],
                )
                .expect("progress row");
            }
        }
        Self {
            db,
            local_raft_id,
            members: members.iter().copied().collect(),
            voters: voters.iter().copied().collect(),
        }
    }

    fn media_rows(&self, sql: &str, limit: Option<i64>) -> Vec<MediaPeerRow> {
        let mut statement = self.db.prepare(sql).expect("prepare roster read");
        let map = |row: &rusqlite::Row<'_>| {
            let raft_id: i64 = row.get("raft_id")?;
            Ok(MediaPeerRow {
                node_id: row.get("node_id")?,
                admitted_role: row.get("role")?,
                raft_id: u64::try_from(raft_id).unwrap_or_default(),
                last_seen_at: row.get("last_seen_at")?,
                http_base: row.get("public_http_url")?,
                bounded_read_ready: row.get("bounded_read_ready")?,
                observed_at: row.get("observed_at")?,
            })
        };
        let rows = match limit {
            Some(limit) => statement.query_map(rusqlite::params![LOCAL, limit], map),
            None => statement.query_map(rusqlite::params![LOCAL], map),
        };
        rows.expect("roster read")
            .collect::<Result<_, _>>()
            .expect("roster rows")
    }

    fn activity_rows(&self, sql: &str, limit: Option<i64>) -> Vec<ActivityPeerRow> {
        let mut statement = self.db.prepare(sql).expect("prepare activity read");
        statement
            .query_map(rusqlite::params![LOCAL, limit], activity_row)
            .expect("activity read")
            .collect::<Result<_, _>>()
            .expect("activity rows")
    }

    fn activity_rows_json(&self, sql: &str, members: &str) -> Vec<ActivityPeerRow> {
        let mut statement = self.db.prepare(sql).expect("prepare operations read");
        statement
            .query_map(rusqlite::params![LOCAL, members], activity_row)
            .expect("operations read")
            .collect::<Result<_, _>>()
            .expect("operations rows")
    }

    /// The new read: one statement, no limit, every purpose over it.
    fn snapshot(&self) -> RosterSnapshot {
        RosterSnapshot {
            clustered: true,
            local_raft_id: self.local_raft_id,
            members: self.members.clone(),
            voters: self.voters.clone(),
            rows: self.media_rows(ROSTER_SQL, None),
            stamp: None,
        }
    }
}

fn activity_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityPeerRow> {
    let raft_id: i64 = row.get("raft_id")?;
    Ok(ActivityPeerRow {
        node_id: row.get("node_id")?,
        raft_id: u64::try_from(raft_id).unwrap_or_default(),
        last_seen_at: row.get("last_seen_at")?,
        http_base: row.get("public_http_url")?,
    })
}

/// Every purpose, old and new, on one fixture.
fn assert_equivalent(name: &str, db: &Fixture) {
    let snapshot = db.snapshot();
    assert_eq!(
        snapshot.voters(NOW),
        old_activity_peers(db),
        "{name}: voters"
    );
    assert_eq!(
        snapshot.voter_count(),
        old_activity_voter_count(db),
        "{name}: voter count"
    );
    for allow_learners in [true, false] {
        assert_eq!(
            snapshot
                .media_workers(NOW, allow_learners)
                .map_err(|e| e.to_string()),
            old_media_peers(db, allow_learners),
            "{name}: media workers (allow_learners {allow_learners})"
        );
    }
    assert_eq!(
        snapshot.operations(NOW).map_err(|e| e.to_string()),
        old_operations_peers(db),
        "{name}: operations"
    );
    assert_eq!(
        snapshot.local_is_voter().expect("clustered"),
        old_local_is_voter(db),
        "{name}: local is voter"
    );
}

// --- The matrix. ------------------------------------------------------------

/// {voter, learner, joining, removed, fence row, unready, stale progress,
/// no HTTP row, unreachable, no progress row, legacy role} in one roster.
fn matrix() -> (Vec<u64>, Vec<u64>, Vec<NodeRow>) {
    let rows = vec![
        // voter
        NodeRow::ready(2),
        // learner
        NodeRow {
            role: Some("learner"),
            ..NodeRow::ready(3)
        },
        // joining: a row, not yet a member
        NodeRow::ready(4),
        NodeRow {
            removed: true,
            ..NodeRow::ready(5)
        },
        NodeRow {
            fenced: true,
            ..NodeRow::ready(6)
        },
        // unready
        NodeRow {
            progress: Some((false, FRESH)),
            ..NodeRow::ready(7)
        },
        // stale progress
        NodeRow {
            progress: Some((true, STALE)),
            ..NodeRow::ready(8)
        },
        NodeRow {
            http: false,
            ..NodeRow::ready(9)
        },
        // unreachable
        NodeRow {
            last_seen_at: STALE,
            ..NodeRow::ready(10)
        },
        NodeRow {
            progress: None,
            ..NodeRow::ready(11)
        },
        // legacy row: no stored role
        NodeRow {
            role: None,
            ..NodeRow::ready(12)
        },
        // a learner whose row says learner and Raft says voter
        NodeRow {
            role: Some("learner"),
            ..NodeRow::ready(13)
        },
    ];
    let voters = vec![1, 2, 5, 6, 7, 8, 9, 10, 11, 12, 13];
    let mut members = voters.clone();
    members.push(3);
    (members, voters, rows)
}

#[test]
fn every_purpose_answers_as_its_producer_did_across_the_row_matrix() {
    let (members, voters, rows) = matrix();
    assert_equivalent("matrix", &Fixture::new(1, &members, &voters, &rows));
    // The same roster seen from a learner: local is not a voter.
    let mut members = members;
    members.push(20);
    assert_equivalent(
        "from a learner",
        &Fixture::new(20, &members, &voters, &rows),
    );
    // A node the applied membership does not list.
    assert_equivalent("absent", &Fixture::new(30, &members, &voters, &rows));
}

#[test]
fn every_purpose_keeps_its_producers_caps_beyond_the_limit() {
    // Seventy remote nodes; the first ten learners take the low Raft ids, so
    // the old LIMIT cut voters the voter filter would have kept.
    let rows = (2..=71)
        .map(|raft_id| NodeRow {
            role: Some(if raft_id <= 11 { "learner" } else { "voter" }),
            ..NodeRow::ready(raft_id)
        })
        .collect::<Vec<_>>();
    let members = (1..=60).collect::<Vec<_>>();
    let voters = std::iter::once(1).chain(12..=60).collect::<Vec<_>>();
    let db = Fixture::new(1, &members, &voters, &rows);
    assert_equivalent("over the limit", &db);
    // Over the committed-roster bound: operations fails closed, as it did.
    let members = (1..=71).collect::<Vec<_>>();
    let voters = std::iter::once(1).chain(12..=71).collect::<Vec<_>>();
    let db = Fixture::new(1, &members, &voters, &rows);
    assert!(db.snapshot().operations(NOW).is_err());
    assert_equivalent("over the committed bound", &db);
}

#[test]
fn an_unknown_stored_role_fails_the_media_directory_as_it_did() {
    let (members, voters, mut rows) = matrix();
    rows.push(NodeRow {
        role: Some("observer"),
        ..NodeRow::ready(14)
    });
    let voters = voters.into_iter().chain([14]).collect::<Vec<_>>();
    let members = members.into_iter().chain([14]).collect::<Vec<_>>();
    let db = Fixture::new(1, &members, &voters, &rows);
    assert!(db.snapshot().media_workers(NOW, true).is_err());
    assert_equivalent("unknown role", &db);
}

#[test]
fn reachability_and_progress_age_on_one_snapshot() {
    let (members, voters, rows) = matrix();
    let snapshot = Fixture::new(1, &members, &voters, &rows).snapshot();
    let later = NOW + NODE_REACHABLE_WINDOW_MS + 1;
    assert!(snapshot.voters(NOW).iter().any(|peer| peer.reachable));
    assert!(snapshot.voters(later).iter().all(|peer| !peer.reachable));
    assert!(!snapshot.media_workers(NOW, true).expect("roles").is_empty());
    assert!(snapshot
        .media_workers(later, true)
        .expect("roles")
        .is_empty());
}

#[test]
fn an_unclustered_process_has_no_peers_and_is_the_one_voter() {
    let snapshot = RosterSnapshot::standalone();
    assert!(!snapshot.is_clustered());
    assert!(snapshot.voters(NOW).is_empty());
    assert_eq!(snapshot.voter_count(), 1);
    assert!(snapshot.media_workers(NOW, true).expect("empty").is_empty());
    assert!(snapshot.operations(NOW).expect("empty").is_empty());
    assert!(matches!(
        snapshot.local_is_voter(),
        Err(MembershipError::Unavailable)
    ));
}

#[test]
fn the_roster_read_names_exactly_its_tables() {
    // The production mark registers no roster table yet (see ROSTER_TABLES),
    // so its stamp is the applied index; it must still fit the mark.
    assert!(crate::store::applied_mark_tables().count() <= hiqlite::APPLIED_MARK_MAX_TABLES);
    for table in ROSTER_TABLES {
        assert!(ROSTER_SQL.contains(&format!("{table} ")), "{table}");
    }
}
