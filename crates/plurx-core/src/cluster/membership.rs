//! M3 cluster membership lifecycle.
//!
//! Membership is cluster infrastructure, not a second application store. The
//! Hiqlite client remains the one replicated write path; this coordinator owns
//! only the small amount of state needed to admit nodes, describe them without
//! exposing listener ports or secrets, and remove a voter safely.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[cfg(unix)]
use std::ffi::CStr;
use std::future::Future;
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hiqlite::macros::params;
use hiqlite::{Client, Node, Param, Row};
use hmac::{Hmac, Mac};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cluster::coordination::{removed_job_owner_key, ClusterJobAuthority};
use crate::domain::{OfflinePackage, OfflineRemovalPlanEntry, OfflineRemovalReport};
use crate::store::{
    ArtworkRepairFence, Store, AUTH_LEARNER_PROTOCOL, AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MIN,
    AUTH_SCHEMA_VERSION,
};

use super::migration::status::{ReplicationMonitor, ReplicationStatus};
use super::migration::ActivationMarker;
use super::ClusterIdentity;

/// What a node is called when nothing usable could be derived for it: no
/// reported hostname, no advertised name, and no reverse lookup. It is a
/// sentinel rather than a name, so callers that have a stable identifier of
/// their own are better off showing that instead.
pub const UNKNOWN_HOSTNAME: &str = "unknown-host";

const JOIN_TOKEN_PREFIX: &str = "plxjoin:v1";
const JOIN_TOKEN_AAD: &[u8] = b"plurx-cluster-join-v1";
const JOIN_TOKEN_VERSION: u32 = 1;
/// Learner admission is a *different* protocol, not a flag on the voter one.
/// The prefix, the AEAD associated data, and the version constant are all
/// distinct, so a build that only knows v1 refuses a v2 token at the prefix,
/// again at decryption, and again at the version, instead of reinterpreting
/// the payload as a voter join.
const JOIN_TOKEN_V2_PREFIX: &str = "plxjoin:v2";
const JOIN_TOKEN_V2_AAD: &[u8] = b"plurx-cluster-join-v2";
const JOIN_TOKEN_V2_VERSION: u32 = 2;
const MEMBERSHIP_SCHEMA_VERSION: i64 = 1;
const NODE_REACHABLE_WINDOW_MS: i64 = 30_000;
/// How long a member may say nothing before a protocol change treats it as
/// absent rather than as proven.
///
/// Twelve heartbeats, deliberately much wider than the thirty-second
/// reachability window: reachability is a health signal an operator reads,
/// while this decides whether to commit a one-way narrowing that can lock a
/// node out of its own store. A brief hiccup must not block activation; two
/// minutes of silence is not a hiccup.
const PROTOCOL_CHANGE_ABSENCE_WINDOW_MS: i64 = 120_000;
const ARTWORK_AUTH_WINDOW_MS: i64 = 60_000;
/// The vendored Raft configuration sends heartbeats every 500 ms and cannot
/// elect a successor before 1,500 ms. Requiring an acknowledgement inside two
/// heartbeats makes the old leader ineligible before a new term can exist.
const ARTWORK_LEADER_QUORUM_FRESH_MS: u64 = 1_000;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
/// Collapse only duplicate/concurrent submissions. The ordinary ten-second
/// cadence and thirty-second reachability contract remain unchanged.
const HEARTBEAT_COALESCE_WINDOW: Duration = Duration::from_millis(250);
/// How long a removal waits for survivors to answer a source probe before
/// treating silence as "cannot prove it". Long enough for a healthy node's
/// poll plus a `stat` on a sleeping NAS; short enough that an operator gets an
/// answer rather than a hung request.
const PROBE_WAIT: Duration = Duration::from_secs(15);
const PROBE_POLL: Duration = Duration::from_millis(500);
/// A probe nobody answered stops being worth answering. This bounds how far
/// back a node looks so an abandoned removal cannot leave work queued forever.
const PROBE_REQUEST_TTL: i64 = 300;
/// Matches the activity surface's notion of a transfer in progress: a lease
/// touched this recently is a downloader that is still fetching.
const TRANSFER_ACTIVE_SECONDS: i64 = 60;
/// How many times a removal re-reads and resolves newly created offline work
/// before it refuses instead. Each round costs at most one [`PROBE_WAIT`], so
/// this bounds the operator's wait as well as the loop.
const OFFLINE_RESOLVE_ROUNDS: u32 = 3;
/// Bound the required odd-voter handoff. Survivor probes run in parallel, so
/// one unavailable voter cannot multiply this deadline.
const SURVIVOR_LEADER_WAIT: Duration = Duration::from_secs(8);
/// A committed self-removal must return to the HTTP layer promptly so the
/// daemon can drain. The durable pending-removal row is already authoritative
/// if this best-effort final tombstone write does not finish in time.
const FINAL_TOMBSTONE_WAIT: Duration = Duration::from_secs(1);
const REMOVAL_ATTEMPT_CAPABILITY: &str = "membership_removal_attempt_refs_v1";
/// Proof that the running binary understands the replicated maintenance fence
/// and the bounded election operation. An older process must never be allowed
/// to acknowledge maintenance while it continues admitting work.
const NODE_MAINTENANCE_CAPABILITY: &str = "node_maintenance_v1";
/// Proof that the binary running on this node implements protocol 5, the
/// non-voting learner admission protocol. Written coupled to the heartbeat for
/// the same reason as [`REMOVAL_ATTEMPT_CAPABILITY`]: activation must be
/// decided on what each voter is running *now*, not on what it once ran.
const LEARNER_PROTOCOL_CAPABILITY: &str = "learner_protocol_v5";
/// Proof that every active member understands readiness-gated routing and the
/// promotion/removal intent rows introduced by the complete worker lifecycle.
const LEARNER_LIFECYCLE_CAPABILITY: &str = "learner_lifecycle_v1";
/// Minimum unreserved capacity required before a learner may be promoted.
/// This is deliberately independent of media-cache headroom: a voter must
/// always retain room for Raft WAL growth, a received snapshot, and SQLite's
/// replacement database even when every disposable cache is full.
const MIN_VOTER_STORAGE_HEADROOM_BYTES: u64 = 512 * 1024 * 1024;
/// A promotion barrier waits for the target's own heartbeat to prove that its
/// local state machine applied through the quorum-confirmed barrier index.
const PROMOTION_BARRIER_WAIT: Duration = Duration::from_secs(20);
/// Re-probe durable voter storage at least once inside the readiness window.
/// The fsync work runs on Tokio's blocking pool, never on an async worker.
const STORAGE_DURABILITY_PROBE_INTERVAL: Duration = Duration::from_secs(20);
const STORAGE_DURABILITY_PROBE_MAX_AGE_MS: i64 = NODE_REACHABLE_WINDOW_MS;

/// Whether this process must heartbeat the way a binary that predates the
/// learner protocol does: it advances `cluster_nodes.last_seen_at` like any
/// other member, and it never writes [`LEARNER_PROTOCOL_CAPABILITY`].
///
/// Activation is refused while any active node's currently running binary has
/// not proven that capability, and the only honest way to test that is to run a
/// second process that genuinely does not write it. The separate-process
/// harness starts one with this variable set. `cluster-validation` is a feature
/// the daemon never enables, so a production build compiles the constant
/// version below instead and has no variable to read.
#[cfg(feature = "cluster-validation")]
fn emulate_pre_learner_protocol_heartbeat() -> bool {
    std::env::var("PLURX_VALIDATION_PRE_LEARNER_HEARTBEAT").as_deref() == Ok("1")
}

/// Production builds do not compile the emulation at all.
#[cfg(not(feature = "cluster-validation"))]
const fn emulate_pre_learner_protocol_heartbeat() -> bool {
    false
}

/// What a cluster member was *admitted* as.
///
/// This is the durable admission record, not the effective role. Whether a
/// process may act as a voter right now is decided by live committed Raft
/// membership ([`MembershipManager::local_node_is_committed_voter`]); this
/// enum only says which protocol admitted it and therefore what its own
/// startup is allowed to do. Anything that reads it to decide about leadership
/// or singleton work is a bug — a learner can be promoted while it runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClusterRole {
    /// Carries a vote, may become leader, may run leader-singleton work.
    #[default]
    Voter,
    /// Receives replication and nothing else.
    Learner,
}

/// Effective local HTTP capacity role from committed membership plus the
/// durable removal fence. This is intentionally not the boot/admission role:
/// promotion takes effect without restarting the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalServingRole {
    Unclustered,
    Voter,
    Learner,
    Fenced,
}

impl LocalServingRole {
    const fn encoded(self) -> u8 {
        match self {
            Self::Unclustered => 0,
            Self::Voter => 1,
            Self::Learner => 2,
            Self::Fenced => 3,
        }
    }

    fn from_encoded(value: u8) -> Self {
        match value {
            0 => Self::Unclustered,
            1 => Self::Voter,
            2 => Self::Learner,
            _ => Self::Fenced,
        }
    }
}

/// One-shot publication of the local serving role.
///
/// `cluster_capacity_gate` loads this slot on every request, so a refresh has
/// to publish exactly once: any interim value is a live 503 for as long as the
/// refresh runs. `commit` publishes the computed role. Dropping without a
/// commit — an early `?`, a panic, or a cancelled future — publishes `Fenced`,
/// so failing closed survives every failure path without also fencing the
/// success path.
struct LocalServingRolePublication<'a> {
    slot: &'a AtomicU8,
    committed: bool,
}

impl<'a> LocalServingRolePublication<'a> {
    #[must_use = "an unused publication fences the node for the rest of the refresh"]
    fn new(slot: &'a AtomicU8) -> Self {
        Self {
            slot,
            committed: false,
        }
    }

    fn commit(mut self, role: LocalServingRole) {
        self.slot.store(role.encoded(), Ordering::Release);
        self.committed = true;
    }
}

impl Drop for LocalServingRolePublication<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.slot
                .store(LocalServingRole::Fenced.encoded(), Ordering::Release);
        }
    }
}

/// One-shot publication of the local maintenance flag.
///
/// `cluster_capacity_gate` consults this flag *before* the serving role, and
/// its allow-list is an explicit enumeration rather than `/healthz` plus
/// `/metrics`, so an interim `true` is a broader outage than an interim
/// `Fenced`. Same contract as the serving-role publication: `commit` is the
/// only way to publish an open gate, and any path that does not reach it
/// leaves the node in maintenance.
struct LocalMaintenancePublication<'a> {
    slot: &'a AtomicBool,
    committed: bool,
}

impl<'a> LocalMaintenancePublication<'a> {
    #[must_use = "an unused publication holds the node in maintenance for the rest of the refresh"]
    fn new(slot: &'a AtomicBool) -> Self {
        Self {
            slot,
            committed: false,
        }
    }

    fn commit(mut self, requested: bool) {
        self.slot.store(requested, Ordering::Release);
        self.committed = true;
    }
}

impl Drop for LocalMaintenancePublication<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.slot.store(true, Ordering::Release);
        }
    }
}

impl ClusterRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Voter => "voter",
            Self::Learner => "learner",
        }
    }

    #[must_use]
    pub fn is_learner(self) -> bool {
        matches!(self, Self::Learner)
    }

    /// Read a role back out of replicated SQL.
    ///
    /// The column is additive and nullable, so every row written before this
    /// binary existed carries `NULL` — and every such row belongs to a node
    /// admitted by the voter-only protocol. An unrecognized value is treated
    /// the same way a missing one is only for `NULL`; anything else is a state
    /// this binary does not understand and must not guess about.
    fn from_stored(value: Option<&str>) -> Result<Self, MembershipError> {
        match value {
            None | Some("voter") => Ok(Self::Voter),
            Some("learner") => Ok(Self::Learner),
            Some(other) => Err(MembershipError::Internal(format!(
                "cluster member role {other:?} is not one this binary understands"
            ))),
        }
    }
}

/// Additive column steps applied by [`MembershipManager::initialize`].
///
/// `cluster_nodes` and `cluster_join_tokens` are created by the
/// `CREATE TABLE IF NOT EXISTS` list below, which does nothing to a table that
/// already exists — so a new column needs its own explicit step. Each is one
/// replicated statement and each is additive and nullable, so an older binary
/// in the same cluster keeps writing and reading its six-column rows unchanged
/// and a partially applied list simply resumes on the next boot.
const MEMBERSHIP_ADDITIVE_COLUMNS: &[AdditiveColumn] = &[
    AdditiveColumn {
        table: "cluster_nodes",
        column: "role",
        statement: "ALTER TABLE cluster_nodes ADD COLUMN role TEXT",
    },
    AdditiveColumn {
        table: "cluster_join_tokens",
        column: "role",
        statement: "ALTER TABLE cluster_join_tokens ADD COLUMN role TEXT",
    },
];

/// Refuse a learner credential on the legacy voter redemption path.
///
/// A coordinator from before P6 ignores unknown JSON fields and knows only
/// `/cluster/join/redeem`. The dedicated v2 handler installs a transaction-
/// local intent before changing the token state; an old coordinator cannot,
/// so replicated SQLite rejects its otherwise-valid `issued -> redeeming`
/// update before it can publish the learner as a voter.
const REQUIRE_LEARNER_JOIN_INTENT_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_learner_join_v1_guard \
     BEFORE UPDATE OF state ON cluster_join_tokens \
     WHEN NEW.state = 'redeeming' AND OLD.state = 'issued' \
       AND OLD.role = 'learner' \
       AND NOT EXISTS (SELECT 1 FROM cluster_learner_join_intents intent \
         WHERE intent.token_hash = OLD.token_hash) \
     BEGIN SELECT RAISE(ABORT, 'learner token requires v2 admission'); END";

// These schema guards are deliberately independent of the current
// coordinator SQL. A previous-release binary does not know to predicate its
// lifecycle writes on `cluster_operation_leases`, but every version still
// submits them through this replicated SQLite state machine.
const PROTECT_OPERATION_LEASE_FROM_REMOVAL_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_operation_lease_removal_guard \
     BEFORE INSERT ON cluster_node_removal_attempts \
     WHEN EXISTS (SELECT 1 FROM cluster_operation_leases) \
     BEGIN SELECT RAISE(ABORT, 'planned outage lease blocks membership removal'); END";
const PROTECT_OPERATION_LEASE_FROM_PROMOTION_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_operation_lease_promotion_guard \
     BEFORE INSERT ON cluster_node_promotions \
     WHEN EXISTS (SELECT 1 FROM cluster_operation_leases) \
     BEGIN SELECT RAISE(ABORT, 'planned outage lease blocks learner promotion'); END";
const PROTECT_OPERATION_LEASE_FROM_JOIN_RESERVATION_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_operation_lease_join_reservation_guard \
     BEFORE UPDATE OF state ON cluster_join_tokens \
     WHEN NEW.state = 'redeeming' AND OLD.state = 'issued' \
       AND EXISTS (SELECT 1 FROM cluster_operation_leases) \
     BEGIN SELECT RAISE(ABORT, 'planned outage lease blocks node join'); END";
const PROTECT_OPERATION_LEASE_FROM_JOIN_STAGING_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_operation_lease_join_staging_guard \
     BEFORE INSERT ON cluster_node_join_staging \
     WHEN EXISTS (SELECT 1 FROM cluster_operation_leases) \
     BEGIN SELECT RAISE(ABORT, 'planned outage lease blocks node join'); END";
const EXPIRE_OPERATION_LEASE_FROM_HEARTBEAT_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_operation_lease_heartbeat_expiry \
     BEFORE UPDATE OF last_seen_at ON cluster_nodes \
     WHEN EXISTS (SELECT 1 FROM cluster_operation_leases \
       WHERE expires_at <= NEW.last_seen_at) \
     BEGIN \
       DELETE FROM cluster_operation_leases WHERE expires_at <= NEW.last_seen_at; \
     END";

/// One additive column, named as well as spelled.
///
/// The table and column are carried beside the statement because both the
/// coordinator and a read-only learner have to be able to *ask* whether the
/// column is there, and neither can learn that from the `ALTER` text.
struct AdditiveColumn {
    table: &'static str,
    column: &'static str,
    statement: &'static str,
}

const MEMBERSHIP_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS cluster_membership_meta (\
         singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
         schema_version INTEGER NOT NULL) STRICT",
    "CREATE TABLE IF NOT EXISTS cluster_join_tokens (\
         token_hash TEXT PRIMARY KEY, \
         raft_id INTEGER NOT NULL CHECK (raft_id > 0), \
         expires_at INTEGER NOT NULL, \
         state TEXT NOT NULL CHECK (state IN ('issued', 'redeeming', 'redeemed')), \
         node_id TEXT, \
         created_at INTEGER NOT NULL, \
         redeemed_at INTEGER) STRICT",
    // The row exists only during the v2 coordinator's replicated redemption
    // transaction. The trigger installed after the additive role columns uses
    // it to distinguish that path from an older coordinator's v1 update.
    "CREATE TABLE IF NOT EXISTS cluster_learner_join_intents (\
         token_hash TEXT PRIMARY KEY) STRICT",
    "CREATE TABLE IF NOT EXISTS cluster_nodes (\
         node_id TEXT PRIMARY KEY, \
         raft_id INTEGER NOT NULL UNIQUE CHECK (raft_id > 0), \
         raft_address TEXT NOT NULL, \
         api_address TEXT NOT NULL, \
         last_seen_at INTEGER NOT NULL, \
         removed_at INTEGER) STRICT",
    // A distinct, restart-safe fence for the ambiguous interval between the
    // removal request and a proven uniform Raft configuration. Keeping intent
    // separate from the final tombstone makes interrupted operations visible
    // and idempotently resumable.
    "CREATE TABLE IF NOT EXISTS cluster_node_removals (\
         node_id TEXT PRIMARY KEY, \
         started_at INTEGER NOT NULL) STRICT",
    // Each concurrent removal owns a distinct replicated reference to the
    // shared admission fence. A definite pre-proposal failure releases only
    // its own reference, while an ambiguous attempt deliberately keeps one.
    "CREATE TABLE IF NOT EXISTS cluster_node_removal_attempts (\
         node_id TEXT NOT NULL, \
         attempt_id TEXT NOT NULL, \
         PRIMARY KEY(node_id, attempt_id)) STRICT",
    // Transaction-local proof that a fence insert belongs to the
    // reference-aware coordinator. Replicated triggers use it to reject an
    // older leader before that process can submit a stale absolute voter set.
    "CREATE TABLE IF NOT EXISTS cluster_node_removal_intents (\
         node_id TEXT PRIMARY KEY, \
         attempt_id TEXT NOT NULL) STRICT",
    // Coupled to the ordinary heartbeat in one Raft transaction. Equality with
    // cluster_nodes.last_seen_at proves the currently running binary, rather
    // than a previously upgraded process, understands attempt references.
    "CREATE TABLE IF NOT EXISTS cluster_node_capabilities (\
         node_id TEXT NOT NULL, \
         capability TEXT NOT NULL, \
         last_seen_at INTEGER NOT NULL, \
         PRIMARY KEY(node_id, capability)) STRICT",
    // One node's local Raft/apply and durable-storage proof, sampled before
    // and committed with its ordinary heartbeat. The coordinator never
    // infers learner readiness from its own local metrics.
    "CREATE TABLE IF NOT EXISTS cluster_node_progress (\
         node_id TEXT PRIMARY KEY, \
         current_term INTEGER NOT NULL CHECK (current_term >= 0), \
         last_applied_index INTEGER, \
         quorum_committed_index INTEGER, \
         apply_lag_entries INTEGER, \
         bounded_read_ready INTEGER NOT NULL CHECK (bounded_read_ready IN (0, 1)), \
         voter_storage_ready INTEGER NOT NULL CHECK (voter_storage_ready IN (0, 1)), \
         storage_headroom_bytes INTEGER, \
         storage_probe_observed_at INTEGER, \
         voter_role_persisted INTEGER NOT NULL CHECK (voter_role_persisted IN (0, 1)), \
         observed_at INTEGER NOT NULL) STRICT",
    // A durable, retryable audit record for the interval between the blocking
    // apply barrier and Hiqlite's joint/uniform voter-set transition.
    "CREATE TABLE IF NOT EXISTS cluster_node_promotions (\
         node_id TEXT PRIMARY KEY, \
         attempt_id TEXT NOT NULL, \
         barrier_index INTEGER, \
         started_at INTEGER NOT NULL) STRICT",
    // A reversible, replicated request fence. The target acknowledges only
    // after its local admission atomics have observed this row.
    "CREATE TABLE IF NOT EXISTS cluster_node_maintenance (\
         node_id TEXT PRIMARY KEY, \
         requested_at INTEGER NOT NULL, \
         acknowledged_at INTEGER) STRICT",
    // Restart preparation and maintenance share one replicated slot. A local
    // drain flag is necessary to linearize process admissions but cannot stop
    // another node from preparing concurrently; this lease is the cluster-wide
    // arbitration point and expires without operator cleanup after a crash.
    "CREATE TABLE IF NOT EXISTS cluster_operation_leases (\
         singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
         node_id TEXT NOT NULL, \
         operation TEXT NOT NULL CHECK (operation IN ('restart', 'maintenance')), \
         claim_id TEXT NOT NULL UNIQUE, \
         expires_at INTEGER NOT NULL) STRICT",
    // Transaction-local proof for the maintenance-aware heartbeat. Once a
    // maintenance row exists, the trigger below makes a rollback to a binary
    // that does not understand the fence fail before that process can bind its
    // normal serving surface after startup.
    "CREATE TABLE IF NOT EXISTS cluster_node_maintenance_heartbeat_intents (\
         node_id TEXT PRIMARY KEY, \
         last_seen_at INTEGER NOT NULL) STRICT",
    "CREATE TRIGGER IF NOT EXISTS cluster_node_maintenance_heartbeat_guard \
         BEFORE UPDATE OF last_seen_at ON cluster_nodes \
         WHEN EXISTS (SELECT 1 FROM cluster_node_maintenance maintenance \
                WHERE maintenance.node_id = NEW.node_id) \
           AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance_heartbeat_intents intent \
                WHERE intent.node_id = NEW.node_id \
                  AND intent.last_seen_at = NEW.last_seen_at) \
         BEGIN SELECT RAISE(ABORT, 'node maintenance requires current binary'); END",
    // A transaction-local marker lets replicated triggers distinguish this
    // binary's heartbeat from an older statement without trusting wall-clock
    // uniqueness. Every new heartbeat creates and consumes its marker in one
    // transaction, so no stale intent is externally visible.
    "CREATE TABLE IF NOT EXISTS cluster_node_heartbeat_intents (\
         node_id TEXT PRIMARY KEY, \
         last_seen_at INTEGER NOT NULL) STRICT",
    // Redeem publishes a coordinator-side row before the joining process can
    // heartbeat. Only this pre-heartbeat row is excluded from rollout
    // readiness; the first old or new heartbeat consumes the marker.
    "CREATE TABLE IF NOT EXISTS cluster_node_join_staging (\
         node_id TEXT PRIMARY KEY) STRICT",
    // Kept separate from `cluster_nodes` so this patch is rolling-compatible
    // with M3 binaries that still write the original six-column row. Public
    // HTTP addressing belongs to the node rather than the logical server: it
    // is where another voter can retrieve node-local materialized bytes.
    "CREATE TABLE IF NOT EXISTS cluster_node_http (\
         node_id TEXT PRIMARY KEY, \
         public_http_url TEXT NOT NULL) STRICT",
    // A durable exact-origin claim distinguishes pre-M3d shared-address rows
    // from origins admitted by the node-scoped redemption protocol. Legacy
    // rows may migrate once; an in-flight redemption cannot substitute a new
    // origin, while an established node may later readdress its own endpoint.
    "CREATE TABLE IF NOT EXISTS cluster_node_http_claims (\
         node_id TEXT PRIMARY KEY, \
         public_http_url TEXT NOT NULL) STRICT",
    // Additive so a rolling upgrade can teach old membership rows their
    // machine names without rewriting the cluster_nodes table underneath an
    // older voter. Every new daemon creates this table before its heartbeat.
    "CREATE TABLE IF NOT EXISTS cluster_node_hostnames (\
         node_id TEXT PRIMARY KEY, \
         hostname TEXT NOT NULL) STRICT",
    // Activity HTTP authority is node-specific. Unlike the shared Hiqlite API
    // secret, a removed node's retained private key cannot impersonate a
    // surviving voter. Keys are immutable once published for a node id.
    "CREATE TABLE IF NOT EXISTS cluster_node_activity_keys (\
         node_id TEXT PRIMARY KEY, \
         public_key TEXT NOT NULL) STRICT",
    // Provider/source repair is a replicated side effect. The current Raft
    // leader arbitrates one durable claim per item and term; a successor waits
    // a full local monotonic lease before fencing an abandoned older term.
    "CREATE TABLE IF NOT EXISTS cluster_artwork_repairs (\
         item_id INTEGER PRIMARY KEY, \
         owner_node_id TEXT NOT NULL, \
         leader_term INTEGER NOT NULL, \
         generation INTEGER NOT NULL) STRICT",
    // Replicated schema guards make the outage lease authoritative even while
    // a previous-release coordinator is still active during upgrade/rollback.
    // Its ordinary heartbeat shape also clears an expired lease, so a full
    // rollback cannot leave membership operations permanently blocked.
    EXPIRE_OPERATION_LEASE_FROM_HEARTBEAT_SQL,
    PROTECT_OPERATION_LEASE_FROM_REMOVAL_SQL,
    PROTECT_OPERATION_LEASE_FROM_PROMOTION_SQL,
    PROTECT_OPERATION_LEASE_FROM_JOIN_RESERVATION_SQL,
    PROTECT_OPERATION_LEASE_FROM_JOIN_STAGING_SQL,
    // These triggers make the removed-owner fence authoritative for every
    // client version. A still-running older binary uses lease SQL that does
    // not know about the settings marker, but SQLite evaluates these guards
    // on the replicated state machine before accepting its insert or update.
    "CREATE TRIGGER IF NOT EXISTS job_leases_removed_owner_insert \
         BEFORE INSERT ON job_leases \
         WHEN EXISTS (SELECT 1 FROM settings \
                      WHERE key = 'internal.cluster_job_owner_removed.' || NEW.owner_node_id) \
         BEGIN SELECT RAISE(ABORT, 'cluster job lease owner has been removed'); END",
    "CREATE TRIGGER IF NOT EXISTS job_leases_removed_owner_update \
         BEFORE UPDATE OF owner_node_id, expires_at_ms, revision ON job_leases \
         WHEN EXISTS (SELECT 1 FROM settings \
                      WHERE key = 'internal.cluster_job_owner_removed.' || NEW.owner_node_id) \
         BEGIN SELECT RAISE(ABORT, 'cluster job lease owner has been removed'); END",
    // Older binaries do not know about attempt references and unconditionally
    // delete the two shared fences on a rejected request. Replicated triggers
    // make those legacy DELETEs harmless while any attempt reference exists.
    PROTECT_REMOVAL_FENCE_DELETE_SQL,
    PROTECT_REMOVAL_OWNER_FENCE_DELETE_SQL,
    MARK_LEGACY_NODE_INSERT_DURING_REMOVAL_SQL,
    MARK_LEGACY_NODE_HEARTBEAT_DURING_REMOVAL_SQL,
    MARK_LEGACY_NODE_FINALIZE_DURING_REMOVAL_SQL,
];

const ACTIVITY_AUTH_WINDOW_MS: i64 = 30_000;
const ACTIVITY_AUTH_CONTEXT: &[u8] = b"plurx-internal-activity-v1";
const INTERNAL_PEER_AUTH_CONTEXT: &[u8] = b"plurx-internal-peer-request-v1";
const MAX_ACTIVITY_PEERS: usize = 64;
/// The operations page is one bounded fan-out, not a general cluster crawler.
pub const MAX_OPERATIONS_PEERS: usize = 8;
const MAX_ACTIVITY_AUTH_CHECKS_PER_SECOND: u8 = 2;
const MAX_INTERNAL_AUTH_CHECKS_PER_SECOND: u8 = 128;
// Exact-request proofs are accepted on the public listener. Bound the work
// performed before a signature is known to be genuine so forged envelopes
// cannot fill every executor with body hashing and Ed25519 verification.
const MAX_INTERNAL_PREVERIFY_PER_SECOND: u8 = 64;
const MAX_INTERNAL_READ_PREVERIFY_PER_SECOND: u16 = 1_024;
const MAX_INTERNAL_REPLAYS_PER_PEER: usize = 4_096;
// Five seconds at the admitted 1,024 reads/second, plus one second of margin.
// Read proofs need their own short-lived cache: sharing the 30-second mutation
// cache would either reject valid segment bursts or evict live mutation proofs.
const MAX_INTERNAL_READ_REPLAYS_PER_PEER: usize = 6_144;
const INTERNAL_READ_AUTH_WINDOW_MS: i64 = 5_000;
const INTERNAL_READ_AUTHORITY_TTL: Duration = Duration::from_secs(1);
const MAX_PEER_NODE_ID_BYTES: usize = 256;
const MAX_JOIN_SOCKET_ADDRESS_BYTES: usize = 512;
const MAX_JOIN_HTTP_ORIGIN_BYTES: usize = 2_048;
const MAX_JOIN_HOSTNAME_BYTES: usize = 253;
const INTERNAL_AUTH_NONCE_BYTES: usize = 36;
const ED25519_SIGNATURE_HEX_BYTES: usize = 128;
const MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND: u8 = 4;
/// Keep cluster-wide ownership slightly longer than the target's local drain
/// timer so a second claimant cannot win in the gap between replicated claim
/// commit and local monotonic-fence installation.
const CLUSTER_OPERATION_LEASE_EXPIRY_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum MembershipError {
    #[error("cluster membership is unavailable while this node uses SQLite recovery")]
    Unavailable,
    #[error("join token is invalid")]
    InvalidToken,
    #[error("join token expired before it was redeemed")]
    ExpiredToken,
    #[error("join token has already been redeemed")]
    ReusedToken,
    #[error("join token is already reserved for another node")]
    ReservedToken,
    #[error("joining binary is incompatible with this cluster")]
    Incompatible,
    #[error("cluster HTTP endpoint must be an http(s) origin without credentials or a path")]
    InvalidHttpEndpoint,
    #[error("cluster HTTP endpoint is already owned by another active node")]
    HttpEndpointInUse,
    #[error("cluster node identity is already present in membership history")]
    NodeIdentityInUse,
    #[error("finish upgrading every active cluster node before changing membership")]
    MembershipUpgradeRequired,
    /// Activation was refused because at least one active node is not running
    /// a binary that has proven the learner protocol. Naming them is the point:
    /// "finish upgrading" is not actionable at 3am without the roster.
    #[error(
        "these cluster nodes are not running a binary that supports the learner protocol: {}; \
         upgrade each of them, let one heartbeat interval pass, then activate again",
        .0.join(", ")
    )]
    LearnerProtocolUpgradeRequired(Vec<String>),
    /// A learner was asked for on a cluster that has not activated the
    /// protocol that admits one. Deploying this binary does not activate it;
    /// only the explicit admin operation does.
    #[error(
        "this cluster has not activated the learner protocol, so it cannot admit a non-voting \
         node; activate it from an admin session once every node reports ready, then retry"
    )]
    LearnerProtocolInactive,
    /// Deactivation was refused because removing protocol 5 would strand a
    /// node that only exists under it.
    ///
    /// The two rosters are carried separately because they are two different
    /// claims. `admitted` is the durable role column and names nodes this
    /// cluster admitted as learners. `non_voting` is committed Raft membership
    /// and names members that hold no vote — which, mid-join, includes an
    /// ordinary voter the vendored client has added but not yet promoted.
    /// Collapsing them told an operator that a healthy voter was a learner.
    #[error(
        "deactivating the learner protocol would strand cluster members{}{}; remove them first, \
         then deactivate",
        crate::cluster::membership::roster_clause(" admitted as learners: ", .admitted),
        crate::cluster::membership::roster_clause(
            " that committed membership currently lists as non-voting (a voter that is still \
             mid-join appears here and settles on its own): ",
            .non_voting
        )
    )]
    LearnerProtocolInUse {
        admitted: Vec<String>,
        non_voting: Vec<String>,
    },
    /// Activation was refused because a node is between redeeming its join
    /// token and proving anything about the binary it runs. Such a node is
    /// deliberately invisible to the capability roster — it has not
    /// heartbeated yet — so without this it would be activated straight past
    /// and could never open its store again while still counting for quorum.
    #[error(
        "these cluster nodes are mid-join and have not yet proven what binary they run: {}; \
         wait for the join to finish, or remove the node if it is not coming back, then activate",
        .0.join(", ")
    )]
    JoinInFlight(Vec<String>),
    /// Activation was refused because a node that is still a member has said
    /// nothing for long enough that no claim about its running binary can be
    /// current. A wall-clock window is a weak proof of presence and a good
    /// proof of absence, which is all this needs.
    #[error(
        "these cluster members have not heartbeated in over {minutes} minutes, so activation \
         cannot tell what binary they would come back running and could lock them out: {}; \
         start them or remove them, then activate",
        .nodes.join(", ")
    )]
    LearnerProtocolNodeAbsent { nodes: Vec<String>, minutes: i64 },
    /// A guarded protocol change lost its compare-and-swap and the re-read
    /// roster came back empty, so nothing can be named. Saying "upgrade these
    /// nodes: []" is worse than saying what actually happened.
    #[error(
        "the cluster's protocol range changed while this operation was committing; re-read the \
         cluster status and retry"
    )]
    ProtocolRangeChanged,
    /// The write is committed by the leader and the client routes it there, so
    /// this is the state an operator can actually act on: right now there is no
    /// leader to route to.
    #[error("the cluster has no elected leader to commit this operation; retry after an election")]
    LeaderUnavailable,
    #[error(
        "node removal remains pending after the membership change was rejected: {0}; finish upgrading every cluster node and retry this removal"
    )]
    RemovalPending(String),
    #[error("node was not found in current cluster membership")]
    NodeNotFound,
    /// The target is a committed member that carries no vote — a learner, or a
    /// voter Raft has added but not yet promoted. The roster lists it, so
    /// "not found" would be a lie; this release simply has no removal path for
    /// it yet.
    #[error(
        "cluster member {0} carries no vote, and this release can only remove voters; it has to \
         be shut down and its data directory discarded"
    )]
    NonVoterRemovalUnsupported(String),
    #[error("node {0} is not a non-voting learner that can be promoted")]
    PromotionRequiresLearner(String),
    #[error(
        "learner {0} has not published a fresh zero-lag quorum/apply proof; wait for catch-up and retry"
    )]
    LearnerNotReady(String),
    #[error(
        "learner {node_id} failed voter storage preflight: at least {required_bytes} bytes of durable headroom are required"
    )]
    VoterStoragePreflight {
        node_id: String,
        required_bytes: u64,
    },
    #[error("learner lifecycle operation for {0} is already pending; retry the same operation")]
    LearnerLifecyclePending(String),
    #[error("the current Raft leader cannot be removed; retry after leadership moves")]
    LeaderRemoval,
    #[error("the local voter must use the graceful leave operation")]
    SelfRemovalRequiresLeave,
    #[error("the leave request targeted a different local cluster node")]
    LeaveNodeMismatch,
    #[error("the local node is no longer an active cluster voter")]
    LocalNodeNotActive,
    #[error("removal would leave fewer than two voters and lose the reconfiguration quorum")]
    QuorumLoss,
    #[error("node maintenance operation for {0} conflicts with another cluster lifecycle change")]
    MaintenanceConflict(String),
    #[error("another node already owns the cluster's planned-outage lease")]
    ClusterOperationPending,
    #[error(
        "putting voter {0} into maintenance would leave fewer reachable voters than the cluster quorum during its restart"
    )]
    MaintenanceWouldLoseQuorum(String),
    #[error("node {0} must be reachable and caught up before maintenance can be cleared")]
    MaintenanceResumeUnsafe(String),
    #[error("the cluster cannot force an election without a reachable voter quorum")]
    ElectionQuorumUnavailable,
    #[error(
        "the cluster has no healthy caught-up follower that can safely campaign for leadership"
    )]
    ElectionCandidateUnavailable,
    #[error("force election is unavailable while a node lifecycle operation is pending")]
    ElectionLifecyclePending,
    #[error("node owns active media sessions that must drain before removal")]
    ActiveMediaSessions,
    /// The removal was refused because this node's offline work could not be
    /// resolved by the §6.7 rule. The payload is the operator-visible reason;
    /// lifting the blanket refusal must not turn removal into "always
    /// succeeds", so what stopped it has to be sayable.
    #[error("node owns offline work that could not be resolved: {0}")]
    OfflineWork(String),
    /// Preserve Hiqlite's typed routing verdict so a caller can distinguish a
    /// read-only operation that lost its leader from a semantic membership
    /// refusal. The public error code and message remain backward-compatible.
    #[error("cluster membership operation failed: LeaderChange: {0}")]
    LeaderChanged(String),
    #[error("cluster membership operation failed: {0}")]
    Internal(String),
}

/// Render one labelled roster into a refusal, or nothing when it is empty.
///
/// Two rosters that answer different questions have to stay labelled in the
/// message an operator reads, and an empty one must not leave a dangling
/// label behind.
#[doc(hidden)]
#[must_use]
pub fn roster_clause(label: &str, nodes: &[String]) -> String {
    if nodes.is_empty() {
        String::new()
    } else {
        format!("{label}{}", nodes.join(", "))
    }
}

impl MembershipError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "membership_unavailable",
            Self::InvalidToken => "join_token_invalid",
            Self::ExpiredToken => "join_token_expired",
            Self::ReusedToken => "join_token_reused",
            Self::ReservedToken => "join_token_reserved",
            Self::Incompatible => "join_incompatible",
            Self::InvalidHttpEndpoint => "cluster_http_endpoint_invalid",
            Self::HttpEndpointInUse => "cluster_http_endpoint_in_use",
            Self::NodeIdentityInUse => "cluster_node_identity_in_use",
            Self::MembershipUpgradeRequired => "membership_upgrade_required",
            Self::LearnerProtocolUpgradeRequired(_) => "learner_protocol_upgrade_required",
            Self::LearnerProtocolInactive => "learner_protocol_inactive",
            Self::LearnerProtocolInUse { .. } => "learner_protocol_in_use",
            Self::JoinInFlight(_) => "join_in_flight",
            Self::LearnerProtocolNodeAbsent { .. } => "learner_protocol_node_absent",
            Self::ProtocolRangeChanged => "cluster_protocol_range_changed",
            Self::LeaderUnavailable => "cluster_leader_unavailable",
            Self::RemovalPending(_) => "membership_removal_pending",
            Self::NodeNotFound => "cluster_node_not_found",
            Self::NonVoterRemovalUnsupported(_) => "cluster_non_voter_removal_unsupported",
            Self::PromotionRequiresLearner(_) => "promotion_requires_learner",
            Self::LearnerNotReady(_) => "learner_not_ready",
            Self::VoterStoragePreflight { .. } => "voter_storage_preflight_failed",
            Self::LearnerLifecyclePending(_) => "learner_lifecycle_pending",
            Self::LeaderRemoval => "cluster_leader_removal_refused",
            Self::SelfRemovalRequiresLeave => "self_removal_requires_leave",
            Self::LeaveNodeMismatch => "leave_node_mismatch",
            Self::LocalNodeNotActive => "local_node_not_active",
            Self::QuorumLoss => "removal_would_lose_quorum",
            Self::MaintenanceConflict(_) => "maintenance_conflict",
            Self::ClusterOperationPending => "cluster_operation_pending",
            Self::MaintenanceWouldLoseQuorum(_) => "maintenance_would_lose_quorum",
            Self::MaintenanceResumeUnsafe(_) => "maintenance_resume_unsafe",
            Self::ElectionQuorumUnavailable => "election_quorum_unavailable",
            Self::ElectionCandidateUnavailable => "election_candidate_unavailable",
            Self::ElectionLifecyclePending => "election_lifecycle_pending",
            Self::ActiveMediaSessions => "media_sessions_active",
            Self::OfflineWork(_) => "node_owns_offline_work",
            Self::LeaderChanged(_) | Self::Internal(_) => "membership_internal",
        }
    }
}

impl From<hiqlite::Error> for MembershipError {
    fn from(error: hiqlite::Error) -> Self {
        match error {
            hiqlite::Error::LeaderChange(message) => Self::LeaderChanged(message.into_owned()),
            error => Self::Internal(error.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl ClusterJobAuthority for MembershipManager {
    async fn may_run_cluster_jobs(&self) -> bool {
        MembershipManager::may_run_cluster_jobs(self).await
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterPeer {
    pub raft_id: u64,
    pub raft_address: String,
    pub api_address: String,
}

impl From<&Node> for ClusterPeer {
    fn from(node: &Node) -> Self {
        Self {
            raft_id: node.id,
            raft_address: node.addr_raft.clone(),
            api_address: node.addr_api.clone(),
        }
    }
}

impl From<&ClusterPeer> for Node {
    fn from(peer: &ClusterPeer) -> Self {
        Self {
            id: peer.raft_id,
            addr_raft: peer.raft_address.clone(),
            addr_api: peer.api_address.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalMembership {
    pub version: u32,
    pub cluster_id: String,
    pub node_id: String,
    pub raft_id: u64,
    pub local: ClusterPeer,
    pub bootstrap: Vec<ClusterPeer>,
    /// Digest of the one-time token that admitted this node. Initial voters
    /// have no digest. This is the local gate that keeps an unrelated token
    /// file from turning an otherwise healthy restart into a failed join.
    #[serde(default)]
    pub join_token_digest: Option<String>,
    /// What this node was admitted as, so a restart resumes the same role
    /// rather than re-deriving it from a token that has since been consumed.
    ///
    /// Absent in a version 1 record, which is why a voter keeps writing
    /// version 1: an operator who rolls a *voter* back to the previous release
    /// must find a file that release can still read. A learner writes version
    /// 2 and the previous release refuses it, which is the correct outcome —
    /// that build has no idea it must not campaign.
    #[serde(default)]
    pub role: ClusterRole,
}

/// The `membership.json` version a node of this role writes.
///
/// Two versions coexist deliberately. See [`LocalMembership::role`].
#[must_use]
pub fn local_membership_version(role: ClusterRole) -> u32 {
    match role {
        ClusterRole::Voter => 1,
        ClusterRole::Learner => 2,
    }
}

/// Whether a decoded `membership.json` record is internally consistent.
///
/// A version 1 record cannot describe a learner: that combination is only
/// reachable by editing the file, and reading it as a voter would start a
/// campaigning process on a node the cluster admitted as a learner.
#[must_use]
pub fn local_membership_version_matches_role(version: u32, role: ClusterRole) -> bool {
    version == local_membership_version(role)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemJoinRequest {
    /// SHA-256 of the bearer held by the joining node. The coordinator needs
    /// only proof of possession; sending the self-contained token would put
    /// every cluster secret on the public HTTP wire.
    pub token_digest: String,
    pub raft_id: u64,
    pub node_id: String,
    /// The joining machine's OS hostname, reduced to its first DNS label.
    /// Defaulting keeps an interrupted request from an older binary resumable;
    /// its first current heartbeat fills the additive hostname table.
    #[serde(default)]
    pub hostname: String,
    pub raft_address: String,
    pub api_address: String,
    /// Explicitly advertised plurxd endpoint. This remains cluster-internal;
    /// public membership projections intentionally omit it.
    #[serde(default)]
    pub http_base: String,
    pub schema_version: i64,
    /// The oldest protocol the joining binary implements, and the field a
    /// coordinator that predates the range compares for exact equality.
    pub protocol_version: i64,
    /// The joining binary's supported protocol range. A joiner that predates
    /// P6 omits both, and [`RedeemJoinRequest::declared_protocol_range`] then
    /// reads its single scalar as the one-element range it actually is.
    #[serde(default)]
    pub protocol_min: i64,
    #[serde(default)]
    pub protocol_max: i64,
}

impl RedeemJoinRequest {
    /// The inclusive protocol range this joiner claims to implement.
    #[must_use]
    pub fn declared_protocol_range(&self) -> (i64, i64) {
        if self.protocol_min > 0 && self.protocol_max >= self.protocol_min {
            (self.protocol_min, self.protocol_max)
        } else {
            (self.protocol_version, self.protocol_version)
        }
    }
}

/// Reject untrusted redemption fields before they can cause any replicated
/// read. The public route is intentionally unauthenticated because possession
/// of the token is its credential; malformed JSON must not become a cheap way
/// to force leader/quorum traffic.
fn validate_redeem_join_request(
    request: &RedeemJoinRequest,
) -> Result<Option<String>, MembershipError> {
    let invalid_identity = !is_join_token_digest(&request.token_digest)
        || request.raft_id == 0
        || request.node_id.is_empty()
        || request.node_id.len() > MAX_PEER_NODE_ID_BYTES
        || request.node_id.chars().any(char::is_control)
        || request.hostname.len() > MAX_JOIN_HOSTNAME_BYTES
        || request.hostname.chars().any(char::is_control)
        || !is_bounded_join_socket_address(&request.raft_address)
        || !is_bounded_join_socket_address(&request.api_address);
    if invalid_identity {
        return Err(MembershipError::InvalidToken);
    }
    let (protocol_min, protocol_max) = request.declared_protocol_range();
    if protocol_min <= 0 || protocol_max < protocol_min {
        return Err(MembershipError::Incompatible);
    }
    if request.http_base.is_empty() {
        return Ok(None);
    }
    if request.http_base.len() > MAX_JOIN_HTTP_ORIGIN_BYTES
        || request.http_base.chars().any(char::is_control)
    {
        return Err(MembershipError::InvalidHttpEndpoint);
    }
    normalize_internal_http_base(&request.http_base)
        .map(Some)
        .ok_or(MembershipError::InvalidHttpEndpoint)
}

fn is_bounded_join_socket_address(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_JOIN_SOCKET_ADDRESS_BYTES
        || value.chars().any(char::is_control)
    {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(&format!("http://{value}")) else {
        return false;
    };
    url.host_str().is_some()
        && url.port().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/"
}

impl std::fmt::Debug for RedeemJoinRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedeemJoinRequest")
            .field("token_digest", &"<redacted>")
            .field("raft_id", &self.raft_id)
            .field("node_id", &self.node_id)
            .field("hostname", &self.hostname)
            .field("raft_address", &self.raft_address)
            .field("api_address", &self.api_address)
            .field("schema_version", &self.schema_version)
            .field("protocol_version", &self.protocol_version)
            .field("protocol_range", &self.declared_protocol_range())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalizeJoinRequest {
    pub token_digest: String,
    pub raft_id: u64,
    pub node_id: String,
}

impl std::fmt::Debug for FinalizeJoinRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FinalizeJoinRequest")
            .field("token_digest", &"<redacted>")
            .field("raft_id", &self.raft_id)
            .field("node_id", &self.node_id)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedJoinToken {
    pub token: String,
    pub expires_at: i64,
    pub raft_id: u64,
}

impl std::fmt::Debug for IssuedJoinToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssuedJoinToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("raft_id", &self.raft_id)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Voter,
    Learner,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterNodeRecord {
    pub node_id: String,
    /// Actual short machine hostname reported by the node itself.
    pub hostname: String,
    /// Host portion of the advertised cluster API address. Listener ports are
    /// still private; loopback is rendered as `localhost`, not a raw IP.
    pub advertised_host: String,
    pub raft_id: u64,
    /// Durable admission role. A voter keeps this role while Raft temporarily
    /// carries it as a non-voting learner during catch-up.
    pub role: NodeRole,
    /// Whether committed Raft membership currently grants this node a vote.
    /// Kept separate from `role` so a joining voter is not mislabeled as a
    /// permanent learner.
    #[serde(default)]
    pub is_voter: bool,
    pub is_leader: bool,
    pub reachable: bool,
    pub last_seen_at: i64,
    /// A durable removal fence still owns this node. The voter can remain in
    /// Raft membership after a rejected or indeterminate request, so expose
    /// the fence instead of rendering the node as fully operational.
    pub removal_pending: bool,
    /// The binary this node is running *now* has proven the learner protocol,
    /// by writing its capability row inside the same Raft transaction as its
    /// current heartbeat. False here is exactly what blocks activation.
    #[serde(default)]
    pub learner_protocol_ready: bool,
    /// Fresh local quorum/apply proof published by this node. A learner is a
    /// read worker only while this is true; losing the proof removes it from
    /// readiness and placement without changing voter redundancy.
    #[serde(default)]
    pub bounded_read_ready: bool,
    /// Quorum-commit to local-apply gap from this node's own heartbeat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_lag_entries: Option<u64>,
    /// Current unreserved bytes on the authoritative voter filesystem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_headroom_bytes: Option<u64>,
    /// Durable-write probe plus the voter headroom threshold. Promotion
    /// refuses unless this node reports true in a fresh heartbeat.
    #[serde(default)]
    pub voter_storage_ready: bool,
    /// A replicated request is keeping this node out of new work while its
    /// existing node-local sessions drain.
    #[serde(default)]
    pub maintenance: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintenance_requested_at: Option<i64>,
    /// The target process has observed the fence in local applied state.
    #[serde(default)]
    pub maintenance_acknowledged: bool,
    /// Raft-facing preconditions are satisfied. Media-session drain remains a
    /// separate preflight exposed by the cluster media directory.
    #[serde(default)]
    pub maintenance_ready: bool,
    /// Live replicated leases owned by this node. Maintenance waits for this
    /// to reach zero; it never terminates a household stream implicitly.
    #[serde(default)]
    pub active_media_sessions: usize,
}

/// Capacity and quorum are deliberately separate arithmetic. A replicated
/// learner adds one recoverable copy and possibly one ready read/media worker;
/// it does not increase the number of failures the voter set tolerates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterCapacityStatus {
    pub voting_nodes: usize,
    pub voting_quorum: usize,
    pub voting_failure_tolerance: usize,
    pub non_voting_replicas: usize,
    pub ready_read_workers: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterRecoveryStatus {
    /// The roster cannot prove a leader backed by a reachable voter quorum.
    /// This signal never authorizes a consensus-bypassing mutation.
    pub required: bool,
    pub quorum_available: bool,
    pub reachable_voters: usize,
    pub required_voters: usize,
    pub leader_elected: bool,
    pub permanent_majority_loss_supported: bool,
}

/// What protocol range the cluster is on, what this binary can do, and — when
/// the two disagree — which nodes are the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterProtocolStatus {
    /// The range the cluster's replicated features actually require. A node
    /// may participate only if it implements all of it.
    pub active_min: i64,
    pub active_max: i64,
    /// The range this binary implements.
    pub binary_min: i64,
    pub binary_max: i64,
    /// The active range is the learner protocol.
    pub learner_protocol_active: bool,
    /// Active, non-staged nodes whose currently running binary has not proven
    /// the learner protocol. Activation is refused while this is non-empty.
    pub learner_protocol_pending: Vec<String>,
}

/// The outcome of an activation or deactivation request.
///
/// `changed` is what separates "this call moved the cluster" from "the cluster
/// was already there"; both are successes, because these operations are
/// idempotent and an operator retrying after a timeout must not be told the
/// cluster is broken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolChange {
    pub changed: bool,
    pub protocol: ClusterProtocolStatus,
}

/// Internal addressing paired with the privacy-safe health projection.
/// This type is never serialized by the public membership API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityPeer {
    pub node_id: String,
    pub http_base: Option<String>,
    pub reachable: bool,
}

/// Route-scoped proof that one live voter requested another voter's activity
/// projection. Naming both ends prevents a captured proof from being replayed
/// against every daemon in the cluster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPeerAuth {
    pub node_id: String,
    pub target_node_id: String,
    pub timestamp_ms: i64,
    pub signature: String,
}

/// Short-lived proof over an exact internal HTTP request.
///
/// The signed message binds method, normalized route, and the SHA-256 of the
/// raw body as well as both node identities. Household credentials never
/// authorize this envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalPeerAuth {
    pub node_id: String,
    pub target_node_id: String,
    pub timestamp_ms: i64,
    pub nonce: String,
    pub signature: String,
}

/// Process-local Ed25519 authority for the cluster activity HTTP route.
///
/// The seed is persisted as an owner-only file by the migration coordinator;
/// only the public half is replicated. Intentionally not `Clone` or `Debug`.
pub struct ActivitySigningKey {
    key_pair: Ed25519KeyPair,
}

impl ActivitySigningKey {
    pub fn from_seed_hex(seed: &str) -> Result<Self, MembershipError> {
        let seed = hex::decode(seed).map_err(|_| {
            MembershipError::Internal("activity signing seed is not hexadecimal".to_owned())
        })?;
        if seed.len() != 32 {
            return Err(MembershipError::Internal(
                "activity signing seed must be exactly 32 bytes".to_owned(),
            ));
        }
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|_| {
            MembershipError::Internal("activity signing seed is invalid".to_owned())
        })?;
        Ok(Self { key_pair })
    }

    fn public_key_hex(&self) -> String {
        hex::encode(self.key_pair.public_key().as_ref())
    }

    fn sign_hex(&self, message: &[u8]) -> String {
        hex::encode(self.key_pair.sign(message).as_ref())
    }
}

/// Normalize one cluster-internal HTTP origin. Stored endpoints are treated
/// as origins, never as arbitrary URLs: path, query, fragment, and userinfo
/// would make route joining ambiguous or expose authority to another origin.
#[must_use]
pub fn normalize_internal_http_base(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    url.set_path("");
    Some(url.as_str().trim_end_matches('/').to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterAvailability {
    SingleNode,
    DegradedReconfiguration,
    HighAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipStatus {
    /// The node that produced this roster. Destructive local actions bind to
    /// this value so a load balancer cannot move a confirmed leave to a
    /// different backend.
    pub local_node_id: String,
    pub availability: ClusterAvailability,
    pub nodes: Vec<ClusterNodeRecord>,
    /// The one canonical lag answer introduced by #233.
    pub replication: ReplicationStatus,
    /// Explicit capacity-versus-redundancy arithmetic for clients that should
    /// not infer either quantity from the length of `nodes`.
    pub capacity: ClusterCapacityStatus,
    /// The cluster's active protocol range and this binary's support for it.
    pub protocol: ClusterProtocolStatus,
    pub recovery: ClusterRecoveryStatus,
}

/// Fixed-cardinality, process-local projection for Prometheus scrapes.
///
/// The handle intentionally cannot reach the Hiqlite client or application
/// store. A background membership sample populates it so `/metrics` never
/// turns an observability request into a cluster read.
#[derive(Clone, Default)]
pub struct PassiveMembershipMetrics {
    inner: Option<Arc<MembershipMetricsCache>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassiveMembershipMetricsView {
    pub replicated: bool,
    pub valid: bool,
    pub age_seconds: Option<u64>,
    pub errors: u64,
    pub sample: Option<MembershipMetricsSample>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipMetricsSample {
    pub voters: u64,
    pub learners: u64,
    pub heartbeat_fresh_voters: u64,
    pub heartbeat_stale_voters: u64,
    pub removals_pending: u64,
    pub local_is_voter: bool,
}

#[derive(Default)]
struct MembershipMetricsCache {
    state: Mutex<MembershipMetricsCacheState>,
}

#[derive(Default)]
struct MembershipMetricsCacheState {
    sample: Option<(Instant, MembershipMetricsSample)>,
    errors: u64,
}

impl PassiveMembershipMetrics {
    fn replicated() -> Self {
        Self {
            inner: Some(Arc::new(MembershipMetricsCache::default())),
        }
    }

    fn record(&self, sample: MembershipMetricsSample) {
        if let Some(inner) = self.inner.as_deref() {
            inner
                .state
                .lock()
                .expect("membership metrics cache poisoned")
                .sample = Some((Instant::now(), sample));
        }
    }

    fn record_error(&self) {
        if let Some(inner) = self.inner.as_deref() {
            let mut state = inner
                .state
                .lock()
                .expect("membership metrics cache poisoned");
            state.errors = state.errors.saturating_add(1);
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> PassiveMembershipMetricsView {
        let Some(inner) = self.inner.as_deref() else {
            return PassiveMembershipMetricsView {
                replicated: false,
                valid: false,
                age_seconds: None,
                errors: 0,
                sample: None,
            };
        };
        let state = inner
            .state
            .lock()
            .expect("membership metrics cache poisoned");
        let age = state
            .sample
            .map(|(sampled_at, _)| sampled_at.elapsed().as_secs());
        PassiveMembershipMetricsView {
            replicated: true,
            valid: age.is_some_and(|age| age <= NODE_REACHABLE_WINDOW_MS as u64 / 1_000),
            age_seconds: age,
            errors: state.errors,
            sample: state.sample.map(|(_, sample)| sample),
        }
    }
}

#[derive(Clone)]
pub struct MembershipManager {
    inner: Option<Arc<ReplicatedMembership>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaderSelfLeaveSequence {
    HandoffThenCommit,
    CommitDirectly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemovalRollbackOutcome {
    RolledBack,
    Pending,
}

const ROLLBACK_REMOVAL_OWNER_FENCE_SQL: &str = "DELETE FROM settings WHERE key = $1 \
     AND NOT EXISTS (SELECT 1 FROM cluster_node_removal_attempts WHERE node_id = $2)";
const ROLLBACK_REMOVAL_FENCE_SQL: &str = "DELETE FROM cluster_node_removals WHERE node_id = $1 \
     AND NOT EXISTS (SELECT 1 FROM cluster_node_removal_attempts WHERE node_id = $1)";
const BEGIN_REMOVAL_FENCE_SQL: &str = "INSERT INTO cluster_node_removals (node_id, started_at) \
     SELECT $1, $2 WHERE EXISTS (\
       SELECT 1 FROM cluster_node_removal_attempts \
       WHERE node_id = $1 AND attempt_id = $3) \
     ON CONFLICT(node_id) DO NOTHING";
const BEGIN_REMOVAL_INTENT_SQL: &str = "INSERT INTO cluster_node_removal_intents \
     (node_id, attempt_id) SELECT $1, $2 WHERE EXISTS (\
       SELECT 1 FROM cluster_node_removal_attempts \
       WHERE node_id = $1 AND attempt_id = $2) \
     ON CONFLICT(node_id) DO UPDATE SET attempt_id = excluded.attempt_id";
const CLEAR_REMOVAL_INTENT_SQL: &str = "DELETE FROM cluster_node_removal_intents \
     WHERE node_id = $1 AND attempt_id = $2";
const BEGIN_REMOVAL_OWNER_FENCE_SQL: &str = "INSERT INTO settings (key, value, updated_at) \
     SELECT $1, '1', $2 WHERE EXISTS (\
       SELECT 1 FROM cluster_node_removal_attempts \
       WHERE node_id = $3 AND attempt_id = $4) \
     ON CONFLICT(key) DO UPDATE SET value = '1', updated_at = excluded.updated_at";
const BEGIN_REMOVAL_JOB_FENCE_SQL: &str = "UPDATE job_leases SET \
       owner_node_id = 'internal.removed-job-owner:' || owner_node_id, \
       expires_at_ms = CASE WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END, \
       revision = CASE WHEN revision < 9223372036854775807 THEN revision + 1 ELSE revision END, \
       updated_at_ms = $1 \
     WHERE owner_node_id = $2 AND EXISTS (\
       SELECT 1 FROM cluster_node_removal_attempts \
       WHERE node_id = $2 AND attempt_id = $3)";
const BEGIN_REMOVAL_MEDIA_FENCE_SQL: &str = "UPDATE media_sessions SET \
       state = 'ended', terminal_reason = COALESCE(terminal_reason, 'replaced'), \
       lease_expires_at_ms = $1, publication_ready_at_ms = $2, updated_at_ms = $1 \
     WHERE owner_node_id = $3 AND state = 'active' AND EXISTS (\
       SELECT 1 FROM cluster_node_removal_attempts \
       WHERE node_id = $3 AND attempt_id = $4)";
const PROTECT_REMOVAL_FENCE_DELETE_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_node_removal_attempt_delete_guard \
     BEFORE DELETE ON cluster_node_removals \
     WHEN EXISTS (SELECT 1 FROM cluster_node_removal_attempts \
                  WHERE node_id = OLD.node_id) \
     BEGIN SELECT RAISE(IGNORE); END";
const PROTECT_REMOVAL_OWNER_FENCE_DELETE_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_node_removal_owner_delete_guard \
     BEFORE DELETE ON settings \
     WHEN EXISTS (SELECT 1 FROM cluster_node_removal_attempts \
                  WHERE OLD.key = 'internal.cluster_job_owner_removed.' || node_id) \
     BEGIN SELECT RAISE(IGNORE); END";
const BACKFILL_REMOVAL_ATTEMPT_REFS_SQL: &str =
    "INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) \
     SELECT node_id, 'internal.preexisting' FROM cluster_node_removals \
     WHERE NOT EXISTS (SELECT 1 FROM cluster_node_removal_attempts AS attempt \
       WHERE attempt.node_id = cluster_node_removals.node_id) \
     ON CONFLICT(node_id, attempt_id) DO NOTHING";
const REQUIRE_REMOVAL_INTENT_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_node_removal_insert_guard \
     BEFORE INSERT ON cluster_node_removals \
     WHEN NOT EXISTS (SELECT 1 FROM cluster_node_removal_intents AS intent \
       JOIN cluster_node_removal_attempts AS attempt \
         ON attempt.node_id = intent.node_id AND attempt.attempt_id = intent.attempt_id \
       WHERE intent.node_id = NEW.node_id) \
     BEGIN SELECT RAISE(ABORT, 'cluster membership removal requires current coordinator'); END";

/// One active node that has *not* proven `capability` with its currently
/// running binary.
///
/// The proof is the equality below: a capability row is written inside the same
/// Raft transaction as the ordinary heartbeat, so `last_seen_at` matching the
/// node's `cluster_nodes.last_seen_at` is what distinguishes the binary running
/// right now from one that was installed, observed, and then rolled back. This
/// is the cluster's only capability *staleness* rule; the separate liveness
/// and join-in-flight rules below answer different questions and deliberately
/// do not touch this one.
///
/// The staging exclusion here keeps the pending *roster* honest: a node that
/// is mid-join has not heartbeated yet and is not "behind". It is not, on its
/// own, permission to activate — see [`no_join_in_flight_predicate`].
fn capability_unready_node_predicate(capability: &str) -> String {
    format!(
        "active.removed_at IS NULL \
         AND NOT EXISTS (SELECT 1 FROM cluster_node_join_staging AS staged \
           WHERE staged.node_id = active.node_id) \
         AND NOT EXISTS (SELECT 1 FROM cluster_node_capabilities AS capability \
           WHERE capability.node_id = active.node_id \
             AND capability.capability = '{capability}' \
             AND capability.last_seen_at = active.last_seen_at)"
    )
}

/// "No node is between redeeming a join token and proving what it runs."
///
/// `redeem` publishes the staging row *and* the `cluster_nodes` row before the
/// joiner has started Raft or run a single compatibility check. Such a node is
/// therefore a committed member with no capability row and no heartbeat, and
/// the roster above deliberately cannot see it. Activating past it leaves a
/// voter that counts for quorum and can never open its store again — on a 3→4
/// growth that takes fault tolerance to zero.
///
/// The roster stays honest and the *commit* refuses instead.
///
/// A tombstoned node is excluded, and that is the operator's exit: a
/// redemption that will never finish would otherwise hold activation for
/// good. The other exit is time — a staged node that has stopped heartbeating
/// is named by [`no_absent_node_predicate`] instead, with a refusal that says
/// to start it or remove it. This one is reserved for a join that is actually
/// in flight right now, where the honest answer is "wait".
fn no_join_in_flight_predicate() -> &'static str {
    "NOT EXISTS (SELECT 1 FROM cluster_node_join_staging AS staged \
       WHERE NOT EXISTS (SELECT 1 FROM cluster_nodes AS removed \
         WHERE removed.node_id = staged.node_id AND removed.removed_at IS NOT NULL))"
}

/// The same rule as a roster, so a refusal can name who is joining.
fn join_in_flight_nodes_sql() -> &'static str {
    "SELECT staged.node_id FROM cluster_node_join_staging AS staged \
     WHERE NOT EXISTS (SELECT 1 FROM cluster_nodes AS removed \
       WHERE removed.node_id = staged.node_id AND removed.removed_at IS NOT NULL) \
     ORDER BY staged.node_id"
}

/// "Every member that is not tombstoned has heartbeated since `$3`."
///
/// The capability equality above proves which binary wrote the *last*
/// heartbeat; it says nothing about whether that node still exists. A node
/// that proved the capability and was then powered off stays "ready" forever,
/// and a binary rolled back while the node is down never produces the
/// desynchronising heartbeat the whole design leans on — so it comes back up
/// unable to speak protocol 5 in a cluster that has already narrowed onto it.
///
/// A wall-clock window is a weak proof of presence. It is a perfectly good
/// proof of *absence*, and absence is the only thing this has to establish.
///
/// Mid-join nodes are deliberately *not* excluded, even though `redeem`
/// writes their `last_seen_at` as a redemption timestamp rather than a
/// heartbeat. That timestamp stops advancing the moment a redemption is
/// abandoned, which makes this the thing that eventually names an interrupted
/// join — and names it with the refusal an operator can act on, "start it or
/// remove it", rather than leaving it to a "wait for the join" that will never
/// end.
fn no_absent_node_predicate() -> &'static str {
    "NOT EXISTS (SELECT 1 FROM cluster_nodes AS present \
       WHERE present.removed_at IS NULL AND present.last_seen_at < $3)"
}

/// The same rule as a roster, so a refusal can name who is not answering.
fn absent_nodes_sql() -> &'static str {
    "SELECT present.node_id FROM cluster_nodes AS present \
     WHERE present.removed_at IS NULL AND present.last_seen_at < $1 \
     ORDER BY present.node_id"
}

/// Whether a failed `ALTER TABLE ... ADD COLUMN` failed only because the
/// column is already there. SQLite reports this as a preparation error, so it
/// arrives as an opaque message rather than a typed variant.
fn is_duplicate_column_error(error: &hiqlite::Error) -> bool {
    error.to_string().contains("duplicate column name")
}

/// "No node admitted under the learner protocol is still a member."
///
/// The deactivation precondition, as replicated SQL so it can travel inside
/// the committing transaction. A read-only preflight can be won and then
/// invalidated by a learner being admitted before the write commits.
///
/// `redeem` commits the `role = 'learner'` row before the joiner has joined
/// Raft at all, so a redemption interrupted by a port conflict, crash, or ^C
/// may leave a row for a process that has not started Raft yet. It has already
/// received authority and may resume, which is why the row remains protected.
///
/// A staging row and an old timestamp are not proof of abandonment. The
/// authorized process may be paused after redemption and resume later, so the
/// durable learner row blocks rollback until an explicit removal protocol can
/// tombstone it or a completed promotion changes its durable role.
fn no_admitted_learner_predicate() -> &'static str {
    "NOT EXISTS (SELECT 1 FROM cluster_nodes AS learner \
       WHERE learner.role = 'learner' AND learner.removed_at IS NULL)"
}

/// The same rule as a roster, so a refusal can name what has to be removed.
fn admitted_learner_nodes_sql() -> &'static str {
    "SELECT learner.node_id FROM cluster_nodes AS learner \
     WHERE learner.role = 'learner' AND learner.removed_at IS NULL \
     ORDER BY learner.node_id"
}

/// "Every active node proves `capability` with the binary it is running now."
fn capability_ready_predicate(capability: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM cluster_nodes AS active \
       WHERE {})",
        capability_unready_node_predicate(capability)
    )
}

/// Whether committed Raft membership shows that `role` was actually admitted.
///
/// What "admitted" means depends on what the token admits, and both arms are
/// load-bearing in a way that `is_member` alone is not.
///
/// A voter has to have committed a *vote*. The vendored client joins a voter
/// with `add_learner` and only then `become_member`, so a voter that finalized
/// on membership alone would finalize while it was still Raft's intermediate
/// learner — before it had a vote at all, and while the cluster was counting
/// on it for one.
///
/// A learner has to be a committed member and must *not* have a vote, because
/// a learner that appears in the voter set was not admitted by this protocol
/// and finalizing it would record a learner admission for a voting node.
///
/// How many times a clustered process declined leader-singleton work because
/// it could not read committed membership at all.
///
/// The fail-closed branch below is unreachable for a real daemon today —
/// `metrics_db()` borrows a local watch channel and cannot fail — so it had
/// only a `tracing::warn!` and no way to notice if that ever changed. A counter
/// that stays at zero is the evidence that the branch is still unreachable;
/// one that moves is a cluster declining its own scheduled work in silence.
static CLUSTER_JOB_AUTHORITY_UNREADABLE: AtomicU64 = AtomicU64::new(0);

/// Render the fail-closed job-authority counter for `/metrics`.
///
/// Fixed cardinality, reads one atomic, and stays at `0` on an unclustered
/// process because the question is never asked there.
#[must_use]
pub fn prometheus_cluster_job_authority() -> String {
    format!(
        "# HELP plurx_cluster_job_authority_unreadable_total Times this node declined \
         leader-singleton work because committed cluster membership could not be read.\n\
         # TYPE plurx_cluster_job_authority_unreadable_total counter\n\
         plurx_cluster_job_authority_unreadable_total {}\n",
        CLUSTER_JOB_AUTHORITY_UNREADABLE.load(Ordering::Relaxed)
    )
}

/// Turn "what does committed membership say" into "may this process run the
/// cluster's singleton work", counting the answer nobody can otherwise see.
///
/// A named function for the same reason [`role_is_admitted`] is one: the arm
/// that matters is not reachable from any fixture, so leaving it inline left it
/// both untested and unobservable.
fn decide_cluster_job_authority(committed_voter: Result<bool, MembershipError>) -> bool {
    match committed_voter {
        Ok(is_voter) => is_voter,
        Err(error) => {
            CLUSTER_JOB_AUTHORITY_UNREADABLE.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                code = error.code(),
                "cannot read committed cluster membership; declining leader-singleton work"
            );
            false
        }
    }
}

/// A named function because neither refusal is reachable from a one-voter
/// replicated fixture — one needs a member that is not a voter, the other a
/// voter holding a learner token — and both survived mutation while the
/// decision was inline.
fn role_is_admitted(role: ClusterRole, is_member: bool, is_voter: bool) -> bool {
    match role {
        ClusterRole::Voter => is_voter,
        ClusterRole::Learner => is_member && !is_voter,
    }
}

/// "`cluster_meta` still holds the range this operation was authorized
/// against", with the min and max supplied as `$min` and `$max`.
///
/// A compare-and-swap on both values, so it catches a narrowing *and* a
/// widening. `redeem` reads the range and then takes at least two more
/// linearizable round-trips before its admitting transaction, and both
/// protocol changes can land in that window — so the read alone decides
/// nothing and this rides inside the write.
fn unchanged_protocol_range_predicate(min: u8, max: u8) -> String {
    format!(
        "EXISTS (SELECT 1 FROM cluster_meta \
           WHERE singleton = 1 AND protocol_min = ${min} AND protocol_max = ${max})"
    )
}

/// Everything activation must be true of, as one predicate that travels
/// inside the committing statement.
///
/// `$3` is the absence cutoff. Kept as one function so the read-only pass and
/// the commit cannot drift apart.
fn activation_guard_predicate() -> String {
    format!(
        "{} AND {} AND {}",
        capability_ready_predicate(LEARNER_PROTOCOL_CAPABILITY),
        no_join_in_flight_predicate(),
        no_absent_node_predicate(),
    )
}

/// The same rule as a roster, so a refusal can name the nodes to upgrade
/// instead of telling an operator only that something is behind.
fn capability_unready_nodes_sql(capability: &str) -> String {
    format!(
        "SELECT active.node_id FROM cluster_nodes AS active WHERE {} \
         ORDER BY active.node_id",
        capability_unready_node_predicate(capability)
    )
}

/// Which copy of replicated state a read may answer from.
///
/// The roster route has to keep answering during quorum loss — that is when an
/// operator most needs to see the cluster — so its reads are local. Anything
/// that decides whether to commit a protocol change takes the quorum read, so
/// a partitioned follower can never answer for the cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Local,
    Quorum,
}

/// Committed members that carry no vote, in a stable order.
///
/// Deliberately derived from the committed configuration rather than from
/// replicated SQL: a node's *role* is decided by Raft, and reading it from a
/// table would let a lagging row answer a question Raft has already answered.
fn non_voting_members(voters: &BTreeSet<u64>, members: impl Iterator<Item = u64>) -> Vec<u64> {
    let mut non_voters = members
        .filter(|id| !voters.contains(id))
        .collect::<Vec<_>>();
    non_voters.sort_unstable();
    non_voters.dedup();
    non_voters
}

fn begin_removal_attempt_sql(drain_media: bool) -> String {
    let ready = capability_ready_predicate(REMOVAL_ATTEMPT_CAPABILITY);
    let active_media_guard = if drain_media {
        String::new()
    } else {
        " AND NOT EXISTS (SELECT 1 FROM media_sessions \
             WHERE owner_node_id = $1 AND state = 'active' \
               AND lease_expires_at_ms > $3)"
            .to_owned()
    };
    format!(
        "INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) \
         SELECT $1, $2 WHERE {ready} \
           AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
           AND NOT EXISTS (SELECT 1 FROM cluster_operation_leases) \
           AND NOT EXISTS (SELECT 1 FROM cluster_node_promotions WHERE node_id = $1)\
           {active_media_guard}"
    )
}

fn maintenance_preserves_quorum(
    is_voter: bool,
    target_reachable: bool,
    voting_nodes: usize,
    reachable_voters: usize,
    voting_quorum: usize,
) -> bool {
    !is_voter
        || voting_nodes <= 1
        || reachable_voters.saturating_sub(usize::from(target_reachable)) >= voting_quorum
}

const ACQUIRE_CLUSTER_OPERATION_LEASE_SQL: &str = "INSERT INTO cluster_operation_leases \
       (singleton, node_id, operation, claim_id, expires_at) \
     SELECT 1, $1, $2, $3, $4 \
     WHERE EXISTS (SELECT 1 FROM cluster_nodes node \
         WHERE node.node_id = $1 AND node.removed_at IS NULL) \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_removals) \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_removal_attempts) \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_promotions) \
       AND NOT EXISTS (SELECT 1 FROM cluster_node_join_staging) \
     ON CONFLICT(singleton) DO UPDATE SET \
       node_id = excluded.node_id, operation = excluded.operation, \
       claim_id = excluded.claim_id, expires_at = excluded.expires_at \
     WHERE cluster_operation_leases.expires_at <= $5";

const RELEASE_CLUSTER_OPERATION_LEASE_SQL: &str = "DELETE FROM cluster_operation_leases \
     WHERE singleton = 1 AND node_id = $1 AND operation = $2 AND claim_id = $3";

const RELEASE_RESTART_PREPARATION_SQL: &str = "DELETE FROM cluster_operation_leases \
     WHERE singleton = 1 AND node_id = $1 AND operation = 'restart'";

const EXIT_MAINTENANCE_SQL: &str = "DELETE FROM cluster_node_maintenance WHERE node_id = $1 \
       AND acknowledged_at IS NOT NULL \
       AND EXISTS (SELECT 1 FROM cluster_nodes node \
         JOIN cluster_node_progress progress ON progress.node_id = node.node_id \
         WHERE node.node_id = $1 AND node.removed_at IS NULL \
           AND node.last_seen_at >= $2 AND progress.observed_at >= $2 \
           AND progress.apply_lag_entries = 0 \
           AND EXISTS (SELECT 1 FROM cluster_node_capabilities capability \
             WHERE capability.node_id = node.node_id \
               AND capability.capability = $3 \
               AND capability.last_seen_at = node.last_seen_at) \
           AND NOT EXISTS (SELECT 1 FROM media_sessions session \
             WHERE session.owner_node_id = node.node_id \
               AND session.state = 'active' \
               AND session.lease_expires_at_ms > $4))";

/// Narrow `cluster_meta` onto exactly one protocol.
///
/// `$1` is the protocol to move to and `$2` the protocol the caller believes is
/// active, so a concurrent change loses instead of overwriting. `guard` is the
/// precondition that must hold *inside this transaction*: a read-only preflight
/// can be won and then invalidated by a heartbeat from an older binary before
/// the write commits, exactly as with `begin_removal_attempt_sql`.
fn narrow_protocol_range_sql(guard: Option<String>) -> String {
    let guard = guard
        .map(|guard| format!(" AND {guard}"))
        .unwrap_or_default();
    format!(
        "UPDATE cluster_meta SET protocol_min = $1, protocol_max = $1 \
         WHERE singleton = 1 AND protocol_min = $2 AND protocol_max = $2{guard}"
    )
}

fn rollback_removal_attempt_sql() -> String {
    let ready = capability_ready_predicate(REMOVAL_ATTEMPT_CAPABILITY);
    format!(
        "DELETE FROM cluster_node_removal_attempts WHERE node_id = $1 AND attempt_id = $2 \
         AND {ready}"
    )
}

const MARK_LEGACY_NODE_INSERT_DURING_REMOVAL_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_node_removal_legacy_insert_guard \
     AFTER INSERT ON cluster_nodes \
     WHEN NOT EXISTS (SELECT 1 FROM cluster_node_heartbeat_intents \
       WHERE node_id = NEW.node_id AND last_seen_at = NEW.last_seen_at) \
     BEGIN \
       DELETE FROM cluster_node_capabilities WHERE node_id = NEW.node_id; \
       INSERT INTO cluster_node_join_staging (node_id) \
       SELECT NEW.node_id WHERE EXISTS (SELECT 1 FROM cluster_join_tokens \
         WHERE node_id = NEW.node_id AND state = 'redeeming') \
       ON CONFLICT(node_id) DO NOTHING; \
       DELETE FROM cluster_node_join_staging WHERE node_id = NEW.node_id \
         AND NOT EXISTS (SELECT 1 FROM cluster_join_tokens \
           WHERE node_id = NEW.node_id AND state = 'redeeming'); \
       INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) \
       SELECT node_id, 'internal.legacy-writer' FROM cluster_node_removals \
       WHERE NOT EXISTS (SELECT 1 FROM cluster_join_tokens \
         WHERE node_id = NEW.node_id AND state = 'redeeming') \
       ON CONFLICT(node_id, attempt_id) DO NOTHING; \
     END";
const MARK_LEGACY_NODE_HEARTBEAT_DURING_REMOVAL_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_node_removal_legacy_heartbeat_guard \
     AFTER UPDATE OF last_seen_at ON cluster_nodes \
     WHEN NOT EXISTS (SELECT 1 FROM cluster_node_heartbeat_intents \
       WHERE node_id = NEW.node_id AND last_seen_at = NEW.last_seen_at) \
     BEGIN \
       DELETE FROM cluster_node_capabilities WHERE node_id = NEW.node_id; \
       DELETE FROM cluster_node_join_staging WHERE node_id = NEW.node_id; \
       INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) \
       SELECT node_id, 'internal.legacy-writer' FROM cluster_node_removals WHERE 1 \
       ON CONFLICT(node_id, attempt_id) DO NOTHING; \
     END";
const MARK_LEGACY_NODE_FINALIZE_DURING_REMOVAL_SQL: &str =
    "CREATE TRIGGER IF NOT EXISTS cluster_node_removal_legacy_finalize_guard \
     AFTER UPDATE OF state ON cluster_join_tokens \
     WHEN NEW.state = 'redeemed' \
     BEGIN \
       DELETE FROM cluster_node_join_staging WHERE node_id = NEW.node_id; \
       INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) \
       SELECT removal.node_id, 'internal.legacy-writer' \
       FROM cluster_node_removals AS removal \
       WHERE NOT EXISTS (SELECT 1 FROM cluster_node_capabilities AS capability \
         JOIN cluster_nodes AS node ON node.node_id = capability.node_id \
         WHERE capability.node_id = NEW.node_id \
           AND capability.capability = 'membership_removal_attempt_refs_v1' \
           AND capability.last_seen_at = node.last_seen_at) \
       ON CONFLICT(node_id, attempt_id) DO NOTHING; \
     END";

fn leader_self_leave_sequence(voter_count: usize) -> LeaderSelfLeaveSequence {
    if voter_count.is_multiple_of(2) {
        LeaderSelfLeaveSequence::CommitDirectly
    } else {
        LeaderSelfLeaveSequence::HandoffThenCommit
    }
}

/// Short-lived proof that one admitted voter requested a node-local artwork
/// file. The shared API secret signs this shape but never crosses the public
/// HTTP listener; a captured proof expires after one minute and names exactly
/// one filename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkPeerAuth {
    pub node_id: String,
    pub timestamp_ms: i64,
    pub signature: String,
}

/// Exact ownership proof for the one cluster-wide planned-outage slot.
/// Callers cannot construct one; the replicated compare-and-swap does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterOperationLease {
    node_id: String,
    operation: &'static str,
    claim_id: String,
    expires_at_unix_ms: i64,
}

impl ClusterOperationLease {
    /// Bound a process-local admission fence by both the requested operator
    /// window and the already-committed replicated expiry. Time spent waiting
    /// for consensus consumes the lease instead of moving this deadline.
    #[must_use]
    pub fn preparation_expiry_unix_ms(&self, requested: Duration) -> Option<u64> {
        bounded_local_operation_expiry(unix_ms().ok()?, requested, self.expires_at_unix_ms)
    }
}

fn bounded_local_operation_expiry(
    now_unix_ms: i64,
    requested: Duration,
    replicated_expiry_unix_ms: i64,
) -> Option<u64> {
    let requested_ms = i64::try_from(requested.as_millis()).unwrap_or(i64::MAX);
    let local_expiry = now_unix_ms
        .saturating_add(requested_ms)
        .min(replicated_expiry_unix_ms);
    (local_expiry > now_unix_ms)
        .then(|| u64::try_from(local_expiry).ok())
        .flatten()
}

/// Fencing proof for one leader-arbitrated source repair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtworkRepairClaim {
    deadline: Instant,
    fence: ArtworkRepairFence,
}

impl ArtworkRepairClaim {
    /// Receiver-local deadline for the side effect. Time spent obtaining the
    /// replicated fence consumes the lease.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Replicated proof every provider mutation must compare atomically.
    #[must_use]
    pub fn fence(&self) -> &ArtworkRepairFence {
        &self.fence
    }
}

struct ReplicatedMembership {
    client: Client,
    local_metrics: hiqlite::LocalDbRaftMetrics,
    store: Arc<dyn Store>,
    identity: ClusterIdentity,
    /// What this process was *admitted* as. Used for the two things a boot
    /// record legitimately decides — which schema work this process may run,
    /// and what role a freshly created membership row carries — and for
    /// nothing that decides leadership or singleton work.
    role: ClusterRole,
    storage_root: PathBuf,
    voter_storage_probe: tokio::sync::Mutex<StorageDurabilityObservation>,
    local_voter_role_persisted: AtomicBool,
    local: ClusterPeer,
    local_hostname: String,
    bootstrap_http: String,
    artwork_http: String,
    secrets: JoinSecrets,
    activity_signing_key: ActivitySigningKey,
    activity_public_keys: Mutex<BTreeMap<String, Vec<u8>>>,
    activity_auth_admission: Mutex<BTreeMap<String, ActivityAuthAdmission>>,
    internal_auth_admission: Mutex<BTreeMap<String, ActivityAuthAdmission>>,
    internal_auth_replays: Mutex<BTreeMap<String, InternalAuthReplayWindow>>,
    internal_read_replays: Mutex<BTreeMap<String, InternalReadReplayWindow>>,
    internal_read_preverify: Mutex<InternalReadAdmission>,
    internal_read_authority: tokio::sync::Mutex<BTreeMap<String, InternalReadAuthority>>,
    internal_preverify_admission: Mutex<ActivityAuthAdmission>,
    activity_key_lookup_admission: Mutex<ActivityAuthAdmission>,
    activation_marker: ActivationMarker,
    replication: ReplicationMonitor,
    membership_metrics: PassiveMembershipMetrics,
    heartbeat_writes: HeartbeatWriteGate,
    /// Fast request-path projection of committed role plus the local route
    /// fence. Heartbeats refresh it from local applied SQL and Raft metrics
    /// before publishing progress, so removal cannot cross its barrier while
    /// the target still admits work. HTTP requests read only this atomic.
    local_serving_role: AtomicU8,
    /// Restart-persistent request admission fence, refreshed from local
    /// applied replicated state before each heartbeat is acknowledged.
    local_maintenance: AtomicBool,
    /// First local observation of an older-term claim. `Instant` deliberately
    /// never crosses a process boundary: a successor waits the entire lease
    /// regardless of either host's wall clock.
    artwork_claim_observed_at: Mutex<BTreeMap<i64, (i64, i64, Instant)>>,
}

#[derive(Clone, Copy)]
struct StorageDurabilityObservation {
    successful: bool,
    observed_at: i64,
    checked_at: tokio::time::Instant,
}

#[derive(Default)]
struct HeartbeatWriteGate {
    last_committed: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

impl HeartbeatWriteGate {
    /// Equal heartbeat callers serialize through the first durable result.
    /// Failed writes do not reserve the window, so a waiter retries instead
    /// of observing an optimistic success.
    async fn run<F>(&self, operation: F) -> Result<bool, MembershipError>
    where
        F: Future<Output = Result<(), MembershipError>>,
    {
        let mut last_committed = self.last_committed.lock().await;
        if last_committed.is_some_and(|committed| {
            tokio::time::Instant::now().duration_since(committed) < HEARTBEAT_COALESCE_WINDOW
        }) {
            return Ok(false);
        }
        operation.await?;
        *last_committed = Some(tokio::time::Instant::now());
        Ok(true)
    }
}

fn reachable_after(now: i64) -> i64 {
    now.saturating_sub(NODE_REACHABLE_WINDOW_MS)
}

fn node_is_reachable(now: i64, last_seen_at: i64) -> bool {
    now.saturating_sub(last_seen_at) <= NODE_REACHABLE_WINDOW_MS
}

fn counts_as_ready_read_worker(role: &NodeRole, is_voter: bool, ready: bool) -> bool {
    *role == NodeRole::Learner && !is_voter && ready
}

struct ActivityAuthAdmission {
    window_started: Instant,
    checks: u8,
}

struct InternalReadAdmission {
    window_started: Instant,
    checks: u16,
}

struct InternalReadAuthority {
    expires_at: Instant,
    refresh: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Default)]
struct InternalAuthReplayWindow {
    accepted: VecDeque<(String, Instant)>,
}

impl InternalAuthReplayWindow {
    fn admit(&mut self, nonce: &str, now: Instant) -> bool {
        while self.accepted.front().is_some_and(|(_, accepted_at)| {
            now.saturating_duration_since(*accepted_at)
                > Duration::from_millis(ACTIVITY_AUTH_WINDOW_MS as u64)
        }) {
            self.accepted.pop_front();
        }
        if self.accepted.iter().any(|(accepted, _)| accepted == nonce)
            || self.accepted.len() >= MAX_INTERNAL_REPLAYS_PER_PEER
        {
            return false;
        }
        self.accepted.push_back((nonce.to_owned(), now));
        true
    }
}

#[derive(Default)]
struct InternalReadReplayWindow {
    accepted: VecDeque<(String, Instant)>,
}

impl InternalReadReplayWindow {
    fn admit(&mut self, nonce: &str, now: Instant) -> bool {
        while self.accepted.front().is_some_and(|(_, accepted_at)| {
            now.saturating_duration_since(*accepted_at)
                > Duration::from_millis(INTERNAL_READ_AUTH_WINDOW_MS as u64)
        }) {
            self.accepted.pop_front();
        }
        if self.accepted.iter().any(|(accepted, _)| accepted == nonce)
            || self.accepted.len() >= MAX_INTERNAL_READ_REPLAYS_PER_PEER
        {
            return false;
        }
        self.accepted.push_back((nonce.to_owned(), now));
        true
    }
}

impl ActivityAuthAdmission {
    fn admit(&mut self, now: Instant, maximum: u8) -> bool {
        if now.saturating_duration_since(self.window_started) >= Duration::from_secs(1) {
            self.window_started = now;
            self.checks = 0;
        }
        if self.checks >= maximum {
            return false;
        }
        self.checks += 1;
        true
    }
}

impl InternalReadAdmission {
    fn admit(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.window_started) >= Duration::from_secs(1) {
            self.window_started = now;
            self.checks = 0;
        }
        if self.checks >= MAX_INTERNAL_READ_PREVERIFY_PER_SECOND {
            return false;
        }
        self.checks += 1;
        true
    }
}

fn admit_internal_preverification(
    admission: &Mutex<ActivityAuthAdmission>,
) -> Result<bool, MembershipError> {
    let mut admission = admission.lock().map_err(|_| {
        MembershipError::Internal("internal preverification lock was poisoned".to_owned())
    })?;
    Ok(admission.admit(Instant::now(), MAX_INTERNAL_PREVERIFY_PER_SECOND))
}

fn insert_bounded_activity_public_key(
    keys: &mut BTreeMap<String, Vec<u8>>,
    node_id: String,
    public_key: Vec<u8>,
) -> Option<String> {
    let evicted = if keys.contains_key(&node_id) || keys.len() < MAX_ACTIVITY_PEERS {
        None
    } else {
        keys.keys().next().cloned()
    };
    if let Some(evicted) = evicted.as_deref() {
        keys.remove(evicted);
    }
    keys.insert(node_id, public_key);
    evicted
}

#[derive(Clone)]
pub struct JoinSecrets {
    pub raft: String,
    pub api: String,
    pub credential_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinPayload {
    version: u32,
    pub cluster_id: String,
    pub raft_id: u64,
    pub expires_at: i64,
    pub bootstrap_http: String,
    pub bootstrap: Vec<ClusterPeer>,
    pub secrets: JoinSecretPayload,
    pub activation_marker: ActivationMarker,
    pub schema_version: i64,
    pub protocol_version: i64,
}

/// The learner-admission payload.
///
/// Two differences from v1 carry the whole protocol. It names the role the
/// coordinator bound to this token, and it declares the cluster's *active*
/// protocol range instead of one scalar — a v1 token says "protocol 4" even
/// when it was minted by a cluster that has activated protocol 5, which
/// under-describes what the joining binary actually has to implement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinPayloadV2 {
    version: u32,
    pub cluster_id: String,
    pub raft_id: u64,
    pub expires_at: i64,
    pub bootstrap_http: String,
    pub bootstrap: Vec<ClusterPeer>,
    pub secrets: JoinSecretPayload,
    pub activation_marker: ActivationMarker,
    pub schema_version: i64,
    /// The range `cluster_meta` was on when this token was minted. Advisory:
    /// the cluster can narrow it afterwards, so the joiner's live preflight
    /// against the coordinator stays authoritative.
    pub protocol_min: i64,
    pub protocol_max: i64,
    pub role: ClusterRole,
}

/// A decoded join token of either protocol version.
///
/// Every caller reads the token through this, so adding a version cannot
/// silently leave one call site reading the old shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinToken {
    V1(JoinPayload),
    V2(JoinPayloadV2),
}

impl JoinToken {
    #[must_use]
    pub fn cluster_id(&self) -> &str {
        match self {
            Self::V1(payload) => &payload.cluster_id,
            Self::V2(payload) => &payload.cluster_id,
        }
    }

    #[must_use]
    pub fn raft_id(&self) -> u64 {
        match self {
            Self::V1(payload) => payload.raft_id,
            Self::V2(payload) => payload.raft_id,
        }
    }

    #[must_use]
    pub fn bootstrap(&self) -> &[ClusterPeer] {
        match self {
            Self::V1(payload) => &payload.bootstrap,
            Self::V2(payload) => &payload.bootstrap,
        }
    }

    #[must_use]
    pub fn bootstrap_http(&self) -> &str {
        match self {
            Self::V1(payload) => &payload.bootstrap_http,
            Self::V2(payload) => &payload.bootstrap_http,
        }
    }

    #[must_use]
    pub fn secrets(&self) -> &JoinSecretPayload {
        match self {
            Self::V1(payload) => &payload.secrets,
            Self::V2(payload) => &payload.secrets,
        }
    }

    #[must_use]
    pub fn activation_marker(&self) -> &ActivationMarker {
        match self {
            Self::V1(payload) => &payload.activation_marker,
            Self::V2(payload) => &payload.activation_marker,
        }
    }

    #[must_use]
    pub fn schema_version(&self) -> i64 {
        match self {
            Self::V1(payload) => payload.schema_version,
            Self::V2(payload) => payload.schema_version,
        }
    }

    /// The protocols this token says the cluster is actively using. A v1 token
    /// carries one scalar, which is the one-element range it always was.
    #[must_use]
    pub fn declared_protocol_range(&self) -> (i64, i64) {
        match self {
            Self::V1(payload) => (payload.protocol_version, payload.protocol_version),
            Self::V2(payload) => (payload.protocol_min, payload.protocol_max),
        }
    }

    /// The role this token admits. A v1 token has no role field and never
    /// admits anything but a voter — that is what makes v2 a separate
    /// protocol rather than an optional field.
    #[must_use]
    pub fn role(&self) -> ClusterRole {
        match self {
            Self::V1(_) => ClusterRole::Voter,
            Self::V2(payload) => payload.role,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSecretPayload {
    pub raft: String,
    pub api: String,
    pub credential_key: String,
}

impl std::fmt::Debug for JoinSecretPayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JoinSecretPayload")
            .field("raft", &"<redacted>")
            .field("api", &"<redacted>")
            .field("credential_key", &"<redacted>")
            .finish()
    }
}

impl MembershipManager {
    #[must_use]
    pub fn unavailable() -> Self {
        Self { inner: None }
    }

    #[must_use]
    pub fn is_replicated(&self) -> bool {
        self.inner.is_some()
    }

    #[must_use]
    pub fn metrics_handle(&self) -> PassiveMembershipMetrics {
        self.inner
            .as_deref()
            .map(|inner| inner.membership_metrics.clone())
            .unwrap_or_default()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn replicated(
        client: Client,
        replication: ReplicationMonitor,
        store: Arc<dyn Store>,
        identity: ClusterIdentity,
        local: ClusterPeer,
        bootstrap_http: String,
        artwork_http: String,
        secrets: JoinSecrets,
        activity_signing_key: ActivitySigningKey,
        activation_marker: ActivationMarker,
        role: ClusterRole,
        storage_root: PathBuf,
    ) -> Result<Self, MembershipError> {
        let local_metrics = client
            .local_db_raft_metrics()
            .map_err(MembershipError::from)?;
        let voter_storage_probe = StorageDurabilityObservation {
            successful: voter_storage_durability_probe(&storage_root),
            observed_at: unix_ms()?,
            checked_at: tokio::time::Instant::now(),
        };
        let local_hostname = membership_hostname(
            system_short_hostname().as_deref().unwrap_or_default(),
            &local.api_address,
        );
        let membership_metrics = PassiveMembershipMetrics::replicated();
        let manager = Self {
            inner: Some(Arc::new(ReplicatedMembership {
                client,
                local_metrics,
                store,
                identity,
                role,
                storage_root,
                voter_storage_probe: tokio::sync::Mutex::new(voter_storage_probe),
                local_voter_role_persisted: AtomicBool::new(!role.is_learner()),
                local,
                local_hostname,
                bootstrap_http,
                artwork_http,
                secrets,
                activity_signing_key,
                activity_public_keys: Mutex::new(BTreeMap::new()),
                activity_auth_admission: Mutex::new(BTreeMap::new()),
                internal_auth_admission: Mutex::new(BTreeMap::new()),
                internal_auth_replays: Mutex::new(BTreeMap::new()),
                internal_read_replays: Mutex::new(BTreeMap::new()),
                internal_read_preverify: Mutex::new(InternalReadAdmission {
                    window_started: Instant::now(),
                    checks: 0,
                }),
                internal_read_authority: tokio::sync::Mutex::new(BTreeMap::new()),
                internal_preverify_admission: Mutex::new(ActivityAuthAdmission {
                    window_started: Instant::now(),
                    checks: 0,
                }),
                activity_key_lookup_admission: Mutex::new(ActivityAuthAdmission {
                    window_started: Instant::now(),
                    checks: 0,
                }),
                activation_marker,
                replication,
                membership_metrics,
                heartbeat_writes: HeartbeatWriteGate::default(),
                local_serving_role: AtomicU8::new(LocalServingRole::Fenced.encoded()),
                local_maintenance: AtomicBool::new(true),
                artwork_claim_observed_at: Mutex::new(BTreeMap::new()),
            })),
        };
        manager.initialize().await?;
        Ok(manager)
    }

    fn replicated_inner(&self) -> Result<&ReplicatedMembership, MembershipError> {
        self.inner.as_deref().ok_or(MembershipError::Unavailable)
    }

    async fn initialize(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        if inner.role.is_learner() {
            // A learner never advances replicated schema — that is voter work
            // and it is the one job whose duplication a lease cannot make
            // safe. It still has to prove the cluster's membership schema is
            // one it understands, so it takes the read and refuses on a
            // mismatch rather than skipping the question.
            self.require_membership_schema().await?;
        } else {
            self.install_membership_schema().await?;
        }
        self.heartbeat().await?;
        self.publish_activity_signing_key().await?;
        self.refresh_activity_public_keys().await?;
        self.publish_http_url().await
    }

    async fn require_membership_schema(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<MembershipSchemaRow, _>(
                "SELECT schema_version FROM cluster_membership_meta WHERE singleton = 1",
                params!(),
            )
            .await?;
        if rows.first().map(|row| row.schema_version) != Some(MEMBERSHIP_SCHEMA_VERSION) {
            return Err(MembershipError::Incompatible);
        }
        // The version marker and the additive columns are separate Raft
        // commands, so a learner can arrive between them — and a learner never
        // installs replicated schema, so it cannot repair the gap. It can only
        // say so. Without this probe the mismatch surfaces on its first
        // heartbeat as an opaque `no such column: role`, which tells an
        // operator nothing about what to do.
        for column in MEMBERSHIP_ADDITIVE_COLUMNS {
            if !self.membership_column_exists(column).await? {
                tracing::warn!(
                    table = column.table,
                    column = column.column,
                    "refusing to join: this cluster's membership schema is missing a column \
                     this binary requires"
                );
                return Err(MembershipError::Incompatible);
            }
        }
        Ok(())
    }

    /// Whether one additive membership column is present in replicated SQL.
    async fn membership_column_exists(
        &self,
        column: &AdditiveColumn,
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM pragma_table_info($1) WHERE name = $2",
                params!(column.table, column.column),
            )
            .await?;
        Ok(rows.first().is_some_and(|row| row.count > 0))
    }

    /// Add every missing additive membership column in one Raft transaction.
    ///
    /// One command per `ALTER` left a window between them in which
    /// `cluster_nodes.role` existed and `cluster_join_tokens.role` did not.
    /// Every statement on the join surface names both, so for the length of
    /// that window the whole surface failed to *prepare* — `no such column:
    /// role` — rather than failing one operation. One transaction means the
    /// window does not exist.
    ///
    /// The list is filtered against `pragma_table_info` first, because SQLite
    /// has no `ADD COLUMN IF NOT EXISTS` and a transaction containing an
    /// `ALTER` for a column that is already there fails as a whole — and
    /// "already there" is the steady state on every boot after the first. The
    /// probe can be raced, so the transaction still tolerates the duplicate
    /// and the loop re-reads instead of assuming.
    async fn apply_additive_membership_columns(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        for _ in 0..3 {
            let mut pending = Vec::new();
            for column in MEMBERSHIP_ADDITIVE_COLUMNS {
                if !self.membership_column_exists(column).await? {
                    pending.push((column.statement.to_owned(), params!()));
                }
            }
            if pending.is_empty() {
                return Ok(());
            }
            match inner.client.txn(pending).await {
                Ok(results) => {
                    for result in results {
                        match result {
                            Ok(_) => {}
                            // Another node applied the same list between this
                            // node's probe and its write. The outcome is the
                            // one this was trying to reach.
                            Err(error) if is_duplicate_column_error(&error) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
                Err(error) if is_duplicate_column_error(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let missing = MEMBERSHIP_ADDITIVE_COLUMNS
            .iter()
            .map(|column| format!("{}.{}", column.table, column.column))
            .collect::<Vec<_>>()
            .join(", ");
        Err(MembershipError::Internal(format!(
            "could not apply the additive membership columns ({missing}); the join surface \
             cannot be prepared until they exist"
        )))
    }

    async fn install_membership_schema(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        for statement in MEMBERSHIP_SCHEMA {
            inner.client.execute(*statement, params!()).await?;
        }
        self.apply_additive_membership_columns().await?;
        // One replicated SQLite transaction closes both upgrade directions:
        // fences written before this schema gain a durable legacy reference,
        // and the trigger rejects every later old-coordinator insert. No Raft
        // write can interleave between the backfill and trigger installation.
        inner
            .client
            .txn(vec![
                (BACKFILL_REMOVAL_ATTEMPT_REFS_SQL.to_owned(), params!()),
                (REQUIRE_REMOVAL_INTENT_SQL.to_owned(), params!()),
                (REQUIRE_LEARNER_JOIN_INTENT_SQL.to_owned(), params!()),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let current = inner
            .client
            .execute(
                "INSERT INTO cluster_membership_meta (singleton, schema_version) VALUES (1, $1) \
                 ON CONFLICT(singleton) DO UPDATE SET schema_version = excluded.schema_version \
                 WHERE cluster_membership_meta.schema_version = excluded.schema_version",
                params!(MEMBERSHIP_SCHEMA_VERSION),
            )
            .await?;
        if current == 0 {
            let rows = inner
                .client
                .query_consistent_map::<MembershipSchemaRow, _>(
                    "SELECT schema_version FROM cluster_membership_meta WHERE singleton = 1",
                    params!(),
                )
                .await?;
            if rows.first().map(|row| row.schema_version) != Some(MEMBERSHIP_SCHEMA_VERSION) {
                return Err(MembershipError::Incompatible);
            }
        }
        self.backfill_removed_job_owner_fences().await?;
        self.heartbeat().await?;
        if let Err(error) = self.refresh_membership_metrics().await {
            tracing::warn!(
                code = error.code(),
                "initial cluster membership metrics refresh failed"
            );
        }
        self.publish_activity_signing_key().await?;
        self.refresh_activity_public_keys().await?;
        self.publish_http_url().await
    }

    /// Mint a voter join token. Voter is the default because changing what an
    /// existing token admits would change a cluster's availability silently.
    pub async fn issue_token(&self, ttl: Duration) -> Result<IssuedJoinToken, MembershipError> {
        self.issue_token_for_role(ttl, ClusterRole::Voter).await
    }

    /// Mint a wire-distinct v2 credential for the dedicated learner flow.
    pub async fn issue_learner_token(
        &self,
        ttl: Duration,
    ) -> Result<IssuedJoinToken, MembershipError> {
        self.issue_token_for_role(ttl, ClusterRole::Learner).await
    }

    /// Mint a join token bound to `role`.
    ///
    /// The role is written into the issued-token record, and redemption reads
    /// it back from there. It is deliberately not something the joining node
    /// can assert: a request body is under the joiner's control, and the token
    /// row is under the coordinator's.
    pub async fn issue_token_for_role(
        &self,
        ttl: Duration,
        role: ClusterRole,
    ) -> Result<IssuedJoinToken, MembershipError> {
        let inner = self.replicated_inner()?;
        let (active_min, active_max) = self.active_protocol_range().await?;
        if role.is_learner() && !(active_min..=active_max).contains(&AUTH_LEARNER_PROTOCOL) {
            return Err(MembershipError::LearnerProtocolInactive);
        }
        let now = unix_ms()?;
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        let expires_at = now.saturating_add(ttl_ms.clamp(1_000, 86_400_000));
        let metrics = inner.client.metrics_db().await?;
        let bootstrap = metrics
            .membership_config
            .nodes()
            .map(|(_, node)| ClusterPeer::from(node))
            .collect::<Vec<_>>();
        if bootstrap.is_empty() {
            return Err(MembershipError::Internal(
                "cluster has no bootstrap member".to_owned(),
            ));
        }

        for _ in 0..8 {
            // Allocate above durable node records *and* the ids that live
            // tokens are currently holding, so a second token can be minted
            // while the first is still outstanding. Expired and redeemed rows
            // are excluded, so an abandoned token stops reserving its id once
            // it lapses rather than consuming the id space permanently.
            let rows = inner
                .client
                .query_consistent_map::<MaxRaftIdRow, _>(
                    "SELECT MAX(raft_id) AS max_raft_id FROM (\
                       SELECT raft_id FROM cluster_nodes \
                       UNION ALL \
                       SELECT raft_id FROM cluster_join_tokens \
                       WHERE state IN ('issued', 'redeeming') AND expires_at > $1\
                     )",
                    params!(now),
                )
                .await?;
            let raft_id = rows
                .first()
                .and_then(|row| row.max_raft_id)
                .unwrap_or(1)
                .saturating_add(1) as u64;
            let secrets = JoinSecretPayload {
                raft: inner.secrets.raft.clone(),
                api: inner.secrets.api.clone(),
                credential_key: inner.secrets.credential_key.clone(),
            };
            let token = match role {
                // A v1 token still carries one scalar, because a coordinator
                // and a joiner that predate the range compare it for equality.
                // It names the cluster's active floor rather than a constant,
                // so a build that cannot participate is turned away by its own
                // cheap local check instead of getting as far as a preflight.
                ClusterRole::Voter => encode_join_token(&JoinPayload {
                    version: JOIN_TOKEN_VERSION,
                    cluster_id: inner.identity.cluster_id.clone(),
                    raft_id,
                    expires_at,
                    bootstrap_http: inner.bootstrap_http.clone(),
                    bootstrap: bootstrap.clone(),
                    secrets,
                    activation_marker: inner.activation_marker.clone(),
                    schema_version: AUTH_SCHEMA_VERSION,
                    protocol_version: active_min,
                })?,
                ClusterRole::Learner => encode_join_token_v2(&JoinPayloadV2 {
                    version: JOIN_TOKEN_V2_VERSION,
                    cluster_id: inner.identity.cluster_id.clone(),
                    raft_id,
                    expires_at,
                    bootstrap_http: inner.bootstrap_http.clone(),
                    bootstrap: bootstrap.clone(),
                    secrets,
                    activation_marker: inner.activation_marker.clone(),
                    schema_version: AUTH_SCHEMA_VERSION,
                    protocol_min: active_min,
                    protocol_max: active_max,
                    role,
                })?,
            };
            let token_hash = join_token_digest(&token);
            let inserted = inner
                .client
                .execute(
                    "INSERT INTO cluster_join_tokens \
                     (token_hash, raft_id, expires_at, state, node_id, created_at, redeemed_at, \
                      role) \
                     SELECT $1, $2, $3, 'issued', NULL, $4, NULL, $5 \
                     WHERE NOT EXISTS (\
                       SELECT 1 FROM cluster_join_tokens \
                       WHERE raft_id = $2 AND state IN ('issued', 'redeeming') AND expires_at > $4\
                     )",
                    params!(token_hash, raft_id as i64, expires_at, now, role.as_str()),
                )
                .await?;
            if inserted == 1 {
                return Ok(IssuedJoinToken {
                    token,
                    expires_at,
                    raft_id,
                });
            }
        }
        Err(MembershipError::Internal(
            "could not allocate a unique Raft id".to_owned(),
        ))
    }

    pub async fn redeem(&self, request: &RedeemJoinRequest) -> Result<(), MembershipError> {
        self.redeem_for_role(request, ClusterRole::Voter).await
    }

    /// Redeem a token through the wire-distinct learner path.
    pub async fn redeem_learner(&self, request: &RedeemJoinRequest) -> Result<(), MembershipError> {
        self.redeem_for_role(request, ClusterRole::Learner).await
    }

    async fn redeem_for_role(
        &self,
        request: &RedeemJoinRequest,
        expected_role: ClusterRole,
    ) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        if request.schema_version != AUTH_SCHEMA_VERSION {
            return Err(MembershipError::Incompatible);
        }
        let http_base = validate_redeem_join_request(request)?;
        // Possession is checked before the cluster-wide protocol projection.
        // An unknown digest pays for one indexed token lookup, not an
        // otherwise-valid quorum range read on behalf of an unauthenticated
        // caller.
        let record = self.token_record(&request.token_digest).await?;
        if record.raft_id != request.raft_id as i64 {
            return Err(MembershipError::InvalidToken);
        }
        // The role is *derived*, never accepted. `RedeemJoinRequest` carries
        // no role field precisely so that a joining process cannot ask to be
        // something other than what the operator's token was minted for.
        let role = record.role()?;
        if role != expected_role {
            return Err(MembershipError::InvalidToken);
        }
        if self.maintenance_operation_pending().await? {
            return Err(MembershipError::MaintenanceConflict(
                request.node_id.clone(),
            ));
        }
        // The same rule the boot-time guard applies, from the coordinator's
        // side: the joiner has to implement every protocol this cluster is
        // actively using. Comparing against a constant instead would admit a
        // protocol-4-only binary into an activated cluster.
        let (cluster_min, cluster_max) = self.active_protocol_range().await?;
        let (joiner_min, joiner_max) = request.declared_protocol_range();
        if !(joiner_min <= cluster_min && cluster_max <= joiner_max) {
            tracing::warn!(
                cluster_min,
                cluster_max,
                joiner_min,
                joiner_max,
                node_id = %request.node_id,
                "refusing a join from a binary that does not implement this cluster's \
                 active protocol range"
            );
            return Err(MembershipError::Incompatible);
        }
        let now = unix_ms()?;
        if role.is_learner() && !(cluster_min..=cluster_max).contains(&AUTH_LEARNER_PROTOCOL) {
            tracing::warn!(
                cluster_min,
                cluster_max,
                node_id = %request.node_id,
                "refusing a learner join because this cluster has not activated the \
                 learner protocol"
            );
            return Err(MembershipError::LearnerProtocolInactive);
        }
        let resume_legacy_partial = match record.state.as_str() {
            "redeemed" => return Err(MembershipError::ReusedToken),
            "redeeming" if record.node_id.as_deref() != Some(&request.node_id) => {
                return Err(MembershipError::ReservedToken)
            }
            // Redemption reserves the credential to one generated node id.
            // That same staged node may resume after the original TTL; expiry
            // still refuses an unused token below, and a different node id is
            // refused above, so this does not restore bearer authority.
            "redeeming" => {
                match self.redeeming_node_matches(request).await? {
                    Some(false) => return Err(MembershipError::NodeIdentityInUse),
                    Some(true) => {
                        if let Some(http_base) = http_base.as_deref() {
                            self.claim_redeeming_http_origin(request, http_base).await?;
                        }
                        self.upsert_hostname(
                            &request.node_id,
                            &membership_hostname(&request.hostname, &request.api_address),
                        )
                        .await?;
                        return Ok(());
                    }
                    // The previous rolling version reserved the token before
                    // its node-publication transaction. Repair that crash
                    // shape below under the exact reservation.
                    None if role.is_learner() => {
                        return Err(MembershipError::Internal(
                            "learner token reservation has no staged node".to_owned(),
                        ));
                    }
                    None => true,
                }
            }
            "issued" if record.expires_at <= now => return Err(MembershipError::ExpiredToken),
            "issued" => false,
            _ => return Err(MembershipError::InvalidToken),
        };

        // Claiming the origin, reserving the token, installing the rolling-
        // upgrade guards, and publishing the staged node are one Raft
        // transaction. The token reservation returns the claimed node id and
        // every publication statement consumes that output. A duplicate
        // origin or a lost token race therefore produces no dependency output
        // and Hiqlite rolls the entire transaction back.
        //
        // The protocol range this admission was authorized against rides in
        // the *first* statement for exactly that reason. It was read above,
        // with at least two more linearizable round-trips between there and
        // here, and `deactivate_learner_protocol` can narrow the range in that
        // window — admitting a learner into a cluster that has just rolled
        // back, which cannot restart and which nothing can then remove. Every
        // later statement consumes this statement's output, so a range that
        // moved rolls the whole join back rather than half-committing it. It
        // is a compare-and-swap on both values, so a widening is caught too: a
        // protocol-4-only binary must not be admitted into a cluster that
        // activated while its request was in flight.
        let mut statements = Vec::new();
        if role.is_learner() && !resume_legacy_partial {
            statements.push((
                "INSERT INTO cluster_learner_join_intents (token_hash) \
                     SELECT token_hash FROM cluster_join_tokens \
                     WHERE token_hash = $1 AND raft_id = $2 AND state = 'issued' \
                       AND expires_at > $3 AND role = 'learner' \
                       AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
                       AND NOT EXISTS (SELECT 1 FROM cluster_operation_leases) \
                     RETURNING token_hash"
                    .to_owned(),
                params!(request.token_digest.as_str(), request.raft_id as i64, now),
            ));
        }
        let proof_statement_index = if let Some(http_base) = http_base.as_deref() {
            let http_statement_index = statements.len();
            statements.push((
                format!(
                    "INSERT INTO cluster_node_http (node_id, public_http_url) \
                     SELECT $1, $2 WHERE EXISTS (\
                       SELECT 1 FROM cluster_join_tokens token \
                       WHERE token.token_hash = $3 AND token.raft_id = $4 \
                         AND ((token.state = 'issued' AND token.expires_at > $5) \
                           OR (token.state = 'redeeming' AND token.node_id = $1))) \
                     AND NOT EXISTS (SELECT 1 FROM cluster_node_http_claims claim \
                       WHERE claim.node_id = $1 AND claim.public_http_url != $2) \
                     AND NOT EXISTS (SELECT 1 FROM cluster_nodes WHERE node_id = $1) \
                     AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
                     AND NOT EXISTS (SELECT 1 FROM cluster_operation_leases) \
                     AND NOT EXISTS (\
                       SELECT 1 FROM cluster_node_http owner_http \
                       JOIN cluster_nodes owner_node ON owner_node.node_id = owner_http.node_id \
                       WHERE owner_http.public_http_url = $2 AND owner_http.node_id != $1 \
                         AND owner_node.removed_at IS NULL \
                         AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removing \
                           WHERE removing.node_id = owner_node.node_id)) \
                     AND {} \
                     ON CONFLICT(node_id) DO UPDATE SET \
                       public_http_url = excluded.public_http_url \
                     RETURNING node_id",
                    unchanged_protocol_range_predicate(6, 7)
                ),
                params!(
                    request.node_id.as_str(),
                    http_base,
                    request.token_digest.as_str(),
                    request.raft_id as i64,
                    now,
                    cluster_min,
                    cluster_max
                ),
            ));
            if resume_legacy_partial {
                http_statement_index
            } else {
                let reservation_statement_index = statements.len();
                statements.push((
                    "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = $1 \
                     WHERE token_hash = $2 AND state = 'issued' AND expires_at > $3 \
                       AND NOT EXISTS (SELECT 1 FROM cluster_nodes WHERE node_id = $1) \
                       AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
                       AND NOT EXISTS (SELECT 1 FROM cluster_operation_leases) \
                     RETURNING node_id"
                        .to_owned(),
                    vec![
                        Param::StmtOutputNamed(http_statement_index, "node_id".into()),
                        Param::Text(request.token_digest.clone()),
                        Param::Integer(now),
                    ],
                ));
                reservation_statement_index
            }
        } else if resume_legacy_partial {
            let reservation_statement_index = statements.len();
            statements.push((
                format!(
                    "UPDATE cluster_join_tokens SET node_id = node_id \
                     WHERE node_id = $1 AND token_hash = $2 AND raft_id = $3 \
                       AND state = 'redeeming' \
                       AND NOT EXISTS (SELECT 1 FROM cluster_nodes WHERE node_id = $1) \
                       AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
                       AND NOT EXISTS (SELECT 1 FROM cluster_operation_leases) \
                       AND {} \
                     RETURNING node_id",
                    unchanged_protocol_range_predicate(4, 5)
                ),
                params!(
                    request.node_id.as_str(),
                    request.token_digest.as_str(),
                    request.raft_id as i64,
                    cluster_min,
                    cluster_max
                ),
            ));
            reservation_statement_index
        } else {
            let reservation_statement_index = statements.len();
            statements.push((
                format!(
                    "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = $1 \
                     WHERE token_hash = $2 AND state = 'issued' AND expires_at > $3 \
                       AND NOT EXISTS (SELECT 1 FROM cluster_nodes WHERE node_id = $1) \
                       AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
                       AND NOT EXISTS (SELECT 1 FROM cluster_operation_leases) \
                       AND {} \
                     RETURNING node_id",
                    unchanged_protocol_range_predicate(4, 5)
                ),
                params!(
                    request.node_id.as_str(),
                    request.token_digest.as_str(),
                    now,
                    cluster_min,
                    cluster_max
                ),
            ));
            reservation_statement_index
        };
        if let Some(http_base) = http_base.as_deref() {
            statements.push((
                "INSERT INTO cluster_node_http_claims (node_id, public_http_url) \
                 VALUES ($1, $2) ON CONFLICT(node_id) DO UPDATE SET \
                   public_http_url = excluded.public_http_url \
                 WHERE cluster_node_http_claims.public_http_url = excluded.public_http_url"
                    .to_owned(),
                vec![
                    Param::StmtOutputNamed(proof_statement_index, "node_id".into()),
                    Param::Text(http_base.to_owned()),
                ],
            ));
        }
        statements.extend([
            (
                "INSERT INTO cluster_node_heartbeat_intents (node_id, last_seen_at) \
                 VALUES ($1, $2) ON CONFLICT(node_id) DO UPDATE SET \
                   last_seen_at = excluded.last_seen_at"
                    .to_owned(),
                vec![
                    Param::StmtOutputNamed(proof_statement_index, "node_id".into()),
                    Param::Integer(now),
                ],
            ),
            (
                "INSERT INTO cluster_node_join_staging (node_id) VALUES ($1) \
                 ON CONFLICT(node_id) DO NOTHING"
                    .to_owned(),
                vec![Param::StmtOutputNamed(
                    proof_statement_index,
                    "node_id".into(),
                )],
            ),
            (
                "INSERT INTO cluster_nodes \
                 (node_id, raft_id, raft_address, api_address, last_seen_at, removed_at, role) \
                 VALUES ($1, $2, $3, $4, $5, NULL, $6)"
                    .to_owned(),
                vec![
                    Param::StmtOutputNamed(proof_statement_index, "node_id".into()),
                    Param::Integer(request.raft_id as i64),
                    Param::Text(request.raft_address.clone()),
                    Param::Text(request.api_address.clone()),
                    Param::Integer(now),
                    Param::Text(role.as_str().to_owned()),
                ],
            ),
            (
                "INSERT INTO cluster_node_hostnames (node_id, hostname) VALUES ($1, $2) \
                 ON CONFLICT(node_id) DO UPDATE SET hostname = excluded.hostname"
                    .to_owned(),
                vec![
                    Param::StmtOutputNamed(proof_statement_index, "node_id".into()),
                    Param::Text(membership_hostname(&request.hostname, &request.api_address)),
                ],
            ),
            (
                "DELETE FROM cluster_node_heartbeat_intents WHERE node_id = $1 \
                 AND last_seen_at = $2"
                    .to_owned(),
                vec![
                    Param::StmtOutputNamed(proof_statement_index, "node_id".into()),
                    Param::Integer(now),
                ],
            ),
        ]);
        if role.is_learner() {
            statements.push((
                "DELETE FROM cluster_learner_join_intents WHERE token_hash = $1".to_owned(),
                params!(request.token_digest.as_str()),
            ));
        }
        let transaction = inner.client.txn(statements).await;
        match transaction {
            Ok(results) => {
                results.into_iter().collect::<Result<Vec<_>, _>>()?;
            }
            Err(error) if error.to_string().contains("StmtIndex(") => {
                if self.maintenance_operation_pending().await? {
                    return Err(MembershipError::MaintenanceConflict(
                        request.node_id.clone(),
                    ));
                }
                // The range predicate is one of the things that can have
                // rolled this transaction back, and it is the one whose real
                // answer would otherwise be reported as a bad token. Check it
                // before anything else so the joiner is told what happened.
                let (now_min, now_max) = self.active_protocol_range().await?;
                if (now_min, now_max) != (cluster_min, cluster_max) {
                    tracing::warn!(
                        cluster_min,
                        cluster_max,
                        now_min,
                        now_max,
                        node_id = %request.node_id,
                        "rolling back a join because the cluster's protocol range moved while \
                         it was being admitted"
                    );
                    return Err(MembershipError::ProtocolRangeChanged);
                }
                let latest = self.token_record(&request.token_digest).await?;
                if latest.state == "redeemed" {
                    return Err(MembershipError::ReusedToken);
                }
                if latest.state == "redeeming" {
                    if latest.node_id.as_deref() != Some(&request.node_id) {
                        return Err(MembershipError::ReservedToken);
                    }
                    return match self.redeeming_node_matches(request).await? {
                        Some(false) => Err(MembershipError::NodeIdentityInUse),
                        Some(true) => {
                            if let Some(http_base) = http_base.as_deref() {
                                self.claim_redeeming_http_origin(request, http_base).await?;
                            }
                            self.upsert_hostname(
                                &request.node_id,
                                &membership_hostname(&request.hostname, &request.api_address),
                            )
                            .await?;
                            Ok(())
                        }
                        None if http_base.is_some() => Err(MembershipError::HttpEndpointInUse),
                        None => Err(MembershipError::Internal(
                            "legacy join reservation could not publish its node row".to_owned(),
                        )),
                    };
                }
                return if latest.expires_at <= now {
                    Err(MembershipError::ExpiredToken)
                } else if self.node_identity_exists(&request.node_id).await? {
                    Err(MembershipError::NodeIdentityInUse)
                } else if http_base.is_some() {
                    Err(MembershipError::HttpEndpointInUse)
                } else {
                    Err(MembershipError::InvalidToken)
                };
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    async fn upsert_hostname(&self, node_id: &str, hostname: &str) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        inner
            .client
            .execute(
                "INSERT INTO cluster_node_hostnames (node_id, hostname) VALUES ($1, $2) \
                 ON CONFLICT(node_id) DO UPDATE SET hostname = excluded.hostname",
                params!(node_id, hostname),
            )
            .await?;
        Ok(())
    }

    pub async fn finalize(&self, request: &FinalizeJoinRequest) -> Result<(), MembershipError> {
        self.finalize_for_role(request, ClusterRole::Voter).await
    }

    /// Finalize only a token admitted through the learner flow.
    pub async fn finalize_learner(
        &self,
        request: &FinalizeJoinRequest,
    ) -> Result<(), MembershipError> {
        self.finalize_for_role(request, ClusterRole::Learner).await
    }

    async fn finalize_for_role(
        &self,
        request: &FinalizeJoinRequest,
        expected_role: ClusterRole,
    ) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        if !is_join_token_digest(&request.token_digest) {
            return Err(MembershipError::InvalidToken);
        }
        let record = self.token_record(&request.token_digest).await?;
        if record.role()? != expected_role {
            return Err(MembershipError::InvalidToken);
        }
        if record.node_id.as_deref() != Some(&request.node_id)
            || record.raft_id != request.raft_id as i64
        {
            return Err(MembershipError::ReservedToken);
        }
        if record.state == "redeemed" {
            return Ok(());
        }
        // What "admitted" means depends on what the token admits. A voter has
        // to have committed a *vote*; a learner has to be a committed member
        // and must not have acquired one, because a learner that appears in
        // the voter set was not admitted by this protocol at all.
        let metrics = inner.client.metrics_db().await?;
        let is_voter = metrics
            .membership_config
            .voter_ids()
            .any(|id| id == request.raft_id);
        let is_member = metrics
            .membership_config
            .nodes()
            .any(|(id, _)| *id == request.raft_id);
        let admitted = role_is_admitted(record.role()?, is_member, is_voter);
        if !admitted {
            return Err(MembershipError::Internal(format!(
                "joining node has not committed {} membership",
                record.role()?.as_str()
            )));
        }
        let now = unix_ms()?;
        let changed = inner
            .client
            .execute(
                "UPDATE cluster_join_tokens SET state = 'redeemed', redeemed_at = $1 \
                 WHERE token_hash = $2 AND state = 'redeeming' AND node_id = $3",
                params!(now, request.token_digest.as_str(), request.node_id.as_str()),
            )
            .await?;
        if changed != 1 {
            return Err(MembershipError::ReusedToken);
        }
        Ok(())
    }

    async fn token_record(&self, token_hash: &str) -> Result<JoinTokenRow, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<JoinTokenRow, _>(
                "SELECT raft_id, expires_at, state, node_id, role FROM cluster_join_tokens \
                 WHERE token_hash = $1",
                params!(token_hash),
            )
            .await?;
        rows.into_iter().next().ok_or(MembershipError::InvalidToken)
    }

    async fn node_identity_exists(&self, node_id: &str) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM cluster_nodes WHERE node_id = $1",
                params!(node_id),
            )
            .await?;
        Ok(rows.first().is_some_and(|row| row.count == 1))
    }

    /// Classify a same-node reservation left by an interrupted redemption.
    /// `None` is the legacy crash shape; `Some(true)` is an already-published
    /// retry; `Some(false)` is an identity collision and must never be upserted.
    async fn redeeming_node_matches(
        &self,
        request: &RedeemJoinRequest,
    ) -> Result<Option<bool>, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<RedeemingNodeRow, _>(
                "SELECT node.raft_id, node.raft_address, node.api_address, node.removed_at, \
                   EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id) AS removal_pending \
                 FROM cluster_nodes node WHERE node.node_id = $1",
                params!(request.node_id.as_str()),
            )
            .await?;
        Ok(rows.first().map(|row| {
            row.raft_id == request.raft_id as i64
                && row.raft_address == request.raft_address
                && row.api_address == request.api_address
                && row.removed_at.is_none()
                && !row.removal_pending
        }))
    }

    /// Upgrade a previously published legacy redemption with its exact HTTP
    /// origin. The serialized statement revalidates both token reservation
    /// and node identity so removal or finalization cannot race the repair.
    async fn claim_redeeming_http_origin(
        &self,
        request: &RedeemJoinRequest,
        http_base: &str,
    ) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let transaction = inner
            .client
            .txn(vec![
                (
                    "INSERT INTO cluster_node_http (node_id, public_http_url) \
                     SELECT $1, $2 WHERE EXISTS (\
                       SELECT 1 FROM cluster_join_tokens token \
                       WHERE token.token_hash = $3 AND token.raft_id = $4 \
                         AND token.state = 'redeeming' AND token.node_id = $1) \
                     AND EXISTS (\
                       SELECT 1 FROM cluster_nodes node \
                       WHERE node.node_id = $1 AND node.raft_id = $4 \
                         AND node.raft_address = $5 AND node.api_address = $6 \
                         AND node.removed_at IS NULL \
                         AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                           WHERE removal.node_id = node.node_id)) \
                     AND NOT EXISTS (SELECT 1 FROM cluster_node_http_claims claim \
                       WHERE claim.node_id = $1 AND claim.public_http_url != $2) \
                     AND NOT EXISTS (\
                       SELECT 1 FROM cluster_node_http owner_http \
                       JOIN cluster_nodes owner_node ON owner_node.node_id = owner_http.node_id \
                       WHERE owner_http.public_http_url = $2 AND owner_http.node_id != $1 \
                         AND owner_node.removed_at IS NULL \
                         AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removing \
                           WHERE removing.node_id = owner_node.node_id)) \
                     ON CONFLICT(node_id) DO UPDATE SET \
                       public_http_url = excluded.public_http_url \
                     RETURNING node_id"
                        .to_owned(),
                    params!(
                        request.node_id.as_str(),
                        http_base,
                        request.token_digest.as_str(),
                        request.raft_id as i64,
                        request.raft_address.as_str(),
                        request.api_address.as_str()
                    ),
                ),
                (
                    "INSERT INTO cluster_node_http_claims (node_id, public_http_url) \
                     VALUES ($1, $2) ON CONFLICT(node_id) DO UPDATE SET \
                       public_http_url = excluded.public_http_url \
                     WHERE cluster_node_http_claims.public_http_url = excluded.public_http_url"
                        .to_owned(),
                    vec![
                        Param::StmtOutputNamed(0, "node_id".into()),
                        Param::Text(http_base.to_owned()),
                    ],
                ),
            ])
            .await;
        match transaction {
            Ok(results) => {
                results.into_iter().collect::<Result<Vec<_>, _>>()?;
                return Ok(());
            }
            Err(error) if error.to_string().contains("StmtIndex(") => {}
            Err(error) => return Err(error.into()),
        }
        let latest = self.token_record(&request.token_digest).await?;
        if latest.state == "redeemed" {
            return Err(MembershipError::ReusedToken);
        }
        if latest.node_id.as_deref() != Some(&request.node_id) {
            return Err(MembershipError::ReservedToken);
        }
        match self.redeeming_node_matches(request).await? {
            Some(false) => Err(MembershipError::NodeIdentityInUse),
            _ => Err(MembershipError::HttpEndpointInUse),
        }
    }

    pub async fn heartbeat(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        inner
            .heartbeat_writes
            .run(self.commit_heartbeat(inner))
            .await
            .map(|_| ())
    }

    /// Submit a heartbeat without the duplicate gate so the separate-process
    /// fault harness can prove tombstone SQL is actually exercised.
    #[cfg(feature = "cluster-validation")]
    #[doc(hidden)]
    pub async fn validation_force_heartbeat(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        self.commit_heartbeat(inner).await
    }

    async fn commit_heartbeat(&self, inner: &ReplicatedMembership) -> Result<(), MembershipError> {
        // Observe maintenance before publishing any acknowledgement. From this
        // point onward request admission and singleton jobs are fenced even if
        // the transaction below is delayed or the response is lost.
        let maintenance_requested = self.refresh_local_maintenance(inner).await?;
        // This local read is also the removal protocol's target-side route
        // barrier, and the barrier is the *publication*, not the call: the
        // refresh finishes before last_applied is sampled and published, so a
        // coordinator that observes the heartbeat write knows the role this
        // process is serving under reflects a read taken before that write.
        // It does not mean the process is fenced — a healthy node publishes
        // `Voter` here and keeps serving, which is the point.
        let serving_role = self.refresh_local_route_admission(inner).await?;
        if serving_role == LocalServingRole::Voter && inner.role.is_learner() {
            self.persist_local_promoted_voter_role(inner).await?;
        }
        let now = unix_ms()?;
        let local = inner.local_metrics.snapshot();
        let passive = inner.replication.metrics_handle().snapshot();
        let watermark = passive.watermark;
        let bounded_read_ready = passive.local_source
            && passive.watermark_valid
            && passive.watermark_local_reads_supported
            && watermark.is_some_and(|sample| sample.apply_lag_entries == Some(0));
        let storage_headroom = available_storage_headroom_bytes(&inner.storage_root);
        let storage_probe = self.refresh_voter_storage_probe(inner).await?;
        let storage_probe_fresh =
            now.saturating_sub(storage_probe.observed_at) <= STORAGE_DURABILITY_PROBE_MAX_AGE_MS;
        let voter_storage_ready = storage_probe.successful
            && storage_probe_fresh
            && storage_headroom.is_some_and(|bytes| bytes >= MIN_VOTER_STORAGE_HEADROOM_BYTES);
        let voter_role_persisted = inner.local_voter_role_persisted.load(Ordering::Acquire);
        let to_sql = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        let mut statements = vec![
            (
                "DELETE FROM cluster_operation_leases WHERE expires_at <= $1".to_owned(),
                params!(now),
            ),
            (
                "INSERT INTO cluster_node_heartbeat_intents (node_id, last_seen_at) \
                     VALUES ($1, $2) ON CONFLICT(node_id) DO UPDATE SET \
                       last_seen_at = excluded.last_seen_at"
                    .to_owned(),
                params!(inner.identity.node_id.as_str(), now),
            ),
            (
                "INSERT INTO cluster_node_maintenance_heartbeat_intents \
                     (node_id, last_seen_at) VALUES ($1, $2) \
                     ON CONFLICT(node_id) DO UPDATE SET last_seen_at = excluded.last_seen_at"
                    .to_owned(),
                params!(inner.identity.node_id.as_str(), now),
            ),
            (
                // `role` is supplied only on the insert branch. The row
                // redemption already wrote owns the admission decision,
                // and a promotion — which this slice does not implement —
                // must be able to move it without a heartbeat undoing it.
                "INSERT INTO cluster_nodes \
                     (node_id, raft_id, raft_address, api_address, last_seen_at, removed_at, \
                      role) \
                     VALUES ($1, $2, $3, $4, $5, NULL, $6) \
                     ON CONFLICT(node_id) DO UPDATE SET last_seen_at = excluded.last_seen_at, \
                       removed_at = NULL WHERE cluster_nodes.removed_at IS NULL"
                    .to_owned(),
                params!(
                    inner.identity.node_id.as_str(),
                    inner.identity.raft_id as i64,
                    inner.local.raft_address.as_str(),
                    inner.local.api_address.as_str(),
                    now,
                    inner.role.as_str()
                ),
            ),
            (
                "INSERT INTO cluster_node_progress \
                     (node_id, current_term, last_applied_index, quorum_committed_index, \
                      apply_lag_entries, bounded_read_ready, voter_storage_ready, \
                      storage_headroom_bytes, storage_probe_observed_at, \
                      voter_role_persisted, observed_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                     ON CONFLICT(node_id) DO UPDATE SET \
                       current_term = excluded.current_term, \
                       last_applied_index = excluded.last_applied_index, \
                       quorum_committed_index = excluded.quorum_committed_index, \
                       apply_lag_entries = excluded.apply_lag_entries, \
                       bounded_read_ready = excluded.bounded_read_ready, \
                       voter_storage_ready = excluded.voter_storage_ready, \
                       storage_headroom_bytes = excluded.storage_headroom_bytes, \
                       storage_probe_observed_at = excluded.storage_probe_observed_at, \
                       voter_role_persisted = excluded.voter_role_persisted, \
                       observed_at = excluded.observed_at"
                    .to_owned(),
                params!(
                    inner.identity.node_id.as_str(),
                    to_sql(local.current_term),
                    local.last_applied_index.map(to_sql),
                    watermark.map(|sample| to_sql(sample.committed_index)),
                    watermark
                        .and_then(|sample| sample.apply_lag_entries)
                        .map(to_sql),
                    bounded_read_ready,
                    voter_storage_ready,
                    storage_headroom.map(to_sql),
                    storage_probe.observed_at,
                    voter_role_persisted,
                    now
                ),
            ),
            (
                "INSERT INTO cluster_node_capabilities \
                     (node_id, capability, last_seen_at) VALUES ($1, $2, $3) \
                     ON CONFLICT(node_id, capability) DO UPDATE SET \
                       last_seen_at = excluded.last_seen_at"
                    .to_owned(),
                params!(
                    inner.identity.node_id.as_str(),
                    REMOVAL_ATTEMPT_CAPABILITY,
                    now
                ),
            ),
            (
                "INSERT INTO cluster_node_capabilities \
                     (node_id, capability, last_seen_at) VALUES ($1, $2, $3) \
                     ON CONFLICT(node_id, capability) DO UPDATE SET \
                       last_seen_at = excluded.last_seen_at"
                    .to_owned(),
                params!(
                    inner.identity.node_id.as_str(),
                    NODE_MAINTENANCE_CAPABILITY,
                    now
                ),
            ),
        ];
        // Same transaction, same timestamp, same coupling: a protocol-5
        // capability row can only carry this heartbeat's `last_seen_at` if this
        // binary wrote this heartbeat. A rollback to an older build advances
        // `cluster_nodes.last_seen_at` without touching this row, and the
        // equality that activation requires breaks. That is the whole contract,
        // so the only way to omit the statement is to be the emulated old
        // binary the validation harness starts.
        if !emulate_pre_learner_protocol_heartbeat() {
            statements.push((
                "INSERT INTO cluster_node_capabilities \
                     (node_id, capability, last_seen_at) VALUES ($1, $2, $3) \
                     ON CONFLICT(node_id, capability) DO UPDATE SET \
                       last_seen_at = excluded.last_seen_at"
                    .to_owned(),
                params!(
                    inner.identity.node_id.as_str(),
                    LEARNER_PROTOCOL_CAPABILITY,
                    now
                ),
            ));
            statements.push((
                "INSERT INTO cluster_node_capabilities \
                     (node_id, capability, last_seen_at) VALUES ($1, $2, $3) \
                     ON CONFLICT(node_id, capability) DO UPDATE SET \
                       last_seen_at = excluded.last_seen_at"
                    .to_owned(),
                params!(
                    inner.identity.node_id.as_str(),
                    LEARNER_LIFECYCLE_CAPABILITY,
                    now
                ),
            ));
        }
        if maintenance_requested {
            statements.push((
                "UPDATE cluster_node_maintenance SET acknowledged_at = COALESCE(acknowledged_at, $2) \
                     WHERE node_id = $1"
                    .to_owned(),
                params!(inner.identity.node_id.as_str(), now),
            ));
        }
        statements.push((
            "DELETE FROM cluster_node_join_staging WHERE node_id = $1".to_owned(),
            params!(inner.identity.node_id.as_str()),
        ));
        statements.push((
            "DELETE FROM cluster_node_heartbeat_intents WHERE node_id = $1 \
                     AND last_seen_at = $2"
                .to_owned(),
            params!(inner.identity.node_id.as_str(), now),
        ));
        statements.push((
            "DELETE FROM cluster_node_maintenance_heartbeat_intents WHERE node_id = $1 \
                     AND last_seen_at = $2"
                .to_owned(),
            params!(inner.identity.node_id.as_str(), now),
        ));
        inner
            .client
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        // Initial bootstrap may create the active node row in this very
        // transaction. Refresh once more so HTTP starts with an authoritative
        // answer instead of waiting one heartbeat interval.
        self.refresh_local_route_admission(inner).await?;
        self.refresh_local_maintenance(inner).await.map(|_| ())
    }

    async fn refresh_local_maintenance(
        &self,
        inner: &ReplicatedMembership,
    ) -> Result<bool, MembershipError> {
        // Publish once, at the end, for the same reason the serving role
        // does: storing `true` up front held the node in maintenance for as
        // long as the read below took, on every heartbeat, on a node nobody
        // had asked to drain. This gate is the first one
        // `cluster_capacity_gate` consults and its allow-list is an explicit
        // enumeration, so that window refused more routes than the fenced one
        // — every ordinary API call the web app makes among them. A transient
        // read error still fails closed: the guard publishes `true` on every
        // path that does not reach its commit.
        let publication = LocalMaintenancePublication::new(&inner.local_maintenance);
        let requested = inner
            .client
            .query_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM cluster_node_maintenance WHERE node_id = $1",
                params!(inner.identity.node_id.as_str()),
            )
            .await?
            .first()
            .is_some_and(|row| row.count == 1);
        publication.commit(requested);
        Ok(requested)
    }

    async fn refresh_local_route_admission(
        &self,
        inner: &ReplicatedMembership,
    ) -> Result<LocalServingRole, MembershipError> {
        // Publish once, at the end. Storing `Fenced` here fenced the request
        // path for as long as the two reads below took — on every heartbeat,
        // on a healthy node — because `cluster_capacity_gate` loads this slot
        // per request and answers 503 for everything but `/healthz` and
        // `/metrics`. The removal protocol's target-side barrier does not
        // need that window: a coordinator observes this node's heartbeat
        // write, which lands after this call returns, so the property it
        // relies on is that the published role reflects a read taken before
        // that write. One publication at the end supplies exactly that, and
        // the guard still fails closed on every error, panic, and
        // cancellation path.
        let publication = LocalServingRolePublication::new(&inner.local_serving_role);
        let active = inner
            .client
            .query_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM cluster_nodes \
                 WHERE node_id = $1 AND removed_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals \
                     WHERE node_id = $1)",
                params!(inner.identity.node_id.as_str()),
            )
            .await?
            .first()
            .is_some_and(|row| row.count == 1);
        let role = if !active || !inner.local_metrics.snapshot().running {
            LocalServingRole::Fenced
        } else {
            let metrics = inner.client.metrics_db().await?;
            let is_member = metrics
                .membership_config
                .nodes()
                .any(|(raft_id, _)| *raft_id == inner.identity.raft_id);
            let is_voter = metrics
                .membership_config
                .voter_ids()
                .any(|raft_id| raft_id == inner.identity.raft_id);
            match (is_member, is_voter) {
                (true, true) => LocalServingRole::Voter,
                (true, false) => LocalServingRole::Learner,
                _ => LocalServingRole::Fenced,
            }
        };
        let published_role = if role == LocalServingRole::Voter
            && inner.role.is_learner()
            && !inner.local_voter_role_persisted.load(Ordering::Acquire)
        {
            LocalServingRole::Fenced
        } else {
            role
        };
        publication.commit(published_role);
        Ok(role)
    }

    async fn refresh_voter_storage_probe(
        &self,
        inner: &ReplicatedMembership,
    ) -> Result<StorageDurabilityObservation, MembershipError> {
        let mut observation = inner.voter_storage_probe.lock().await;
        if observation.checked_at.elapsed() >= STORAGE_DURABILITY_PROBE_INTERVAL {
            let root = inner.storage_root.clone();
            let successful =
                tokio::task::spawn_blocking(move || voter_storage_durability_probe(&root))
                    .await
                    .map_err(|error| MembershipError::Internal(error.to_string()))?;
            *observation = StorageDurabilityObservation {
                successful,
                observed_at: unix_ms()?,
                checked_at: tokio::time::Instant::now(),
            };
        }
        Ok(*observation)
    }

    async fn persist_local_promoted_voter_role(
        &self,
        inner: &ReplicatedMembership,
    ) -> Result<(), MembershipError> {
        if inner.local_voter_role_persisted.load(Ordering::Acquire) {
            return Ok(());
        }
        let data_dir = inner.storage_root.clone();
        tokio::task::spawn_blocking(move || {
            super::migration::persist_promoted_voter_role(&data_dir)
        })
        .await
        .map_err(|error| MembershipError::Internal(error.to_string()))?
        .map_err(|error| MembershipError::Internal(error.to_string()))?;
        inner
            .local_voter_role_persisted
            .store(true, Ordering::Release);
        Ok(())
    }

    /// Publish the public half of this node's durable activity authority.
    ///
    /// A key is immutable for a node id. Losing the private file is therefore
    /// a fail-closed recovery event, not permission to replace replicated
    /// authority; operators must recover the data directory or rejoin with a
    /// new node identity.
    async fn publish_activity_signing_key(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let public_key = inner.activity_signing_key.public_key_hex();
        let changed = inner
            .client
            .execute(
                "INSERT INTO cluster_node_activity_keys (node_id, public_key) \
                 SELECT $1, $2 WHERE EXISTS (\
                   SELECT 1 FROM cluster_nodes node \
                   WHERE node.node_id = $1 AND node.removed_at IS NULL \
                     AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                       WHERE removal.node_id = node.node_id)) \
                 ON CONFLICT(node_id) DO UPDATE SET public_key = excluded.public_key \
                 WHERE cluster_node_activity_keys.public_key = excluded.public_key",
                params!(inner.identity.node_id.as_str(), public_key.as_str()),
            )
            .await?;
        if changed != 1 {
            return Err(MembershipError::Internal(
                "local activity signing key does not match immutable cluster authority".to_owned(),
            ));
        }
        Ok(())
    }

    /// Refresh the bounded local verifier set from replicated membership.
    /// Invalid signatures never cross the consensus boundary; this cache is
    /// only a cheap prefilter, while the final live-voter decision remains a
    /// consistent read below.
    async fn refresh_activity_public_keys(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_map::<ActivityPublicKeyRow, _>(
                "SELECT key.node_id, key.public_key \
                 FROM cluster_node_activity_keys key \
                 JOIN cluster_nodes node ON node.node_id = key.node_id \
                 WHERE node.removed_at IS NULL AND node.node_id != $1 \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id) \
                 ORDER BY node.raft_id LIMIT $2",
                params!(inner.identity.node_id.as_str(), MAX_ACTIVITY_PEERS as i64),
            )
            .await?;
        let keys = rows
            .into_iter()
            .filter_map(|row| {
                let key = hex::decode(row.public_key).ok()?;
                (key.len() == 32).then_some((row.node_id, key))
            })
            .collect::<BTreeMap<_, _>>();
        inner
            .activity_auth_admission
            .lock()
            .map_err(|_| {
                MembershipError::Internal("activity admission lock was poisoned".to_owned())
            })?
            .retain(|node_id, _| keys.contains_key(node_id));
        inner
            .internal_auth_admission
            .lock()
            .map_err(|_| {
                MembershipError::Internal("internal peer admission lock was poisoned".to_owned())
            })?
            .retain(|node_id, _| keys.contains_key(node_id));
        inner
            .internal_auth_replays
            .lock()
            .map_err(|_| {
                MembershipError::Internal("internal peer replay lock was poisoned".to_owned())
            })?
            .retain(|node_id, _| keys.contains_key(node_id));
        inner
            .internal_read_replays
            .lock()
            .map_err(|_| {
                MembershipError::Internal("internal read replay lock was poisoned".to_owned())
            })?
            .retain(|node_id, _| keys.contains_key(node_id));
        inner
            .internal_read_authority
            .lock()
            .await
            .retain(|node_id, _| keys.contains_key(node_id));
        *inner.activity_public_keys.lock().map_err(|_| {
            MembershipError::Internal("activity public-key lock was poisoned".to_owned())
        })? = keys;
        Ok(())
    }

    /// Publish this process-constant address once at startup. Rewriting it on
    /// every ten-second liveness beat would double steady Raft traffic while
    /// carrying no new information.
    async fn publish_http_url(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        // The ownership check belongs in the serialized Raft statement rather
        // than a UNIQUE table constraint. Existing M3 clusters already have
        // this table without that constraint, and may temporarily contain the
        // former shared join URL in every row while voters roll forward. A
        // node may replace its own legacy value, but may never claim a URL
        // currently published by another node. A still-redeeming token freezes
        // the durable origin chosen during redemption; after finalization the
        // active node identity may atomically publish an operator readdress.
        let transaction = inner
            .client
            .txn(vec![
                (
                    "INSERT INTO cluster_node_http (node_id, public_http_url) \
                     SELECT $1, $2 WHERE EXISTS (\
                       SELECT 1 FROM cluster_nodes self_node \
                       WHERE self_node.node_id = $1 AND self_node.removed_at IS NULL \
                         AND NOT EXISTS (SELECT 1 FROM cluster_node_removals self_removal \
                           WHERE self_removal.node_id = self_node.node_id)) \
                     AND (NOT EXISTS (\
                       SELECT 1 FROM cluster_node_http_claims claim \
                       WHERE claim.node_id = $1 AND claim.public_http_url != $2) \
                       OR NOT EXISTS (SELECT 1 FROM cluster_join_tokens token \
                         WHERE token.node_id = $1 AND token.state = 'redeeming')) \
                     AND NOT EXISTS (\
                       SELECT 1 FROM cluster_node_http AS owner_http \
                       WHERE owner_http.public_http_url = $2 \
                         AND owner_http.node_id != $1 \
                         AND EXISTS (\
                           SELECT 1 FROM cluster_nodes AS owner_node \
                           WHERE owner_node.node_id = owner_http.node_id \
                             AND owner_node.removed_at IS NULL \
                             AND NOT EXISTS (\
                               SELECT 1 FROM cluster_node_removals AS removing \
                               WHERE removing.node_id = owner_node.node_id))) \
                     ON CONFLICT(node_id) DO UPDATE SET \
                       public_http_url = excluded.public_http_url \
                     RETURNING node_id"
                        .to_owned(),
                    params!(inner.identity.node_id.as_str(), inner.artwork_http.as_str()),
                ),
                (
                    "INSERT INTO cluster_node_http_claims (node_id, public_http_url) \
                     VALUES ($1, $2) ON CONFLICT(node_id) DO UPDATE SET \
                       public_http_url = excluded.public_http_url"
                        .to_owned(),
                    vec![
                        Param::StmtOutputNamed(0, "node_id".into()),
                        Param::Text(inner.artwork_http.clone()),
                    ],
                ),
            ])
            .await;
        match transaction {
            Ok(results) => {
                results.into_iter().collect::<Result<Vec<_>, _>>()?;
            }
            Err(error) if error.to_string().contains("StmtIndex(") => {
                return Err(MembershipError::Internal(format!(
                    "artwork URL {} conflicts with an existing node claim",
                    inner.artwork_http
                )));
            }
            Err(error) => return Err(error.into()),
        }
        self.upsert_hostname(&inner.identity.node_id, &inner.local_hostname)
            .await
    }

    /// Claim the one cluster-wide provider/source repair for an item.
    ///
    /// Only the current Raft leader may submit the conditional write. This
    /// makes one clock and one process the arbitration point instead of asking
    /// replicas with different clocks and apply positions to pick a caller.
    /// After an election, the successor observes the older term and waits one
    /// complete local monotonic lease before fencing it. A restarted successor
    /// waits again, which is conservative but cannot overlap an old repair.
    pub async fn observe_artwork_source_repair(
        &self,
        item_id: i64,
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        if !self.local_node_is_active_voter().await? {
            return Err(MembershipError::LocalNodeNotActive);
        }
        let metrics = inner.client.metrics_db().await?;
        if metrics.current_leader != Some(inner.identity.raft_id)
            || metrics
                .millis_since_quorum_ack
                .is_none_or(|age| age > ARTWORK_LEADER_QUORUM_FRESH_MS)
        {
            return Ok(false);
        }
        let leader_term = i64::try_from(metrics.current_term)
            .map_err(|_| MembershipError::Internal("Raft term overflow".to_owned()))?;
        let existing = inner
            .client
            .query_consistent_map::<ArtworkRepairLeaseRow, _>(
                "SELECT owner_node_id, leader_term, generation FROM cluster_artwork_repairs \
                 WHERE item_id = $1",
                params!(item_id),
            )
            .await?;
        let Some(previous) = existing.first() else {
            return Ok(false);
        };
        if previous.leader_term > leader_term
            || (previous.leader_term == leader_term
                && previous.owner_node_id != inner.identity.node_id)
        {
            return Ok(false);
        }
        let mut observations = inner.artwork_claim_observed_at.lock().map_err(|_| {
            MembershipError::Internal("artwork claim observation lock was poisoned".to_owned())
        })?;
        match observations.get(&item_id) {
            Some((term, generation, _))
                if *term == previous.leader_term && *generation == previous.generation => {}
            _ => {
                observations.insert(
                    item_id,
                    (previous.leader_term, previous.generation, Instant::now()),
                );
            }
        }
        Ok(true)
    }

    /// Claim a repair generation after the receiver-local observation window.
    /// Callers that only need to establish that window must use
    /// [`Self::observe_artwork_source_repair`], which cannot reach the CAS.
    pub async fn claim_artwork_source_repair(
        &self,
        item_id: i64,
        lease: Duration,
    ) -> Result<Option<ArtworkRepairClaim>, MembershipError> {
        let inner = self.replicated_inner()?;
        let claim_started = Instant::now();
        let claim_deadline = claim_started + lease;
        if !self.local_node_is_active_voter().await? {
            return Err(MembershipError::LocalNodeNotActive);
        }
        let metrics = inner.client.metrics_db().await?;
        if metrics.current_leader != Some(inner.identity.raft_id)
            || metrics
                .millis_since_quorum_ack
                .is_none_or(|age| age > ARTWORK_LEADER_QUORUM_FRESH_MS)
        {
            return Ok(None);
        }
        let leader_term = i64::try_from(metrics.current_term)
            .map_err(|_| MembershipError::Internal("Raft term overflow".to_owned()))?;
        let existing = inner
            .client
            .query_consistent_map::<ArtworkRepairLeaseRow, _>(
                "SELECT owner_node_id, leader_term, generation FROM cluster_artwork_repairs \
                 WHERE item_id = $1",
                params!(item_id),
            )
            .await?;
        let previous = existing.first();
        if let Some(previous) = previous {
            if previous.leader_term > leader_term {
                return Ok(None);
            }
            let mut observations = inner.artwork_claim_observed_at.lock().map_err(|_| {
                MembershipError::Internal("artwork claim observation lock was poisoned".to_owned())
            })?;
            if previous.leader_term == leader_term {
                if previous.owner_node_id != inner.identity.node_id {
                    return Ok(None);
                }
                match observations.get(&item_id) {
                    Some((term, generation, observed_at))
                        if *term == leader_term
                            && *generation == previous.generation
                            && observed_at.elapsed() >= lease => {}
                    Some((term, generation, _))
                        if *term == leader_term && *generation == previous.generation =>
                    {
                        return Ok(None);
                    }
                    _ => {
                        observations
                            .insert(item_id, (leader_term, previous.generation, Instant::now()));
                        return Ok(None);
                    }
                }
            }
            match observations.get(&item_id) {
                Some((term, generation, observed_at))
                    if *term == previous.leader_term
                        && *generation == previous.generation
                        && observed_at.elapsed() >= lease => {}
                Some((term, generation, _))
                    if *term == previous.leader_term && *generation == previous.generation =>
                {
                    return Ok(None);
                }
                _ => {
                    observations.insert(
                        item_id,
                        (previous.leader_term, previous.generation, Instant::now()),
                    );
                    return Ok(None);
                }
            }
        }
        let previous_owner = previous
            .map(|row| row.owner_node_id.as_str())
            .unwrap_or_default();
        let previous_term = previous.map_or(-1, |row| row.leader_term);
        let previous_generation = previous.map_or(0, |row| row.generation);
        let generation = previous_generation.checked_add(1).ok_or_else(|| {
            MembershipError::Internal("artwork repair generation overflow".to_owned())
        })?;
        let changed = inner
            .client
            .execute(
                "INSERT INTO cluster_artwork_repairs \
                 (item_id, owner_node_id, leader_term, generation) \
                 SELECT $1, $2, $3, $4 WHERE EXISTS (\
                   SELECT 1 FROM cluster_nodes \
                   WHERE node_id = $2 AND removed_at IS NULL \
                     AND NOT EXISTS (SELECT 1 FROM cluster_node_removals \
                       WHERE node_id = $2)\
                 ) \
                 ON CONFLICT(item_id) DO UPDATE SET \
                   owner_node_id = excluded.owner_node_id, \
                   leader_term = excluded.leader_term, \
                   generation = excluded.generation \
                 WHERE cluster_artwork_repairs.owner_node_id = $5 \
                   AND cluster_artwork_repairs.leader_term = $6 \
                   AND cluster_artwork_repairs.generation = $7 \
                   AND (cluster_artwork_repairs.leader_term < excluded.leader_term \
                     OR (cluster_artwork_repairs.leader_term = excluded.leader_term \
                       AND cluster_artwork_repairs.owner_node_id = excluded.owner_node_id))",
                params!(
                    item_id,
                    inner.identity.node_id.as_str(),
                    leader_term,
                    generation,
                    previous_owner,
                    previous_term,
                    previous_generation
                ),
            )
            .await?;
        if changed == 1 {
            inner
                .artwork_claim_observed_at
                .lock()
                .map_err(|_| {
                    MembershipError::Internal(
                        "artwork claim observation lock was poisoned".to_owned(),
                    )
                })?
                .insert(item_id, (leader_term, generation, claim_started));
            Ok(Some(ArtworkRepairClaim {
                deadline: claim_deadline,
                fence: ArtworkRepairFence {
                    item_id,
                    owner_node_id: inner.identity.node_id.clone(),
                    leader_term,
                    generation,
                },
            }))
        } else {
            Ok(None)
        }
    }

    /// Whether this process is both present in the committed Raft voter set
    /// and non-tombstoned in replicated membership state.
    pub async fn local_node_is_active_voter(&self) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        if !self.local_node_is_committed_voter().await? {
            return Ok(false);
        }
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM cluster_nodes \
                 WHERE node_id = $1 AND removed_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals \
                     WHERE node_id = $1)",
                params!(inner.identity.node_id.as_str()),
            )
            .await?;
        Ok(rows.first().is_some_and(|row| row.count == 1))
    }

    /// Whether this process may run cluster-wide leader-singleton work.
    ///
    /// Re-derived from live committed membership on every call, never from the
    /// role this process booted with. A learner may be promoted while the
    /// daemon runs, and Hiqlite's `learner_only` startup hint is consulted once
    /// at startup and never again — so a cached answer would either keep a
    /// promoted voter idle or, worse, let a node that was demoted keep running
    /// the jobs the cluster now expects someone else to own.
    ///
    /// An unclustered process is the whole cluster and owns every job. A
    /// clustered process whose membership cannot be read right now is refused:
    /// duplicating a provider pass or a scan is the failure this exists to
    /// prevent, and skipping a tick costs at most one interval.
    pub async fn may_run_cluster_jobs(&self) -> bool {
        if self.inner.is_none() {
            return true;
        }
        if self.local_maintenance_active() {
            return false;
        }
        decide_cluster_job_authority(self.local_node_is_committed_voter().await)
    }

    /// Fast local admission projection. This deliberately does not perform a
    /// quorum read on an HTTP request path; heartbeat acknowledgement is what
    /// proves the process has already refreshed it from local applied state.
    #[must_use]
    pub fn local_maintenance_active(&self) -> bool {
        self.inner
            .as_deref()
            .is_some_and(|inner| inner.local_maintenance.load(Ordering::Acquire))
    }

    pub async fn local_node_is_committed_voter(&self) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        Ok(metrics
            .membership_config
            .voter_ids()
            .any(|raft_id| raft_id == inner.identity.raft_id))
    }

    /// Whether committed membership still lists this node at all.
    ///
    /// Not a synonym for [`Self::local_node_is_committed_voter`]: a learner is
    /// a committed member for as long as it is admitted and never becomes a
    /// voter, and so is a joining voter that Raft has added but not yet
    /// promoted out of its intermediate learner state. Anything that reads
    /// "carries no vote" as "was removed" stops work a healthy member still
    /// has to do.
    pub async fn local_node_is_committed_member(&self) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        let is_member = metrics
            .membership_config
            .nodes()
            .any(|(raft_id, _)| *raft_id == inner.identity.raft_id);
        Ok(is_member)
    }

    /// Effective role for request admission from the heartbeat-refreshed
    /// atomic projection. Membership metrics, local SQL, and promotion-role
    /// persistence all happen before publication, never on the HTTP path.
    pub async fn local_serving_role(&self) -> Result<LocalServingRole, MembershipError> {
        let Some(inner) = self.inner.as_deref() else {
            return Ok(LocalServingRole::Unclustered);
        };
        Ok(LocalServingRole::from_encoded(
            inner.local_serving_role.load(Ordering::Acquire),
        ))
    }

    /// Retire one completed or timed-out provider repair generation. The
    /// conditional increment is itself a Raft command submitted after the
    /// repair work, so an earlier command either lands before retirement or
    /// observes an obsolete generation and becomes a no-op afterwards.
    pub async fn retire_artwork_source_repair(
        &self,
        fence: &ArtworkRepairFence,
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let changed = inner
            .client
            .execute(
                "UPDATE cluster_artwork_repairs SET generation = generation + 1 \
                 WHERE item_id = $1 AND owner_node_id = $2 AND leader_term = $3 \
                   AND generation = $4 AND generation < 9223372036854775807",
                params!(
                    fence.item_id,
                    fence.owner_node_id.as_str(),
                    fence.leader_term,
                    fence.generation
                ),
            )
            .await?;
        Ok(changed == 1)
    }

    /// Ask this node's Raft endpoint to stand for election. Production uses
    /// the same bounded operation during graceful leader leave; the cluster
    /// harness uses it to prove artwork fencing across a live handoff.
    pub async fn trigger_local_election(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        trigger_election(&inner.local.api_address, &inner.secrets.api).await
    }

    /// Acquire the one replicated planned-outage slot before this process
    /// fences any local restart admissions.
    pub async fn acquire_restart_preparation(
        &self,
        node_id: &str,
        duration: Duration,
    ) -> Result<ClusterOperationLease, MembershipError> {
        self.acquire_cluster_operation_lease(node_id, "restart", duration)
            .await
    }

    /// Acquire the same slot for maintenance. The returned proof must be
    /// consumed by `enter_maintenance`; it cannot authorize any other node.
    pub async fn acquire_maintenance_preparation(
        &self,
        node_id: &str,
        duration: Duration,
    ) -> Result<ClusterOperationLease, MembershipError> {
        self.acquire_cluster_operation_lease(node_id, "maintenance", duration)
            .await
    }

    async fn acquire_cluster_operation_lease(
        &self,
        node_id: &str,
        operation: &'static str,
        duration: Duration,
    ) -> Result<ClusterOperationLease, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        let lease_duration = duration.saturating_add(CLUSTER_OPERATION_LEASE_EXPIRY_GRACE);
        let duration_ms = i64::try_from(lease_duration.as_millis()).unwrap_or(i64::MAX);
        let expires_at = now.saturating_add(duration_ms);
        let claim_id = uuid::Uuid::new_v4().to_string();
        let changed = inner
            .client
            .execute(
                ACQUIRE_CLUSTER_OPERATION_LEASE_SQL,
                params!(node_id, operation, claim_id.as_str(), expires_at, now),
            )
            .await?;
        if changed != 1 {
            return Err(MembershipError::ClusterOperationPending);
        }
        Ok(ClusterOperationLease {
            node_id: node_id.to_owned(),
            operation,
            claim_id,
            expires_at_unix_ms: expires_at,
        })
    }

    /// Release one exact failed claim. A successor claim, even for the same
    /// node, cannot be cleared by a delayed failure path.
    pub async fn release_cluster_operation_lease(
        &self,
        lease: &ClusterOperationLease,
    ) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        inner
            .client
            .execute(
                RELEASE_CLUSTER_OPERATION_LEASE_SQL,
                params!(
                    lease.node_id.as_str(),
                    lease.operation,
                    lease.claim_id.as_str()
                ),
            )
            .await?;
        Ok(())
    }

    /// Direct cancellation intentionally means "cancel the current restart
    /// preparation on this node", including a retried request after the
    /// original HTTP response was lost.
    pub async fn release_restart_preparation(&self, node_id: &str) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        inner
            .client
            .execute(RELEASE_RESTART_PREPARATION_SQL, params!(node_id))
            .await?;
        Ok(())
    }

    /// Enter reversible maintenance for one node. Leadership moves first;
    /// only then is the replicated admission fence committed. The target's
    /// next heartbeat acknowledges after its local atomics have observed it.
    pub async fn enter_maintenance(
        &self,
        node_id: &str,
        lease: &ClusterOperationLease,
    ) -> Result<MembershipStatus, MembershipError> {
        let inner = self.replicated_inner()?;
        if lease.node_id != node_id || lease.operation != "maintenance" {
            return Err(MembershipError::MaintenanceConflict(node_id.to_owned()));
        }
        self.require_maintenance_capability().await?;
        let status = self.status().await?;
        let target = status
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .cloned()
            .ok_or(MembershipError::NodeNotFound)?;
        if target.maintenance {
            return Ok(status);
        }
        if status.recovery.required || !status.recovery.quorum_available {
            return Err(MembershipError::ElectionQuorumUnavailable);
        }
        if target.removal_pending || self.lifecycle_operation_pending().await? {
            return Err(MembershipError::MaintenanceConflict(node_id.to_owned()));
        }
        if !maintenance_preserves_quorum(
            target.is_voter,
            target.reachable,
            status.capacity.voting_nodes,
            status.recovery.reachable_voters,
            status.capacity.voting_quorum,
        ) {
            return Err(MembershipError::MaintenanceWouldLoseQuorum(
                node_id.to_owned(),
            ));
        }
        if target.is_leader && status.capacity.voting_nodes > 1 {
            self.handoff_leadership(target.raft_id, &status).await?;
        }
        let now = unix_ms()?;
        let transition = inner
            .client
            .txn(vec![
                (
                    format!(
                        "INSERT INTO cluster_node_maintenance \
                           (node_id, requested_at, acknowledged_at) \
                         SELECT $1, $2, NULL WHERE {} \
                           AND EXISTS (SELECT 1 FROM cluster_nodes node \
                             WHERE node.node_id = $1 AND node.removed_at IS NULL) \
                           AND EXISTS (SELECT 1 FROM cluster_operation_leases lease \
                             WHERE lease.singleton = 1 AND lease.node_id = $1 \
                               AND lease.operation = 'maintenance' \
                               AND lease.claim_id = $3 AND lease.expires_at > $2) \
                           AND NOT EXISTS (SELECT 1 FROM cluster_node_removals) \
                           AND NOT EXISTS (SELECT 1 FROM cluster_node_promotions) \
                           AND NOT EXISTS (SELECT 1 FROM cluster_node_join_staging) \
                           AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
                         ON CONFLICT(node_id) DO NOTHING RETURNING node_id",
                        capability_ready_predicate(NODE_MAINTENANCE_CAPABILITY)
                    ),
                    params!(node_id, now, lease.claim_id.as_str()),
                ),
                (
                    "DELETE FROM cluster_operation_leases \
                     WHERE singleton = 1 AND node_id = $1 \
                       AND operation = 'maintenance' AND claim_id = $2"
                        .to_owned(),
                    vec![
                        Param::StmtOutputNamed(0, "node_id".into()),
                        Param::Text(lease.claim_id.clone()),
                    ],
                ),
            ])
            .await;
        match transition {
            Ok(results) => {
                results.into_iter().collect::<Result<Vec<_>, _>>()?;
            }
            Err(error) if error.to_string().contains("StmtIndex(") => {
                return Err(MembershipError::MaintenanceConflict(node_id.to_owned()));
            }
            Err(error) => return Err(error.into()),
        }
        if node_id == inner.identity.node_id {
            self.refresh_local_maintenance(inner).await?;
            self.heartbeat().await?;
        }
        self.status().await
    }

    /// Clear maintenance only after the target is back, fresh, and caught up.
    /// This keeps a rebooting node fenced even if an operator double-clicks.
    pub async fn exit_maintenance(
        &self,
        node_id: &str,
    ) -> Result<MembershipStatus, MembershipError> {
        let inner = self.replicated_inner()?;
        self.require_maintenance_capability().await?;
        let mut status = self.status().await?;
        let mut target = status
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .cloned()
            .ok_or(MembershipError::NodeNotFound)?;
        if !target.maintenance {
            return Ok(status);
        }
        if target.is_leader && status.capacity.voting_nodes > 1 {
            self.handoff_leadership(target.raft_id, &status).await?;
            status = self.status().await?;
            target = status
                .nodes
                .iter()
                .find(|node| node.node_id == node_id)
                .cloned()
                .ok_or(MembershipError::NodeNotFound)?;
        }
        if !target.maintenance_ready {
            return Err(MembershipError::MaintenanceResumeUnsafe(node_id.to_owned()));
        }
        let now = unix_ms()?;
        let cutoff = reachable_after(now);
        let changed = inner
            .client
            .execute(
                EXIT_MAINTENANCE_SQL,
                params!(node_id, cutoff, NODE_MAINTENANCE_CAPABILITY, now),
            )
            .await?;
        if changed != 1 {
            return Err(MembershipError::MaintenanceResumeUnsafe(node_id.to_owned()));
        }
        if node_id == inner.identity.node_id {
            self.refresh_local_maintenance(inner).await?;
        }
        self.status().await
    }

    /// Ask one healthy caught-up follower to campaign, then require a stable
    /// quorum observation of a different leader before reporting success.
    pub async fn force_election(&self) -> Result<MembershipStatus, MembershipError> {
        // A leaderless but otherwise reachable voter set is the main reason
        // this operation exists. Its preflights therefore use local applied
        // state; Raft itself still requires a real quorum before the campaign
        // can elect or commit anything.
        self.require_maintenance_capability_local().await?;
        let status = self.status().await?;
        if !status.recovery.quorum_available {
            return Err(MembershipError::ElectionQuorumUnavailable);
        }
        if self.lifecycle_operation_pending_local().await?
            || status.nodes.iter().any(|node| node.maintenance)
        {
            return Err(MembershipError::ElectionLifecyclePending);
        }
        let leader = status.nodes.iter().find(|node| node.is_leader);
        if let Some(leader) = leader {
            self.force_leader_change(leader.raft_id, &status).await?;
        } else {
            self.campaign_without_leader(&status).await?;
        }
        self.status().await
    }

    async fn require_maintenance_capability(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                format!(
                    "SELECT CASE WHEN {} THEN 1 ELSE 0 END AS count",
                    capability_ready_predicate(NODE_MAINTENANCE_CAPABILITY)
                ),
                params!(),
            )
            .await?;
        if rows.first().is_some_and(|row| row.count == 1) {
            Ok(())
        } else {
            Err(MembershipError::MembershipUpgradeRequired)
        }
    }

    async fn require_maintenance_capability_local(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_map::<CountRow, _>(
                format!(
                    "SELECT CASE WHEN {} THEN 1 ELSE 0 END AS count",
                    capability_ready_predicate(NODE_MAINTENANCE_CAPABILITY)
                ),
                params!(),
            )
            .await?;
        if rows.first().is_some_and(|row| row.count == 1) {
            Ok(())
        } else {
            Err(MembershipError::MembershipUpgradeRequired)
        }
    }

    async fn lifecycle_operation_pending(&self) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                "SELECT (SELECT COUNT(*) FROM cluster_node_removals) \
                   + (SELECT COUNT(*) FROM cluster_node_promotions) \
                   + (SELECT COUNT(*) FROM cluster_node_join_staging) AS count",
                params!(),
            )
            .await?;
        Ok(rows.first().is_some_and(|row| row.count > 0))
    }

    async fn lifecycle_operation_pending_local(&self) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_map::<CountRow, _>(
                "SELECT (SELECT COUNT(*) FROM cluster_node_removals) \
                   + (SELECT COUNT(*) FROM cluster_node_promotions) \
                   + (SELECT COUNT(*) FROM cluster_node_join_staging) AS count",
                params!(),
            )
            .await?;
        Ok(rows.first().is_some_and(|row| row.count > 0))
    }

    async fn maintenance_operation_pending(&self) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                "SELECT (SELECT COUNT(*) FROM cluster_node_maintenance) \
                   + (SELECT COUNT(*) FROM cluster_operation_leases) AS count",
                params!(),
            )
            .await?;
        Ok(rows.first().is_some_and(|row| row.count > 0))
    }

    async fn handoff_leadership(
        &self,
        departed: u64,
        status: &MembershipStatus,
    ) -> Result<u64, MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        let remaining = metrics
            .membership_config
            .voter_ids()
            .filter(|raft_id| *raft_id != departed)
            .collect::<BTreeSet<_>>();
        let survivor_nodes = metrics
            .membership_config
            .membership()
            .nodes()
            .filter(|(raft_id, _)| remaining.contains(raft_id))
            .map(|(raft_id, node)| (*raft_id, node.addr_api.clone()))
            .collect::<Vec<_>>();
        let candidate = status.nodes.iter().find(|node| {
            remaining.contains(&node.raft_id)
                && node.is_voter
                && node.reachable
                && !node.maintenance
                && !node.removal_pending
                && node.apply_lag_entries == Some(0)
        });
        let candidate = candidate.ok_or(MembershipError::ElectionCandidateUnavailable)?;
        let candidate_api = survivor_nodes
            .iter()
            .find(|(raft_id, _)| *raft_id == candidate.raft_id)
            .map(|(_, api)| api.as_str())
            .ok_or(MembershipError::ElectionCandidateUnavailable)?;
        trigger_election(candidate_api, &inner.secrets.api).await?;
        self.await_survivor_leader(departed, &remaining, &survivor_nodes, &inner.secrets.api)
            .await
    }

    async fn campaign_without_leader(
        &self,
        status: &MembershipStatus,
    ) -> Result<u64, MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        let voters = metrics
            .membership_config
            .voter_ids()
            .collect::<BTreeSet<_>>();
        let voter_nodes = metrics
            .membership_config
            .membership()
            .nodes()
            .filter(|(raft_id, _)| voters.contains(raft_id))
            .map(|(raft_id, node)| (*raft_id, node.addr_api.clone()))
            .collect::<Vec<_>>();
        let candidate = status.nodes.iter().find(|node| {
            node.is_voter
                && node.reachable
                && !node.maintenance
                && !node.removal_pending
                && node.apply_lag_entries == Some(0)
        });
        let candidate = candidate.ok_or(MembershipError::ElectionCandidateUnavailable)?;
        let candidate_api = voter_nodes
            .iter()
            .find(|(raft_id, _)| *raft_id == candidate.raft_id)
            .map(|(_, api)| api.as_str())
            .ok_or(MembershipError::ElectionCandidateUnavailable)?;
        trigger_election(candidate_api, &inner.secrets.api).await?;
        self.await_survivor_leader(0, &voters, &voter_nodes, &inner.secrets.api)
            .await
    }

    async fn force_leader_change(
        &self,
        departed: u64,
        status: &MembershipStatus,
    ) -> Result<u64, MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        let voters = metrics
            .membership_config
            .voter_ids()
            .collect::<BTreeSet<_>>();
        let voter_nodes = metrics
            .membership_config
            .membership()
            .nodes()
            .filter(|(raft_id, _)| voters.contains(raft_id))
            .map(|(raft_id, node)| (*raft_id, node.addr_api.clone()))
            .collect::<Vec<_>>();
        let candidate = status.nodes.iter().find(|node| {
            node.raft_id != departed
                && node.is_voter
                && node.reachable
                && !node.maintenance
                && !node.removal_pending
                && node.apply_lag_entries == Some(0)
        });
        let candidate = candidate.ok_or(MembershipError::ElectionCandidateUnavailable)?;
        let candidate_api = voter_nodes
            .iter()
            .find(|(raft_id, _)| *raft_id == candidate.raft_id)
            .map(|(_, api)| api.as_str())
            .ok_or(MembershipError::ElectionCandidateUnavailable)?;
        trigger_election(candidate_api, &inner.secrets.api).await?;
        // Unlike maintenance, the old leader is not about to leave. Its vote
        // and observation count toward the unchanged cluster majority.
        self.await_survivor_leader(departed, &voters, &voter_nodes, &inner.secrets.api)
            .await
    }

    /// Public HTTP bases for currently reachable peers.
    ///
    /// Originally node-to-node only, for recovering materialized bytes such as
    /// artwork. Since the media pool's client failover it is also what
    /// `GET /api/v1/cluster/ingress` hands to a **signed-in** client, so these
    /// values are no longer purely internal — an operator's
    /// `cluster.artwork_url` must name an address a household client can
    /// actually reach. The ordinary membership status still omits addresses,
    /// and no unauthenticated route exposes them.
    pub async fn reachable_peer_http_urls(&self) -> Result<Vec<String>, MembershipError> {
        let Some(inner) = self.inner.as_ref() else {
            return Ok(Vec::new());
        };
        let reachable_after = reachable_after(unix_ms()?);
        let rows = inner
            .client
            .query_map::<HttpUrlRow, _>(
                "SELECT http.public_http_url \
                 FROM cluster_node_http http \
                 JOIN cluster_nodes node ON node.node_id = http.node_id \
                 WHERE node.removed_at IS NULL AND node.node_id != $1 \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id) \
                   AND node.last_seen_at >= $2 \
                 ORDER BY node.last_seen_at DESC, node.raft_id",
                params!(inner.identity.node_id.as_str(), reachable_after),
            )
            .await?;
        Ok(rows.into_iter().map(|row| row.public_http_url).collect())
    }

    /// Sign one artwork request with the established rolling-compatible HMAC
    /// wire. Activity uses separate per-node authority below; changing this
    /// already-deployed protocol requires its own negotiated transition.
    pub fn artwork_peer_auth(&self, filename: &str) -> Result<ArtworkPeerAuth, MembershipError> {
        let inner = self.replicated_inner()?;
        let timestamp_ms = unix_ms()?;
        let message = artwork_auth_message(&inner.identity.node_id, timestamp_ms, filename);
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(inner.secrets.api.as_bytes())
            .map_err(|error| MembershipError::Internal(error.to_string()))?;
        mac.update(message.as_bytes());
        Ok(ArtworkPeerAuth {
            node_id: inner.identity.node_id.clone(),
            timestamp_ms,
            signature: hex::encode(mac.finalize().into_bytes()),
        })
    }

    /// Verify the established artwork proof and the sender's live membership.
    pub async fn verify_artwork_peer_auth(
        &self,
        filename: &str,
        auth: &ArtworkPeerAuth,
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        if now.abs_diff(auth.timestamp_ms) > ARTWORK_AUTH_WINDOW_MS as u64 {
            return Ok(false);
        }
        let signature = match hex::decode(&auth.signature) {
            Ok(signature) => signature,
            Err(_) => return Ok(false),
        };
        let message = artwork_auth_message(&auth.node_id, auth.timestamp_ms, filename);
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(inner.secrets.api.as_bytes())
            .map_err(|error| MembershipError::Internal(error.to_string()))?;
        mac.update(message.as_bytes());
        if mac.verify_slice(&signature).is_err() {
            return Ok(false);
        }
        let reachable_after = reachable_after(now);
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM cluster_nodes \
                 WHERE node_id = $1 AND removed_at IS NULL AND last_seen_at >= $2 \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals \
                     WHERE node_id = $1)",
                params!(auth.node_id.as_str(), reachable_after),
            )
            .await?;
        Ok(rows.first().is_some_and(|row| row.count == 1))
    }

    /// Resolve peer daemon endpoints without widening the public node status.
    pub async fn activity_peers(&self) -> Result<Vec<ActivityPeer>, MembershipError> {
        let Some(inner) = self.inner.as_deref() else {
            return Ok(Vec::new());
        };
        let now = unix_ms()?;
        let voters = inner
            .client
            .metrics_db()
            .await?
            .membership_config
            .voter_ids()
            .take(MAX_ACTIVITY_PEERS.saturating_add(1))
            .collect::<BTreeSet<_>>();
        let rows = inner
            .client
            .query_map::<ActivityPeerRow, _>(
                "SELECT node.node_id, node.raft_id, node.last_seen_at, \
                        http.public_http_url \
                 FROM cluster_nodes node \
                 LEFT JOIN cluster_node_http http ON http.node_id = node.node_id \
                 WHERE node.node_id != $1 AND node.removed_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id) \
                 ORDER BY node.raft_id LIMIT $2",
                params!(inner.identity.node_id.as_str(), MAX_ACTIVITY_PEERS as i64),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter(|row| voters.contains(&row.raft_id))
            .take(MAX_ACTIVITY_PEERS)
            .map(|row| ActivityPeer {
                http_base: row.http_base,
                node_id: row.node_id,
                reachable: node_is_reachable(now, row.last_seen_at),
            })
            .collect())
    }

    /// Machine names for the live roster, keyed by node id.
    ///
    /// Deliberately narrower than [`Self::status`]: a caller that only wants
    /// to put a name on a node id should not pay for Raft metrics, protocol
    /// status and five joins. The activity page polls this every few seconds.
    ///
    /// A node whose name could not be derived is *absent* from the map rather
    /// than present as [`UNKNOWN_HOSTNAME`], so a caller that already holds a
    /// stable node id shows that instead of a sentinel that names nothing.
    /// The local node answers from memory: its replicated row is written by
    /// the heartbeat, so a table read alone would leave this node nameless for
    /// the first heartbeat interval after start.
    pub async fn node_hostnames(&self) -> Result<BTreeMap<String, String>, MembershipError> {
        let Some(inner) = self.inner.as_deref() else {
            return Ok(BTreeMap::new());
        };
        let rows = inner
            .client
            .query_map::<NodeHostnameRow, _>(
                "SELECT node.node_id, node.api_address, \
                        COALESCE(host.hostname, '') AS hostname \
                 FROM cluster_nodes node \
                 LEFT JOIN cluster_node_hostnames host ON host.node_id = node.node_id \
                 WHERE node.removed_at IS NULL",
                params!(),
            )
            .await?;
        Ok(roster_hostnames(
            rows,
            &inner.identity.node_id,
            &inner.local_hostname,
        ))
    }

    /// Resolve the directly observable committed members for one bounded
    /// operations-status refresh.
    ///
    /// Unlike media placement this deliberately retains stale/unready rows:
    /// the aggregator must render silence as unreachable, not quietly omit it.
    pub async fn operations_peers(&self) -> Result<Vec<ActivityPeer>, MembershipError> {
        let Some(inner) = self.inner.as_deref() else {
            return Ok(Vec::new());
        };
        let now = unix_ms()?;
        let members = inner
            .client
            .metrics_db()
            .await?
            .membership_config
            .nodes()
            .map(|(raft_id, _)| *raft_id)
            .take(MAX_OPERATIONS_PEERS.saturating_add(2))
            .collect::<BTreeSet<_>>();
        let rows = inner
            .client
            .query_map::<ActivityPeerRow, _>(
                "SELECT node.node_id, node.raft_id, node.last_seen_at, \
                        http.public_http_url \
                 FROM cluster_nodes node \
                 LEFT JOIN cluster_node_http http ON http.node_id = node.node_id \
                 WHERE node.node_id != $1 AND node.removed_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id) \
                 ORDER BY node.raft_id LIMIT $2",
                params!(inner.identity.node_id.as_str(), MAX_OPERATIONS_PEERS as i64),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter(|row| members.contains(&row.raft_id))
            .take(MAX_OPERATIONS_PEERS)
            .map(|row| ActivityPeer {
                http_base: row.http_base,
                node_id: row.node_id,
                reachable: node_is_reachable(now, row.last_seen_at),
            })
            .collect())
    }

    /// Ready media-capacity targets, including committed learners.
    ///
    /// Activity aggregation and rollout unanimity deliberately remain
    /// voter-only. Placement is different: a learner is useful precisely as a
    /// non-voting node-local worker. The replicated removal fence ejects it
    /// from this directory before owned work is settled, and the progress row
    /// keeps a lagged learner out even while its heartbeat is fresh.
    pub async fn media_peers(&self) -> Result<Vec<ActivityPeer>, MembershipError> {
        let Some(inner) = self.inner.as_deref() else {
            return Ok(Vec::new());
        };
        let now = unix_ms()?;
        let members = inner
            .client
            .metrics_db()
            .await?
            .membership_config
            .nodes()
            .map(|(raft_id, _)| *raft_id)
            .take(MAX_ACTIVITY_PEERS.saturating_add(1))
            .collect::<BTreeSet<_>>();
        let rows = inner
            .client
            .query_map::<MediaPeerRow, _>(
                "SELECT node.node_id, node.raft_id, node.last_seen_at, \
                        http.public_http_url, \
                        COALESCE(progress.bounded_read_ready, 0) AS bounded_read_ready, \
                        progress.observed_at \
                 FROM cluster_nodes node \
                 LEFT JOIN cluster_node_http http ON http.node_id = node.node_id \
                 LEFT JOIN cluster_node_progress progress ON progress.node_id = node.node_id \
                 WHERE node.node_id != $1 AND node.removed_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id) \
                 ORDER BY node.raft_id LIMIT $2",
                params!(inner.identity.node_id.as_str(), MAX_ACTIVITY_PEERS as i64),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter(|row| members.contains(&row.raft_id))
            .filter(|row| {
                row.bounded_read_ready
                    && node_is_reachable(now, row.observed_at.unwrap_or_default())
            })
            .take(MAX_ACTIVITY_PEERS)
            .map(|row| ActivityPeer {
                http_base: row.http_base,
                node_id: row.node_id,
                reachable: node_is_reachable(now, row.last_seen_at),
            })
            .collect())
    }

    /// Number of committed voters, including this node. Media rollout gates
    /// compare this with the bounded peer directory so a missing, legacy, or
    /// over-limit voter fails closed instead of being silently omitted.
    pub async fn activity_voter_count(&self) -> Result<usize, MembershipError> {
        let Some(inner) = self.inner.as_deref() else {
            return Ok(1);
        };
        Ok(inner
            .client
            .metrics_db()
            .await?
            .membership_config
            .voter_ids()
            .take(MAX_ACTIVITY_PEERS.saturating_add(2))
            .count())
    }

    /// Sign one short-lived, sender-and-target-bound activity request with
    /// this node's private key.
    pub fn sign_activity_request(
        &self,
        target_node_id: &str,
        timestamp_ms: i64,
    ) -> Result<ActivityPeerAuth, MembershipError> {
        let inner = self.replicated_inner()?;
        let message = activity_auth_message(&inner.identity.node_id, target_node_id, timestamp_ms);
        Ok(ActivityPeerAuth {
            node_id: inner.identity.node_id.clone(),
            target_node_id: target_node_id.to_owned(),
            timestamp_ms,
            signature: inner.activity_signing_key.sign_hex(&message),
        })
    }

    /// Authenticate the per-node proof and confirm that its sender is still a
    /// live, non-removed committed voter.
    pub async fn authorize_activity_request(
        &self,
        auth: &ActivityPeerAuth,
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        if auth.target_node_id != inner.identity.node_id
            || now.abs_diff(auth.timestamp_ms) > ACTIVITY_AUTH_WINDOW_MS as u64
            || auth.node_id == auth.target_node_id
            || auth.node_id.len() > MAX_PEER_NODE_ID_BYTES
            || auth.target_node_id.len() > MAX_PEER_NODE_ID_BYTES
            || auth.signature.len() != ED25519_SIGNATURE_HEX_BYTES
        {
            return Ok(false);
        }
        let signature = match hex::decode(&auth.signature) {
            Ok(signature) => signature,
            Err(_) => return Ok(false),
        };
        let message = activity_auth_message(&auth.node_id, &auth.target_node_id, auth.timestamp_ms);
        if !self
            .activity_signature_is_valid(&auth.node_id, &message, &signature)
            .await?
            || !self.admit_activity_authority_check(&auth.node_id)?
        {
            return Ok(false);
        }
        self.verify_live_activity_authority(&auth.node_id, now)
            .await
    }

    /// Sign an exact internal peer HTTP request with this node's durable key.
    pub fn sign_internal_peer_request(
        &self,
        target_node_id: &str,
        timestamp_ms: i64,
        nonce: &str,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<InternalPeerAuth, MembershipError> {
        let inner = self.replicated_inner()?;
        let message = internal_peer_auth_message(
            &inner.identity.node_id,
            target_node_id,
            timestamp_ms,
            nonce,
            method,
            path,
            body,
        )
        .ok_or_else(|| MembershipError::Internal("invalid internal peer route".to_owned()))?;
        Ok(InternalPeerAuth {
            node_id: inner.identity.node_id.clone(),
            target_node_id: target_node_id.to_owned(),
            timestamp_ms,
            nonce: nonce.to_owned(),
            signature: inner.activity_signing_key.sign_hex(&message),
        })
    }

    /// Authenticate one exact internal request and its live voter authority.
    pub async fn authorize_internal_peer_request(
        &self,
        auth: &InternalPeerAuth,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        if auth.target_node_id != inner.identity.node_id
            || now.abs_diff(auth.timestamp_ms) > ACTIVITY_AUTH_WINDOW_MS as u64
            || auth.node_id == auth.target_node_id
            || auth.node_id.len() > MAX_PEER_NODE_ID_BYTES
            || auth.target_node_id.len() > MAX_PEER_NODE_ID_BYTES
            || !canonical_internal_auth_nonce(&auth.nonce)
            || auth.signature.len() != ED25519_SIGNATURE_HEX_BYTES
        {
            return Ok(false);
        }
        // Admission is a real global rate bucket rather than a concurrency
        // semaphore: cached-key verification completes in one executor poll
        // and would otherwise release a permit before peers can contend.
        if !admit_internal_preverification(&inner.internal_preverify_admission)? {
            return Ok(false);
        }
        let Some(message) = internal_peer_auth_message(
            &auth.node_id,
            &auth.target_node_id,
            auth.timestamp_ms,
            &auth.nonce,
            method,
            path,
            body,
        ) else {
            return Ok(false);
        };
        let signature = match hex::decode(&auth.signature) {
            Ok(signature) => signature,
            Err(_) => return Ok(false),
        };
        if !self
            .activity_signature_is_valid(&auth.node_id, &message, &signature)
            .await?
        {
            return Ok(false);
        }
        // The signed random nonce lets legitimate identical concurrent
        // requests remain distinct. Keep every admitted nonce for the full
        // window so a captured proof can authorize at most one request, and
        // reject rather than evict while the bounded cache is full. Forged
        // proofs never reach this cache.
        if !self.admit_internal_replay(&auth.node_id, &auth.nonce)?
            || !self.admit_internal_authority_check(&auth.node_id)?
        {
            return Ok(false);
        }
        self.verify_live_activity_authority(&auth.node_id, now)
            .await
    }

    /// Authenticate an idempotent read-only relay. Signature verification is
    /// mandatory, globally rate-bounded, and each signed nonce is single-use
    /// for the complete five-second read window. A separate, right-sized
    /// replay cache keeps segment bursts from consuming mutation admission;
    /// the live-voter proof is cached for one second per sender behind a
    /// single-flight mutex so segment bursts do not become Raft read bursts.
    pub async fn authorize_internal_peer_read_request(
        &self,
        auth: &InternalPeerAuth,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        if auth.target_node_id != inner.identity.node_id
            || now.abs_diff(auth.timestamp_ms) > INTERNAL_READ_AUTH_WINDOW_MS as u64
            || auth.node_id == auth.target_node_id
            || auth.node_id.len() > MAX_PEER_NODE_ID_BYTES
            || auth.target_node_id.len() > MAX_PEER_NODE_ID_BYTES
            || !canonical_internal_auth_nonce(&auth.nonce)
            || auth.signature.len() != ED25519_SIGNATURE_HEX_BYTES
        {
            return Ok(false);
        }
        if !inner
            .internal_read_preverify
            .lock()
            .map_err(|_| {
                MembershipError::Internal(
                    "internal read preverification lock was poisoned".to_owned(),
                )
            })?
            .admit(Instant::now())
        {
            return Ok(false);
        }
        let Some(message) = internal_peer_auth_message(
            &auth.node_id,
            &auth.target_node_id,
            auth.timestamp_ms,
            &auth.nonce,
            method,
            path,
            body,
        ) else {
            return Ok(false);
        };
        let signature = match hex::decode(&auth.signature) {
            Ok(signature) => signature,
            Err(_) => return Ok(false),
        };
        if !self
            .activity_signature_is_valid(&auth.node_id, &message, &signature)
            .await?
        {
            return Ok(false);
        }
        if !self.admit_internal_read_replay(&auth.node_id, &auth.nonce)? {
            return Ok(false);
        }
        if unix_ms()?.abs_diff(auth.timestamp_ms) > INTERNAL_READ_AUTH_WINDOW_MS as u64 {
            return Ok(false);
        }
        let observed = Instant::now();
        let refresh = {
            let mut authority = inner.internal_read_authority.lock().await;
            if let Some(entry) = authority.get(&auth.node_id) {
                if entry.expires_at > observed {
                    return Ok(unix_ms()?.abs_diff(auth.timestamp_ms)
                        <= INTERNAL_READ_AUTH_WINDOW_MS as u64);
                }
                Arc::clone(&entry.refresh)
            } else {
                // Only senders with a verified cached signing key reach this
                // map. Keep expired entries as their per-sender single-flight
                // gates; membership/key eviction removes them, so the map has
                // the same hard peer bound without a global slow-query lock.
                if authority.len() >= MAX_ACTIVITY_PEERS {
                    return Ok(false);
                }
                let refresh = Arc::new(tokio::sync::Mutex::new(()));
                authority.insert(
                    auth.node_id.clone(),
                    InternalReadAuthority {
                        expires_at: observed,
                        refresh: Arc::clone(&refresh),
                    },
                );
                refresh
            }
        };
        let _refresh = refresh.lock().await;
        // Queueing behind this sender's single-flight consumes both the proof
        // window and heartbeat freshness. Re-sample wall time after the wait;
        // never let the timestamp captured before signature verification
        // authorize a later consensus read or cached-authority hit.
        let authority_now = unix_ms()?;
        if authority_now.abs_diff(auth.timestamp_ms) > INTERNAL_READ_AUTH_WINDOW_MS as u64 {
            return Ok(false);
        }
        {
            // A waiter for this sender may find that the task ahead of it
            // already refreshed authority. Re-check without coupling any
            // other sender to the consistent read below.
            let authority = inner.internal_read_authority.lock().await;
            if authority
                .get(&auth.node_id)
                .is_some_and(|entry| entry.expires_at > Instant::now())
            {
                return Ok(
                    unix_ms()?.abs_diff(auth.timestamp_ms) <= INTERNAL_READ_AUTH_WINDOW_MS as u64
                );
            }
        }
        let live = self
            .verify_live_activity_authority(&auth.node_id, authority_now)
            .await?;
        let proof_still_fresh =
            unix_ms()?.abs_diff(auth.timestamp_ms) <= INTERNAL_READ_AUTH_WINDOW_MS as u64;
        if live && proof_still_fresh {
            let mut authority = inner.internal_read_authority.lock().await;
            if let Some(entry) = authority.get_mut(&auth.node_id) {
                entry.expires_at = Instant::now() + INTERNAL_READ_AUTHORITY_TTL;
            }
        }
        Ok(live && proof_still_fresh)
    }

    async fn activity_signature_is_valid(
        &self,
        node_id: &str,
        message: &[u8],
        signature: &[u8],
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let cached = inner
            .activity_public_keys
            .lock()
            .map_err(|_| {
                MembershipError::Internal("activity public-key lock was poisoned".to_owned())
            })?
            .get(node_id)
            .cloned();
        if let Some(public_key) = cached {
            return Ok(UnparsedPublicKey::new(&ED25519, public_key)
                .verify(message, signature)
                .is_ok());
        }
        if !self.admit_activity_key_lookup()? {
            return Ok(false);
        }
        let rows = inner
            .client
            .query_map::<ActivityPublicKeyRow, _>(
                "SELECT key.node_id, key.public_key \
                 FROM cluster_node_activity_keys key \
                 JOIN cluster_nodes node ON node.node_id = key.node_id \
                 WHERE key.node_id = $1 AND node.removed_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id) LIMIT 1",
                params!(node_id),
            )
            .await?;
        let Some((node_id, public_key)) = rows.into_iter().next().and_then(|row| {
            let public_key = hex::decode(row.public_key).ok()?;
            (public_key.len() == 32).then_some((row.node_id, public_key))
        }) else {
            return Ok(false);
        };
        if UnparsedPublicKey::new(&ED25519, &public_key)
            .verify(message, signature)
            .is_err()
        {
            return Ok(false);
        }

        let evicted = {
            let mut keys = inner.activity_public_keys.lock().map_err(|_| {
                MembershipError::Internal("activity public-key lock was poisoned".to_owned())
            })?;
            insert_bounded_activity_public_key(&mut keys, node_id, public_key)
        };
        if let Some(evicted) = evicted {
            inner
                .activity_auth_admission
                .lock()
                .map_err(|_| {
                    MembershipError::Internal("activity admission lock was poisoned".to_owned())
                })?
                .remove(&evicted);
            inner
                .internal_auth_admission
                .lock()
                .map_err(|_| {
                    MembershipError::Internal(
                        "internal peer admission lock was poisoned".to_owned(),
                    )
                })?
                .remove(&evicted);
            inner
                .internal_auth_replays
                .lock()
                .map_err(|_| {
                    MembershipError::Internal("internal peer replay lock was poisoned".to_owned())
                })?
                .remove(&evicted);
            inner
                .internal_read_replays
                .lock()
                .map_err(|_| {
                    MembershipError::Internal("internal read replay lock was poisoned".to_owned())
                })?
                .remove(&evicted);
            inner.internal_read_authority.lock().await.remove(&evicted);
        }
        Ok(true)
    }

    fn admit_activity_key_lookup(&self) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = Instant::now();
        Ok(inner
            .activity_key_lookup_admission
            .lock()
            .map_err(|_| {
                MembershipError::Internal(
                    "activity key-lookup admission lock was poisoned".to_owned(),
                )
            })?
            .admit(now, MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND))
    }

    fn admit_activity_authority_check(&self, node_id: &str) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = Instant::now();
        let mut admission = inner.activity_auth_admission.lock().map_err(|_| {
            MembershipError::Internal("activity admission lock was poisoned".to_owned())
        })?;
        let state = admission
            .entry(node_id.to_owned())
            .or_insert(ActivityAuthAdmission {
                window_started: now,
                checks: 0,
            });
        Ok(state.admit(now, MAX_ACTIVITY_AUTH_CHECKS_PER_SECOND))
    }

    fn admit_internal_authority_check(&self, node_id: &str) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = Instant::now();
        let mut admission = inner.internal_auth_admission.lock().map_err(|_| {
            MembershipError::Internal("internal peer admission lock was poisoned".to_owned())
        })?;
        let state = admission
            .entry(node_id.to_owned())
            .or_insert(ActivityAuthAdmission {
                window_started: now,
                checks: 0,
            });
        Ok(state.admit(now, MAX_INTERNAL_AUTH_CHECKS_PER_SECOND))
    }

    fn admit_internal_replay(&self, node_id: &str, nonce: &str) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = Instant::now();
        let mut replays = inner.internal_auth_replays.lock().map_err(|_| {
            MembershipError::Internal("internal peer replay lock was poisoned".to_owned())
        })?;
        Ok(replays
            .entry(node_id.to_owned())
            .or_default()
            .admit(nonce, now))
    }

    fn admit_internal_read_replay(
        &self,
        node_id: &str,
        nonce: &str,
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = Instant::now();
        let mut replays = inner.internal_read_replays.lock().map_err(|_| {
            MembershipError::Internal("internal read replay lock was poisoned".to_owned())
        })?;
        Ok(replays
            .entry(node_id.to_owned())
            .or_default()
            .admit(nonce, now))
    }

    async fn verify_live_activity_authority(
        &self,
        node_id: &str,
        now: i64,
    ) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        if !metrics
            .membership_config
            .voter_ids()
            .any(|raft_id| raft_id == inner.identity.raft_id)
        {
            return Ok(false);
        }
        let reachable_after = reachable_after(now);
        let rows = inner
            .client
            .query_consistent_map::<ActivityAuthNodeRow, _>(
                "SELECT node.raft_id, node.last_seen_at \
                 FROM cluster_nodes node \
                 WHERE node.node_id = $1 AND node.removed_at IS NULL \
                   AND node.last_seen_at >= $2 \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id)",
                params!(node_id, reachable_after),
            )
            .await?;
        let verified_reachable_after = unix_ms()?.saturating_sub(NODE_REACHABLE_WINDOW_MS);
        Ok(rows.len() == 1
            && rows[0].last_seen_at >= verified_reachable_after
            && metrics
                .membership_config
                .voter_ids()
                .any(|raft_id| raft_id == rows[0].raft_id))
    }

    async fn refresh_membership_metrics(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let result = async {
            let now = unix_ms()?;
            let metrics = inner.client.metrics_db().await?;
            let voters = metrics
                .membership_config
                .voter_ids()
                .collect::<BTreeSet<_>>();
            let members = metrics
                .membership_config
                .nodes()
                .map(|(raft_id, _)| *raft_id)
                .collect::<BTreeSet<_>>();
            let rows = inner
                .client
                .query_map::<MembershipMetricsRow, _>(
                    "SELECT node.raft_id, node.last_seen_at, \
                            EXISTS (SELECT 1 FROM cluster_node_removals removal \
                              WHERE removal.node_id = node.node_id) AS removal_pending \
                     FROM cluster_nodes node WHERE node.removed_at IS NULL",
                    params!(),
                )
                .await?;
            let rows = rows
                .into_iter()
                .filter(|row| members.contains(&row.raft_id))
                .map(|row| (row.raft_id, row))
                .collect::<BTreeMap<_, _>>();
            let heartbeat_fresh_voters = voters
                .iter()
                .filter(|raft_id| {
                    rows.get(raft_id)
                        .is_some_and(|row| node_is_reachable(now, row.last_seen_at))
                })
                .count() as u64;
            Ok::<_, MembershipError>(MembershipMetricsSample {
                voters: voters.len() as u64,
                learners: members.difference(&voters).count() as u64,
                heartbeat_fresh_voters,
                heartbeat_stale_voters: (voters.len() as u64)
                    .saturating_sub(heartbeat_fresh_voters),
                removals_pending: rows.values().filter(|row| row.removal_pending).count() as u64,
                local_is_voter: voters.contains(&inner.identity.raft_id),
            })
        }
        .await;
        match result {
            Ok(sample) => {
                inner.membership_metrics.record(sample);
                Ok(())
            }
            Err(error) => {
                inner.membership_metrics.record_error();
                Err(error)
            }
        }
    }

    pub async fn heartbeat_loop(self) {
        if self.inner.is_none() {
            return;
        }
        loop {
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
            match self.heartbeat().await {
                Err(error) => {
                    if let Some(inner) = self.inner.as_deref() {
                        inner.membership_metrics.record_error();
                    }
                    tracing::warn!(code = error.code(), "cluster node heartbeat failed");
                }
                Ok(()) => {
                    if let Err(error) = self.refresh_membership_metrics().await {
                        tracing::warn!(
                            code = error.code(),
                            "cluster membership metrics refresh failed"
                        );
                    }
                    if let Err(error) = self.refresh_activity_public_keys().await {
                        tracing::warn!(
                            code = error.code(),
                            "cluster activity verifier refresh failed"
                        );
                    }
                }
            }
        }
    }

    pub async fn status(&self) -> Result<MembershipStatus, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        let metrics = inner.client.metrics_db().await?;
        let voters = metrics
            .membership_config
            .voter_ids()
            .collect::<BTreeSet<_>>();
        let members = metrics
            .membership_config
            .nodes()
            .map(|(id, _)| *id)
            .collect::<BTreeSet<_>>();
        let rows = inner
            .client
            .query_map::<MembershipNodeRow, _>(
                "SELECT n.node_id, n.raft_id, n.api_address, n.last_seen_at, n.role, \
                        COALESCE(h.hostname, '') AS hostname, \
                        progress.apply_lag_entries, \
                        COALESCE(progress.bounded_read_ready, 0) AS bounded_read_ready, \
                        COALESCE(progress.voter_storage_ready, 0) AS voter_storage_ready, \
                        progress.storage_headroom_bytes, \
                        progress.storage_probe_observed_at, \
                        progress.observed_at AS progress_observed_at, \
                        maintenance.requested_at AS maintenance_requested_at, \
                        maintenance.acknowledged_at AS maintenance_acknowledged_at, \
                        EXISTS (SELECT 1 FROM cluster_node_capabilities capability \
                          WHERE capability.node_id = n.node_id \
                            AND capability.capability = $2 \
                            AND capability.last_seen_at = n.last_seen_at) AS maintenance_capable, \
                        (SELECT COUNT(*) FROM media_sessions session \
                          WHERE session.owner_node_id = n.node_id \
                            AND session.state = 'active' \
                            AND session.lease_expires_at_ms > $1) AS active_media_sessions, \
                        EXISTS (SELECT 1 FROM cluster_node_removals AS removal \
                          WHERE removal.node_id = n.node_id) AS removal_pending \
                 FROM cluster_nodes n \
                 LEFT JOIN cluster_node_hostnames h ON h.node_id = n.node_id \
                 LEFT JOIN cluster_node_progress progress ON progress.node_id = n.node_id \
                 LEFT JOIN cluster_node_maintenance maintenance ON maintenance.node_id = n.node_id \
                 WHERE n.removed_at IS NULL ORDER BY n.raft_id",
                params!(now, NODE_MAINTENANCE_CAPABILITY),
            )
            .await?;
        let protocol = self.protocol_status().await?;
        let pending = protocol
            .learner_protocol_pending
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let nodes = rows
            .into_iter()
            .filter(|row| members.contains(&(row.raft_id as u64)))
            .map(|row| {
                let admitted_role = ClusterRole::from_stored(row.admitted_role.as_deref())?;
                let raft_id = row.raft_id as u64;
                let reachable = node_is_reachable(now, row.last_seen_at);
                let maintenance = row.maintenance_requested_at.is_some();
                let maintenance_acknowledged =
                    row.maintenance_acknowledged_at.is_some() && row.maintenance_capable;
                Ok(ClusterNodeRecord {
                    learner_protocol_ready: !pending.contains(&row.node_id),
                    node_id: row.node_id,
                    hostname: membership_hostname(&row.hostname, &row.api_address),
                    advertised_host: advertised_host(&row.api_address),
                    raft_id,
                    role: match admitted_role {
                        ClusterRole::Voter => NodeRole::Voter,
                        ClusterRole::Learner => NodeRole::Learner,
                    },
                    is_voter: voters.contains(&raft_id),
                    is_leader: metrics.current_leader == Some(raft_id),
                    reachable,
                    last_seen_at: row.last_seen_at,
                    removal_pending: row.removal_pending,
                    bounded_read_ready: row.bounded_read_ready
                        && node_is_reachable(now, row.progress_observed_at.unwrap_or_default())
                        && !row.removal_pending,
                    apply_lag_entries: row
                        .apply_lag_entries
                        .and_then(|value| u64::try_from(value).ok()),
                    storage_headroom_bytes: row
                        .storage_headroom_bytes
                        .and_then(|value| u64::try_from(value).ok()),
                    voter_storage_ready: row.voter_storage_ready
                        && node_is_reachable(
                            now,
                            row.storage_probe_observed_at.unwrap_or_default(),
                        ),
                    maintenance,
                    maintenance_requested_at: row.maintenance_requested_at,
                    maintenance_acknowledged,
                    maintenance_ready: maintenance
                        && maintenance_acknowledged
                        && reachable
                        && (voters.len() == 1 || metrics.current_leader != Some(raft_id))
                        && row.apply_lag_entries == Some(0)
                        && row.active_media_sessions == 0,
                    active_media_sessions: usize::try_from(row.active_media_sessions)
                        .unwrap_or(usize::MAX),
                })
            })
            .collect::<Result<Vec<_>, MembershipError>>()?;
        let availability = match voters.len() {
            0 | 1 => ClusterAvailability::SingleNode,
            2 => ClusterAvailability::DegradedReconfiguration,
            _ => ClusterAvailability::HighAvailability,
        };
        let voting_quorum = voters.len() / 2 + 1;
        let reachable_voters = nodes
            .iter()
            .filter(|node| node.is_voter && node.reachable)
            .count();
        let quorum_available = reachable_voters >= voting_quorum;
        let leader_elected = metrics.current_leader.is_some();
        let non_voting_replicas = nodes
            .iter()
            .filter(|node| node.role == NodeRole::Learner && !node.is_voter)
            .count();
        let ready_read_workers = nodes
            .iter()
            .filter(|node| {
                counts_as_ready_read_worker(&node.role, node.is_voter, node.bounded_read_ready)
            })
            .count();
        let replication = inner.replication.status().await;
        Ok(MembershipStatus {
            local_node_id: inner.identity.node_id.clone(),
            availability,
            nodes,
            replication,
            capacity: ClusterCapacityStatus {
                voting_nodes: voters.len(),
                voting_quorum,
                voting_failure_tolerance: voters.len().saturating_sub(voting_quorum),
                non_voting_replicas,
                ready_read_workers,
            },
            protocol,
            recovery: ClusterRecoveryStatus {
                required: !quorum_available || !leader_elected,
                quorum_available,
                reachable_voters,
                required_voters: voting_quorum,
                leader_elected,
                permanent_majority_loss_supported: false,
            },
        })
    }

    /// Promote one ready learner into the committed voter set.
    ///
    /// Readiness is the target's own fresh zero-lag heartbeat, followed by a
    /// new quorum-confirmed barrier that the same target must apply. The
    /// lifecycle intent blocks removal across the otherwise unavoidably
    /// separate SQL and OpenRaft membership operations. Ambiguous transport
    /// results retain that intent until a retry reconciles the voter set.
    pub async fn promote_learner(
        &self,
        node_id: &str,
    ) -> Result<MembershipStatus, MembershipError> {
        let inner = self.replicated_inner()?;
        self.require_learner_lifecycle_capability().await?;
        if self.maintenance_operation_pending().await? {
            return Err(MembershipError::MaintenanceConflict(node_id.to_owned()));
        }
        let initial_metrics = inner.client.metrics_db().await?;
        let target = self.promotion_target(node_id).await?;
        let target_raft_id = u64::try_from(target.raft_id)
            .map_err(|_| MembershipError::PromotionRequiresLearner(node_id.to_owned()))?;
        let already_voter = initial_metrics
            .membership_config
            .voter_ids()
            .any(|raft_id| raft_id == target_raft_id);
        let admitted_role = ClusterRole::from_stored(target.admitted_role.as_deref())?;
        if already_voter {
            // Idempotence is only for a learner whose Raft promotion committed
            // before the SQL role/intent transaction returned. An arbitrary
            // existing voter is not a successful learner promotion request.
            if admitted_role != ClusterRole::Learner {
                return Err(MembershipError::PromotionRequiresLearner(
                    node_id.to_owned(),
                ));
            }
            self.wait_for_promoted_voter_reconciliation(node_id).await?;
            self.finish_learner_promotion(node_id).await?;
            return self.status().await;
        }
        if !initial_metrics
            .membership_config
            .nodes()
            .any(|(raft_id, _)| *raft_id == target_raft_id)
            || admitted_role != ClusterRole::Learner
            || target.removal_pending
        {
            return Err(MembershipError::PromotionRequiresLearner(
                node_id.to_owned(),
            ));
        }
        self.require_ready_promotion_target(node_id, &target)?;

        let existing = inner
            .client
            .query_consistent_map::<PromotionAttemptRow, _>(
                "SELECT attempt_id \
                 FROM cluster_node_promotions WHERE node_id = $1",
                params!(node_id),
            )
            .await?
            .into_iter()
            .next();
        if existing.is_some() {
            let membership_nodes = initial_metrics
                .membership_config
                .membership()
                .nodes()
                .map(|(raft_id, node)| (*raft_id, node.addr_api.clone()))
                .collect::<Vec<_>>();
            match reconcile_promotion_change(&inner.secrets.api, target_raft_id, &membership_nodes)
                .await
            {
                MembershipChangeOutcome::Promoted => {
                    self.wait_for_promoted_voter_reconciliation(node_id).await?;
                    self.finish_learner_promotion(node_id).await?;
                    return self.status().await;
                }
                MembershipChangeOutcome::Removed => {
                    return Err(MembershipError::PromotionRequiresLearner(
                        node_id.to_owned(),
                    ));
                }
                MembershipChangeOutcome::Indeterminate => {}
            }
        }
        let (attempt_id, new_attempt) = if let Some(existing) = existing {
            (existing.attempt_id, false)
        } else {
            let attempt_id = uuid::Uuid::new_v4().to_string();
            let started_at = unix_ms()?;
            let inserted = inner
                .client
                .execute(
                    "INSERT INTO cluster_node_promotions \
                 (node_id, attempt_id, barrier_index, started_at) \
                 SELECT $1, $2, NULL, $3 \
                 WHERE NOT EXISTS (SELECT 1 FROM cluster_node_removals WHERE node_id = $1) \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_maintenance) \
                   AND NOT EXISTS (SELECT 1 FROM cluster_operation_leases) \
                 ON CONFLICT(node_id) DO NOTHING",
                    params!(node_id, attempt_id.as_str(), started_at),
                )
                .await?;
            if inserted != 1 {
                if self.maintenance_operation_pending().await? {
                    return Err(MembershipError::MaintenanceConflict(node_id.to_owned()));
                }
                return Err(MembershipError::LearnerLifecyclePending(node_id.to_owned()));
            }
            (attempt_id, true)
        };
        // Every submission, including an ambiguous retry, crosses a new
        // target-local barrier. Reusing the first request's watermark would
        // let a learner go offline and still be added to the voter set from a
        // stale progress row during the freshness window.
        let barrier = match inner.client.db_quorum_watermark().await {
            Ok(watermark) => watermark.committed_index,
            Err(error) => {
                if new_attempt {
                    self.clear_learner_promotion(node_id, &attempt_id).await;
                }
                return Err(error.into());
            }
        };
        inner
            .client
            .execute(
                "UPDATE cluster_node_promotions SET barrier_index = $1 \
                     WHERE node_id = $2 AND attempt_id = $3",
                params!(
                    i64::try_from(barrier).unwrap_or(i64::MAX),
                    node_id,
                    attempt_id.as_str()
                ),
            )
            .await?;
        if let Err(error) = self.wait_for_promotion_barrier(node_id, barrier).await {
            if new_attempt {
                self.clear_learner_promotion(node_id, &attempt_id).await;
            }
            return Err(error);
        }

        let metrics = inner.client.metrics_db().await?;
        let leader_id = metrics
            .current_leader
            .ok_or(MembershipError::LeaderUnavailable)?;
        let leader = metrics
            .membership_config
            .membership()
            .get_node(&leader_id)
            .ok_or_else(|| MembershipError::Internal("leader has no node record".to_owned()))?;
        let target_node = metrics
            .membership_config
            .membership()
            .get_node(&target_raft_id)
            .cloned()
            .ok_or_else(|| MembershipError::PromotionRequiresLearner(node_id.to_owned()))?;
        let membership_nodes = metrics
            .membership_config
            .membership()
            .nodes()
            .map(|(raft_id, node)| (*raft_id, node.addr_api.clone()))
            .collect::<Vec<_>>();
        match request_learner_promotion(&leader.addr_api, &inner.secrets.api, &target_node).await {
            Ok(()) => {}
            Err(MembershipChangeFailure::Rejected(error)) => {
                if new_attempt {
                    self.clear_learner_promotion(node_id, &attempt_id).await;
                    return Err(error);
                }
                return Err(MembershipError::LearnerLifecyclePending(format!(
                    "{node_id}: retry was rejected after an earlier ambiguous attempt: {error}"
                )));
            }
            Err(MembershipChangeFailure::Ambiguous(error)) => {
                match reconcile_promotion_change(
                    &inner.secrets.api,
                    target_raft_id,
                    &membership_nodes,
                )
                .await
                {
                    MembershipChangeOutcome::Promoted => {
                        tracing::warn!(%error, %node_id, "learner promotion committed after an ambiguous HTTP result");
                    }
                    MembershipChangeOutcome::Indeterminate | MembershipChangeOutcome::Removed => {
                        return Err(MembershipError::LearnerLifecyclePending(format!(
                            "{node_id}: promotion outcome is indeterminate after {error}"
                        )));
                    }
                }
            }
        }
        self.wait_for_promoted_voter_reconciliation(node_id).await?;
        self.finish_learner_promotion(node_id).await?;
        self.status().await
    }

    async fn promotion_target(&self, node_id: &str) -> Result<PromotionTargetRow, MembershipError> {
        let inner = self.replicated_inner()?;
        inner
            .client
            .query_consistent_map::<PromotionTargetRow, _>(
                "SELECT node.raft_id, node.role, node.last_seen_at, \
                        EXISTS (SELECT 1 FROM cluster_node_removals removal \
                          WHERE removal.node_id = node.node_id) AS removal_pending, \
                        progress.last_applied_index, progress.apply_lag_entries, \
                        COALESCE(progress.bounded_read_ready, 0) AS bounded_read_ready, \
                        COALESCE(progress.voter_storage_ready, 0) AS voter_storage_ready, \
                        progress.storage_headroom_bytes, progress.storage_probe_observed_at, \
                        COALESCE(progress.voter_role_persisted, 0) AS voter_role_persisted, \
                        progress.observed_at \
                 FROM cluster_nodes node \
                 LEFT JOIN cluster_node_progress progress ON progress.node_id = node.node_id \
                 WHERE node.node_id = $1 AND node.removed_at IS NULL",
                params!(node_id),
            )
            .await?
            .into_iter()
            .next()
            .ok_or(MembershipError::NodeNotFound)
    }

    fn require_ready_promotion_target(
        &self,
        node_id: &str,
        target: &PromotionTargetRow,
    ) -> Result<(), MembershipError> {
        let now = unix_ms()?;
        if !node_is_reachable(now, target.last_seen_at)
            || !node_is_reachable(now, target.observed_at.unwrap_or_default())
            || !target.bounded_read_ready
            || target.apply_lag_entries != Some(0)
            || target.last_applied_index.is_none()
        {
            return Err(MembershipError::LearnerNotReady(node_id.to_owned()));
        }
        if !target.voter_storage_ready
            || !target.storage_probe_observed_at.is_some_and(|observed_at| {
                now.saturating_sub(observed_at) <= STORAGE_DURABILITY_PROBE_MAX_AGE_MS
            })
            || target.storage_headroom_bytes.unwrap_or_default()
                < i64::try_from(MIN_VOTER_STORAGE_HEADROOM_BYTES).unwrap_or(i64::MAX)
        {
            return Err(MembershipError::VoterStoragePreflight {
                node_id: node_id.to_owned(),
                required_bytes: MIN_VOTER_STORAGE_HEADROOM_BYTES,
            });
        }
        Ok(())
    }

    async fn wait_for_promotion_barrier(
        &self,
        node_id: &str,
        barrier: u64,
    ) -> Result<(), MembershipError> {
        let deadline = tokio::time::Instant::now() + PROMOTION_BARRIER_WAIT;
        loop {
            let target = self.promotion_target(node_id).await?;
            self.require_ready_promotion_target(node_id, &target)?;
            // The progress row is written by a heartbeat transaction. A
            // target-local applied index at or beyond the promotion-intent
            // barrier proves this heartbeat happened after that barrier,
            // without comparing wall clocks from two different machines.
            if target
                .last_applied_index
                .and_then(|index| u64::try_from(index).ok())
                .is_some_and(|index| index >= barrier)
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MembershipError::LearnerNotReady(node_id.to_owned()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn wait_for_promoted_voter_reconciliation(
        &self,
        node_id: &str,
    ) -> Result<(), MembershipError> {
        let deadline = tokio::time::Instant::now() + PROMOTION_BARRIER_WAIT;
        loop {
            let target = self.promotion_target(node_id).await?;
            let now = unix_ms()?;
            if target.voter_role_persisted
                && node_is_reachable(now, target.last_seen_at)
                && node_is_reachable(now, target.observed_at.unwrap_or_default())
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MembershipError::LearnerLifecyclePending(format!(
                    "{node_id}: committed voter has not persisted its restart role"
                )));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn finish_learner_promotion(&self, node_id: &str) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        inner
            .client
            .txn(vec![
                (
                    "UPDATE cluster_nodes SET role = 'voter' \
                     WHERE node_id = $1 AND removed_at IS NULL"
                        .to_owned(),
                    params!(node_id),
                ),
                (
                    "DELETE FROM cluster_node_promotions WHERE node_id = $1".to_owned(),
                    params!(node_id),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(())
    }

    async fn clear_learner_promotion(&self, node_id: &str, attempt_id: &str) {
        let Ok(inner) = self.replicated_inner() else {
            return;
        };
        if let Err(error) = inner
            .client
            .execute(
                "DELETE FROM cluster_node_promotions WHERE node_id = $1 AND attempt_id = $2",
                params!(node_id, attempt_id),
            )
            .await
        {
            tracing::warn!(%error, %node_id, "could not clear rejected learner promotion intent");
        }
    }

    /// Remove either a voter or an admitted learner. The two paths share the
    /// durable admission fence and owned-work settlement, but only voter
    /// removal enforces the reconfiguration quorum-size rule.
    pub async fn remove_node(&self, node_id: &str) -> Result<MembershipStatus, MembershipError> {
        let inner = self.replicated_inner()?;
        if node_id == inner.identity.node_id {
            return Err(MembershipError::SelfRemovalRequiresLeave);
        }
        let metrics = inner.client.metrics_db().await?;
        let target = inner
            .client
            .query_consistent_map::<PromotionTargetRow, _>(
                "SELECT node.raft_id, node.role, node.last_seen_at, \
                        EXISTS (SELECT 1 FROM cluster_node_removals removal \
                          WHERE removal.node_id = node.node_id) AS removal_pending, \
                        progress.last_applied_index, progress.apply_lag_entries, \
                        COALESCE(progress.bounded_read_ready, 0) AS bounded_read_ready, \
                        COALESCE(progress.voter_storage_ready, 0) AS voter_storage_ready, \
                        progress.storage_headroom_bytes, progress.storage_probe_observed_at, \
                        COALESCE(progress.voter_role_persisted, 0) AS voter_role_persisted, \
                        progress.observed_at \
                 FROM cluster_nodes node \
                 LEFT JOIN cluster_node_progress progress ON progress.node_id = node.node_id \
                 WHERE node.node_id = $1",
                params!(node_id),
            )
            .await?
            .into_iter()
            .next()
            .ok_or(MembershipError::NodeNotFound)?;
        let target_raft_id = u64::try_from(target.raft_id).unwrap_or_default();
        if metrics
            .membership_config
            .voter_ids()
            .any(|raft_id| raft_id == target_raft_id)
        {
            self.remove_voter(node_id).await
        } else if metrics
            .membership_config
            .nodes()
            .any(|(raft_id, _)| *raft_id == target_raft_id)
            && ClusterRole::from_stored(target.admitted_role.as_deref())? == ClusterRole::Learner
        {
            self.remove_learner_impl(node_id).await?;
            self.status().await
        } else if self.node_is_tombstoned(node_id).await? {
            self.fence_removed_job_owner(node_id).await?;
            self.finalize_node_removal(node_id).await;
            self.status().await
        } else {
            Err(MembershipError::NodeNotFound)
        }
    }

    async fn remove_learner_impl(&self, node_id: &str) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        self.require_learner_lifecycle_capability().await?;
        self.require_removal_capability().await?;
        let metrics = inner.client.metrics_db().await?;
        let target = self.promotion_target(node_id).await?;
        let target_raft_id = u64::try_from(target.raft_id)
            .map_err(|_| MembershipError::PromotionRequiresLearner(node_id.to_owned()))?;
        let voters = metrics
            .membership_config
            .voter_ids()
            .collect::<BTreeSet<_>>();
        let members = metrics
            .membership_config
            .nodes()
            .map(|(raft_id, _)| *raft_id)
            .collect::<BTreeSet<_>>();
        if voters.contains(&target_raft_id)
            || !members.contains(&target_raft_id)
            || ClusterRole::from_stored(target.admitted_role.as_deref())? != ClusterRole::Learner
        {
            return Err(MembershipError::PromotionRequiresLearner(
                node_id.to_owned(),
            ));
        }
        let mut resolved = self.settle_offline_work(node_id).await?;
        let leader_id = metrics
            .current_leader
            .ok_or(MembershipError::LeaderUnavailable)?;
        let leader = metrics
            .membership_config
            .membership()
            .get_node(&leader_id)
            .ok_or_else(|| {
                MembershipError::Internal("cluster leader has no node record".to_owned())
            })?;
        let membership_nodes = metrics
            .membership_config
            .membership()
            .nodes()
            .map(|(raft_id, node)| (*raft_id, node.addr_api.clone()))
            .collect::<Vec<_>>();
        // Learner removal is the draining variant: the reference-counted
        // fence first ejects placement, supersedes active media ownership,
        // expires job ownership, and blocks every later route admission.
        let (removal_attempt, new_attempt) = if target.removal_pending {
            (self.existing_removal_attempt(node_id).await?, false)
        } else {
            (self.begin_node_removal(node_id, true).await?, true)
        };
        let fence_barrier = inner.client.db_quorum_watermark().await?.committed_index;
        self.wait_for_removal_fence(node_id, fence_barrier).await?;
        match self.settle_offline_work(node_id).await {
            Ok(report) => {
                resolved.requeued += report.requeued;
                resolved.failed += report.failed;
            }
            Err(error) => {
                if new_attempt {
                    return Err(self
                        .rollback_node_removal_after_failure(node_id, &removal_attempt, error)
                        .await);
                }
                return Err(MembershipError::RemovalPending(error.to_string()));
            }
        }
        match request_learner_removal(&leader.addr_api, &inner.secrets.api, target_raft_id).await {
            Ok(()) => {}
            Err(MembershipChangeFailure::Rejected(error)) => {
                if new_attempt {
                    return Err(self
                        .rollback_node_removal_after_failure(node_id, &removal_attempt, error)
                        .await);
                }
                return Err(MembershipError::RemovalPending(error.to_string()));
            }
            Err(MembershipChangeFailure::Ambiguous(error)) => {
                match reconcile_member_removal(
                    &inner.secrets.api,
                    target_raft_id,
                    &membership_nodes,
                )
                .await
                {
                    MembershipChangeOutcome::Removed => {
                        tracing::warn!(%error, %node_id, "learner removal committed after an ambiguous HTTP result");
                    }
                    MembershipChangeOutcome::Indeterminate | MembershipChangeOutcome::Promoted => {
                        return Err(MembershipError::RemovalPending(format!(
                            "learner removal outcome is indeterminate after {error}"
                        )));
                    }
                }
            }
        }
        self.finalize_node_removal(node_id).await;
        tracing::info!(
            %node_id,
            requeued = resolved.requeued,
            failed = resolved.failed,
            "removed non-voting learner"
        );
        Ok(())
    }

    async fn existing_removal_attempt(&self, node_id: &str) -> Result<String, MembershipError> {
        let inner = self.replicated_inner()?;
        inner
            .client
            .query_consistent_map::<RemovalAttemptRow, _>(
                "SELECT attempt_id FROM cluster_node_removal_attempts \
                 WHERE node_id = $1 ORDER BY attempt_id LIMIT 1",
                params!(node_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.attempt_id)
            .ok_or_else(|| {
                MembershipError::RemovalPending(format!(
                    "{node_id} has a removal fence without an attempt reference"
                ))
            })
    }

    async fn wait_for_removal_fence(
        &self,
        node_id: &str,
        barrier: u64,
    ) -> Result<(), MembershipError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let target = self.promotion_target(node_id).await?;
            if !node_is_reachable(unix_ms()?, target.last_seen_at) {
                return Ok(());
            }
            if target
                .last_applied_index
                .and_then(|index| u64::try_from(index).ok())
                .is_some_and(|index| index >= barrier)
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MembershipError::RemovalPending(format!(
                    "{node_id} has not applied its durable route fence"
                )));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn remove_voter(&self, node_id: &str) -> Result<MembershipStatus, MembershipError> {
        let inner = self.replicated_inner()?;
        if node_id == inner.identity.node_id {
            return Err(MembershipError::SelfRemovalRequiresLeave);
        }
        let metrics = inner.client.metrics_db().await?;
        let target = inner
            .client
            .query_consistent_map::<TargetNodeRow, _>(
                "SELECT raft_id FROM cluster_nodes WHERE node_id = $1",
                params!(node_id),
            )
            .await?
            .into_iter()
            .next()
            .ok_or(MembershipError::NodeNotFound)?;
        let voters = metrics
            .membership_config
            .voter_ids()
            .collect::<BTreeSet<_>>();
        let target_raft_id = target.raft_id as u64;
        if !voters.contains(&target_raft_id) {
            if self.node_is_tombstoned(node_id).await? {
                self.fence_removed_job_owner(node_id).await?;
                self.finalize_node_removal(node_id).await;
                return self.status().await;
            }
            // A committed member that is not a voter is not "not found" — the
            // roster lists it, and answering 404 for a node an operator can
            // see is the least useful thing this could say. This low-level
            // compatibility method remains voter-only; `remove_node` chooses
            // the learner lifecycle before reaching it.
            if metrics
                .membership_config
                .nodes()
                .any(|(id, _)| *id == target_raft_id)
            {
                return Err(MembershipError::NonVoterRemovalUnsupported(
                    node_id.to_owned(),
                ));
            }
            return Err(MembershipError::NodeNotFound);
        }
        if metrics.current_leader == Some(target_raft_id) {
            return Err(MembershipError::LeaderRemoval);
        }
        if voters.len() < 3 {
            return Err(MembershipError::QuorumLoss);
        }
        self.require_removal_capability().await?;
        // Offline work is resolved before the membership change commits, never
        // after. A removal that half-commits leaves packages owned by a node
        // that no longer exists, which `CLUSTERING-PLAN.md` §6.7 calls an
        // activation blocker and which is strictly worse than refusing.
        let mut resolved = self.settle_offline_work(node_id).await?;
        let leader_id = metrics
            .current_leader
            .ok_or_else(|| MembershipError::Internal("cluster has no current leader".to_owned()))?;
        let leader = metrics
            .membership_config
            .membership()
            .get_node(&leader_id)
            .ok_or_else(|| MembershipError::Internal("leader has no node record".to_owned()))?;
        let membership_nodes = metrics
            .membership_config
            .membership()
            .nodes()
            .map(|(raft_id, node)| (*raft_id, node.addr_api.clone()))
            .collect::<Vec<_>>();
        let removal_attempt = self.begin_node_removal(node_id, false).await?;
        // Close the final admission race only after the durable removal fence
        // exists. Package creation checks that fence atomically, so this is the
        // last work that can still be owned by the departing node. Reusing the
        // normal settlement path preserves the refusal for an active transfer.
        // A definite pre-proposal failure rolls the fence back; any packages
        // already resolved remain safely resolved for the retry.
        match self.settle_offline_work(node_id).await {
            Ok(report) => {
                resolved.requeued += report.requeued;
                resolved.failed += report.failed;
            }
            Err(error) => {
                return Err(self
                    .rollback_node_removal_after_failure(node_id, &removal_attempt, error)
                    .await);
            }
        }
        match request_voter_removal(&leader.addr_api, &inner.secrets.api, target_raft_id).await {
            Ok(()) => {}
            Err(MembershipChangeFailure::Rejected(removal_error)) => {
                return Err(self
                    .rollback_node_removal_after_failure(node_id, &removal_attempt, removal_error)
                    .await);
            }
            Err(MembershipChangeFailure::Ambiguous(removal_error)) => {
                match reconcile_membership_change(
                    &inner.secrets.api,
                    target_raft_id,
                    &membership_nodes,
                )
                .await
                {
                    MembershipChangeOutcome::Removed => {
                        tracing::warn!(%removal_error, %node_id, "voter removal committed after an ambiguous HTTP result");
                    }
                    MembershipChangeOutcome::Indeterminate | MembershipChangeOutcome::Promoted => {
                        return Err(MembershipError::Internal(format!(
                        "voter removal outcome is indeterminate after {removal_error}; the target remains fenced"
                    )));
                    }
                }
            }
        }
        self.finalize_node_removal(node_id).await;
        if resolved.requeued + resolved.failed > 0 {
            tracing::info!(
                requeued = resolved.requeued,
                failed = resolved.failed,
                "resolved offline packages owned by a removed node"
            );
        }
        self.status().await
    }

    /// Remove this process using the lifecycle appropriate to its effective
    /// committed role. A learner leave never changes quorum arithmetic.
    pub async fn leave_node(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        if metrics
            .membership_config
            .voter_ids()
            .any(|raft_id| raft_id == inner.identity.raft_id)
        {
            self.leave_voter().await
        } else if metrics
            .membership_config
            .nodes()
            .any(|(raft_id, _)| *raft_id == inner.identity.raft_id)
        {
            self.remove_learner_impl(&inner.identity.node_id).await
        } else if self.node_is_tombstoned(&inner.identity.node_id).await? {
            self.finalize_node_removal(&inner.identity.node_id).await;
            Ok(())
        } else {
            Err(MembershipError::NodeNotFound)
        }
    }

    /// Remove this process's own voter, including when it is the leader.
    ///
    /// Arbitrary leader removal remains refused by [`Self::remove_voter`]. A
    /// self-leave is different: the departing process is still alive to settle
    /// its owned work, commit a membership that excludes itself, and then let
    /// the HTTP layer drain the daemon. An odd voter set confirms a surviving
    /// leader before committing; an even voter set commits directly so the new
    /// odd quorum can elect after OpenRaft steps this leader down.
    pub async fn leave_voter(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let node_id = inner.identity.node_id.clone();
        let metrics = inner.client.metrics_db().await?;
        let voters = metrics
            .membership_config
            .voter_ids()
            .collect::<BTreeSet<_>>();
        if !voters.contains(&inner.identity.raft_id) {
            if self.node_is_tombstoned(&node_id).await? {
                self.fence_removed_job_owner(&node_id).await?;
                self.finalize_node_removal(&node_id).await;
                return Ok(());
            }
            return Err(MembershipError::NodeNotFound);
        }
        if voters.len() < 3 {
            return Err(MembershipError::QuorumLoss);
        }
        self.require_removal_capability().await?;

        let mut resolved = self.settle_offline_work(&node_id).await?;
        let remaining = voters
            .iter()
            .copied()
            .filter(|id| *id != inner.identity.raft_id)
            .collect::<BTreeSet<_>>();
        let leader_id = metrics
            .current_leader
            .ok_or_else(|| MembershipError::Internal("cluster has no current leader".to_owned()))?;
        let leader = metrics
            .membership_config
            .membership()
            .get_node(&leader_id)
            .ok_or_else(|| MembershipError::Internal("leader has no node record".to_owned()))?;
        let membership_nodes = metrics
            .membership_config
            .membership()
            .nodes()
            .map(|(raft_id, node)| (*raft_id, node.addr_api.clone()))
            .collect::<Vec<_>>();
        let survivor_nodes = membership_nodes
            .iter()
            .filter(|(raft_id, _)| remaining.contains(raft_id))
            .cloned()
            .collect::<Vec<_>>();
        let mut commit_leader_api = leader.addr_api.clone();

        // OpenRaft 0.9 has no dedicated transfer-leader call: its election
        // trigger only queues one campaign. In an odd voter configuration the
        // survivors can form the old quorum without this leader, so preserve
        // the established handoff-before-commit path. In an even configuration
        // with one survivor unavailable they cannot; the current leader must
        // instead commit OpenRaft's joint then uniform membership change. The
        // resulting odd survivor set can elect after OpenRaft steps the removed
        // leader down.
        let local_leader_sequence =
            (leader_id == inner.identity.raft_id).then(|| leader_self_leave_sequence(voters.len()));
        if local_leader_sequence == Some(LeaderSelfLeaveSequence::HandoffThenCommit) {
            if survivor_nodes.is_empty() {
                return Err(MembershipError::Internal(
                    "cluster has no leadership successor".to_owned(),
                ));
            }
            let mut triggered = false;
            for (_, candidate_api) in &survivor_nodes {
                if trigger_election(candidate_api, &inner.secrets.api)
                    .await
                    .is_ok()
                {
                    triggered = true;
                    break;
                }
            }
            if !triggered {
                return Err(MembershipError::Internal(
                    "no surviving voter accepted the bounded leadership handoff".to_owned(),
                ));
            }
            let new_leader = self
                .await_survivor_leader(
                    inner.identity.raft_id,
                    &remaining,
                    &survivor_nodes,
                    &inner.secrets.api,
                )
                .await?;
            commit_leader_api = survivor_nodes
                .iter()
                .find(|(raft_id, _)| *raft_id == new_leader)
                .map(|(_, api)| api.clone())
                .ok_or_else(|| {
                    MembershipError::Internal("new leader has no node record".to_owned())
                })?;
        }
        // Fence while this voter is still inside the old quorum. The separate
        // pending row survives a crash and keeps the operation retryable while
        // OpenRaft is in a joint or otherwise indeterminate configuration.
        let removal_attempt = self.begin_node_removal(&node_id, false).await?;
        // The fence prevents any later local ownership admission. Settle work
        // that raced with the earlier pass before proposing removal, preserving
        // the active-transfer refusal. A definite failure rolls the fence back
        // so this still-admitted voter remains operational while the operator
        // retries.
        match self.settle_offline_work(&node_id).await {
            Ok(report) => {
                resolved.requeued += report.requeued;
                resolved.failed += report.failed;
            }
            Err(error) => {
                return Err(self
                    .rollback_node_removal_after_failure(&node_id, &removal_attempt, error)
                    .await);
            }
        }
        match request_voter_removal(
            &commit_leader_api,
            &inner.secrets.api,
            inner.identity.raft_id,
        )
        .await
        {
            Ok(()) => {}
            Err(MembershipChangeFailure::Rejected(removal_error)) => {
                return Err(self
                    .rollback_node_removal_after_failure(&node_id, &removal_attempt, removal_error)
                    .await);
            }
            Err(MembershipChangeFailure::Ambiguous(removal_error)) => {
                match reconcile_membership_change(
                    &inner.secrets.api,
                    inner.identity.raft_id,
                    &membership_nodes,
                )
                .await
                {
                    MembershipChangeOutcome::Removed => {
                        tracing::warn!(%removal_error, %node_id, "self-removal committed after an ambiguous HTTP result");
                    }
                    MembershipChangeOutcome::Indeterminate | MembershipChangeOutcome::Promoted => {
                        return Err(MembershipError::Internal(format!(
                        "self-removal outcome is indeterminate after {removal_error}; this voter remains fenced"
                    )));
                    }
                }
            }
        }
        if tokio::time::timeout(FINAL_TOMBSTONE_WAIT, self.finalize_node_removal(&node_id))
            .await
            .is_err()
        {
            // The pending-removal row is already the authoritative durable
            // fence. Do not keep a committed nonmember serving while a final
            // convenience tombstone write waits on the surviving quorum.
            tracing::warn!(
                %node_id,
                "committed self-removal is draining before final tombstone materialization"
            );
        }
        tracing::info!(
            %node_id,
            requeued = resolved.requeued,
            failed = resolved.failed,
            "local voter left the cluster"
        );
        Ok(())
    }

    async fn begin_node_removal(
        &self,
        node_id: &str,
        drain_media: bool,
    ) -> Result<String, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let owner_fence_key = removed_job_owner_key(node_id);
        let attempt_params = if drain_media {
            params!(node_id, attempt_id.as_str())
        } else {
            params!(node_id, attempt_id.as_str(), now)
        };
        let mut statements = vec![
            // Preserve a fence written by a binary that predates attempt
            // references. It may protect an ambiguous proposal and must
            // never become owned by this newer retry.
            (
                "INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) \
                     SELECT $1, 'internal.preexisting' \
                     WHERE EXISTS (SELECT 1 FROM cluster_node_removals WHERE node_id = $1) \
                       AND NOT EXISTS (SELECT 1 FROM cluster_node_removal_attempts \
                         WHERE node_id = $1) \
                     ON CONFLICT(node_id, attempt_id) DO NOTHING"
                    .to_owned(),
                params!(node_id),
            ),
            (begin_removal_attempt_sql(drain_media), attempt_params),
            (
                BEGIN_REMOVAL_INTENT_SQL.to_owned(),
                params!(node_id, attempt_id.as_str()),
            ),
            (
                BEGIN_REMOVAL_FENCE_SQL.to_owned(),
                params!(node_id, now, attempt_id.as_str()),
            ),
            (
                BEGIN_REMOVAL_OWNER_FENCE_SQL.to_owned(),
                params!(
                    owner_fence_key.as_str(),
                    now / 1_000,
                    node_id,
                    attempt_id.as_str()
                ),
            ),
            (
                BEGIN_REMOVAL_JOB_FENCE_SQL.to_owned(),
                params!(now, node_id, attempt_id.as_str()),
            ),
            (
                CLEAR_REMOVAL_INTENT_SQL.to_owned(),
                params!(node_id, attempt_id.as_str()),
            ),
        ];
        if drain_media {
            statements.insert(
                statements.len() - 1,
                (
                    BEGIN_REMOVAL_MEDIA_FENCE_SQL.to_owned(),
                    params!(
                        now,
                        crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                        node_id,
                        attempt_id.as_str()
                    ),
                ),
            );
        }
        let results = inner
            .client
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        if results.get(1).copied() != Some(1) {
            if self.maintenance_operation_pending().await? {
                return Err(MembershipError::MaintenanceConflict(node_id.to_owned()));
            }
            let promotions = inner
                .client
                .query_consistent_map::<CountRow, _>(
                    "SELECT COUNT(*) AS count FROM cluster_node_promotions WHERE node_id = $1",
                    params!(node_id),
                )
                .await?;
            if promotions.first().is_some_and(|row| row.count > 0) {
                return Err(MembershipError::LearnerLifecyclePending(node_id.to_owned()));
            }
            let active_sessions = inner
                .client
                .query_consistent_map::<CountRow, _>(
                    "SELECT COUNT(*) AS count FROM media_sessions \
                     WHERE owner_node_id = $1 AND state = 'active' \
                       AND lease_expires_at_ms > $2",
                    params!(node_id, now),
                )
                .await?;
            if !drain_media && active_sessions.first().is_some_and(|row| row.count > 0) {
                return Err(MembershipError::ActiveMediaSessions);
            }
            return Err(MembershipError::MembershipUpgradeRequired);
        }
        if !self.node_is_tombstoned(node_id).await? {
            return Err(MembershipError::Internal(
                "node removal fence did not become authoritative".to_owned(),
            ));
        }
        Ok(attempt_id)
    }

    /// Reject before settling work or moving leadership when a rolling cluster
    /// still contains a writer that does not protect reference-counted removal
    /// fences. The atomic check in `begin_node_removal` remains authoritative
    /// and closes a heartbeat/version race after this read-only preflight.
    async fn require_removal_capability(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                format!(
                    "SELECT CASE WHEN {} THEN 1 ELSE 0 END AS count",
                    capability_ready_predicate(REMOVAL_ATTEMPT_CAPABILITY)
                ),
                params!(),
            )
            .await?;
        if rows.first().is_some_and(|row| row.count == 1) {
            Ok(())
        } else {
            Err(MembershipError::MembershipUpgradeRequired)
        }
    }

    async fn require_learner_lifecycle_capability(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                format!(
                    "SELECT CASE WHEN {} THEN 1 ELSE 0 END AS count",
                    capability_ready_predicate(LEARNER_LIFECYCLE_CAPABILITY)
                ),
                params!(),
            )
            .await?;
        if rows.first().is_some_and(|row| row.count == 1) {
            Ok(())
        } else {
            Err(MembershipError::MembershipUpgradeRequired)
        }
    }

    /// The protocol range the cluster's replicated features currently require.
    ///
    /// Read through the leader: an activation decision must never be made from
    /// a follower's unapplied copy of `cluster_meta`.
    pub async fn active_protocol_range(&self) -> Result<(i64, i64), MembershipError> {
        self.protocol_range(Read::Quorum).await
    }

    async fn protocol_range(&self, read: Read) -> Result<(i64, i64), MembershipError> {
        let inner = self.replicated_inner()?;
        const SQL: &str = "SELECT protocol_min, protocol_max FROM cluster_meta WHERE singleton = 1";
        let rows = match read {
            Read::Quorum => {
                inner
                    .client
                    .query_consistent_map::<ProtocolRangeRow, _>(SQL, params!())
                    .await?
            }
            Read::Local => {
                inner
                    .client
                    .query_map::<ProtocolRangeRow, _>(SQL, params!())
                    .await?
            }
        };
        let [row] = rows.as_slice() else {
            return Err(MembershipError::Internal(format!(
                "cluster protocol range returned {} rows",
                rows.len()
            )));
        };
        Ok((row.protocol_min, row.protocol_max))
    }

    /// Active nodes whose currently running binary has not proven `capability`.
    async fn nodes_missing_capability(
        &self,
        capability: &str,
    ) -> Result<Vec<String>, MembershipError> {
        self.unready_nodes(capability, Read::Quorum).await
    }

    async fn unready_nodes(
        &self,
        capability: &str,
        read: Read,
    ) -> Result<Vec<String>, MembershipError> {
        let inner = self.replicated_inner()?;
        let sql = capability_unready_nodes_sql(capability);
        let rows = match read {
            Read::Quorum => {
                inner
                    .client
                    .query_consistent_map::<NodeIdRow, _>(sql, params!())
                    .await?
            }
            Read::Local => {
                inner
                    .client
                    .query_map::<NodeIdRow, _>(sql, params!())
                    .await?
            }
        };
        Ok(rows.into_iter().map(|row| row.node_id).collect())
    }

    /// Members of the committed Raft configuration that do not carry a vote.
    async fn committed_non_voters(&self) -> Result<Vec<u64>, MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        Ok(non_voting_members(
            &metrics
                .membership_config
                .voter_ids()
                .collect::<BTreeSet<_>>(),
            metrics.membership_config.nodes().map(|(id, _)| *id),
        ))
    }

    /// These operations commit one `cluster_meta` write. The Hiqlite client
    /// routes writes to the leader on the caller's behalf — as every other
    /// membership mutation here does — so the state an operator can actually
    /// act on is not "you asked the wrong node" but "there is no leader to
    /// route to right now".
    async fn require_elected_leader(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        if metrics.current_leader.is_some() {
            Ok(())
        } else {
            Err(MembershipError::LeaderUnavailable)
        }
    }

    /// The operator-facing projection of the protocol range.
    ///
    /// This is a report, not a decision, and it is reached through the roster
    /// route — which has to keep answering when the cluster has lost quorum,
    /// precisely so an operator can see what the cluster looks like while it is
    /// broken. It therefore reads the applied local state, exactly as the
    /// roster beside it does. Every path that *acts* on the range takes the
    /// quorum read instead.
    pub async fn protocol_status(&self) -> Result<ClusterProtocolStatus, MembershipError> {
        self.protocol_projection(Read::Local).await
    }

    async fn protocol_projection(
        &self,
        read: Read,
    ) -> Result<ClusterProtocolStatus, MembershipError> {
        let (active_min, active_max) = self.protocol_range(read).await?;
        Ok(ClusterProtocolStatus {
            active_min,
            active_max,
            binary_min: AUTH_PROTOCOL_MIN,
            binary_max: AUTH_PROTOCOL_MAX,
            // The learner protocol, named as itself. `AUTH_PROTOCOL_MAX` is
            // the same number today and moves with every future protocol,
            // which is precisely why `AUTH_LEARNER_PROTOCOL` exists.
            learner_protocol_active: (active_min, active_max)
                == (AUTH_LEARNER_PROTOCOL, AUTH_LEARNER_PROTOCOL),
            learner_protocol_pending: self
                .unready_nodes(LEARNER_PROTOCOL_CAPABILITY, read)
                .await?,
        })
    }

    /// Narrow the cluster's active range onto the learner protocol.
    ///
    /// Nothing about deploying this binary activates protocol 5; this call is
    /// the only thing that does, and after it an older binary can no longer
    /// boot, join, or rejoin as a voter. Every precondition is therefore
    /// checked twice on purpose: the read-only pass exists to produce a
    /// refusal that names nodes, and the same predicates are embedded in the
    /// committing statement so a leader that won the reads and then lost a
    /// race cannot commit anyway.
    ///
    /// Three preconditions, because "can this cluster speak protocol 5" has
    /// three ways to be false and only one of them is about capability rows:
    ///
    /// 1. Every active node proves the capability with its *current*
    ///    heartbeat, so a rolled-back binary is caught.
    /// 2. No join is in flight. A node between redeeming its token and its
    ///    first heartbeat is a committed member with no capability row that
    ///    the roster is deliberately blind to; activating past it can leave a
    ///    voter that counts for quorum and can never open its store again.
    /// 3. No member has gone silent. A node that proved the capability and
    ///    then died stays "ready" forever, and a binary rolled back while that
    ///    node is down never writes the desynchronising heartbeat rule 1 is
    ///    built on.
    pub async fn activate_learner_protocol(&self) -> Result<ProtocolChange, MembershipError> {
        let inner = self.replicated_inner()?;
        self.require_elected_leader().await?;
        let (active_min, active_max) = self.active_protocol_range().await?;
        if (active_min, active_max) == (AUTH_LEARNER_PROTOCOL, AUTH_LEARNER_PROTOCOL) {
            return Ok(ProtocolChange {
                changed: false,
                protocol: self.protocol_projection(Read::Quorum).await?,
            });
        }
        if (active_min, active_max) != (AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN) {
            return Err(MembershipError::Internal(format!(
                "cluster protocol range {active_min}..={active_max} is neither the unactivated \
                 range {AUTH_PROTOCOL_MIN}..={AUTH_PROTOCOL_MIN} nor the learner protocol \
                 {AUTH_LEARNER_PROTOCOL}..={AUTH_LEARNER_PROTOCOL}; refusing to narrow it"
            )));
        }
        let absence_cutoff = unix_ms()?.saturating_sub(PROTOCOL_CHANGE_ABSENCE_WINDOW_MS);
        self.refuse_unactivatable_cluster(absence_cutoff).await?;
        let changed = inner
            .client
            .execute(
                narrow_protocol_range_sql(Some(activation_guard_predicate())),
                params!(AUTH_LEARNER_PROTOCOL, AUTH_PROTOCOL_MIN, absence_cutoff),
            )
            .await?;
        let protocol = self.protocol_projection(Read::Quorum).await?;
        if changed == 0 && !protocol.learner_protocol_active {
            // The embedded predicates rejected the write after the read-only
            // pass admitted it: an older binary's heartbeat, a redemption, or
            // a node going quiet landed in between. Re-derive which, and say
            // "retry" rather than naming an empty roster if the range itself
            // moved under us.
            let cutoff = unix_ms()?.saturating_sub(PROTOCOL_CHANGE_ABSENCE_WINDOW_MS);
            self.refuse_unactivatable_cluster(cutoff).await?;
            return Err(MembershipError::ProtocolRangeChanged);
        }
        Ok(ProtocolChange {
            changed: changed == 1,
            protocol,
        })
    }

    /// The read-only pass behind [`Self::activate_learner_protocol`], which
    /// exists to *name* what is in the way. Returns `Ok(())` when nothing is.
    async fn refuse_unactivatable_cluster(&self, cutoff: i64) -> Result<(), MembershipError> {
        // Absence first, deliberately. A join that was abandoned satisfies
        // both rules, and "wait for it to finish" is the wrong thing to tell
        // an operator about a process that is never coming back; "start it or
        // remove it" is the one that ends.
        let absent = self.absent_nodes(cutoff).await?;
        if !absent.is_empty() {
            return Err(MembershipError::LearnerProtocolNodeAbsent {
                nodes: absent,
                minutes: PROTOCOL_CHANGE_ABSENCE_WINDOW_MS / 60_000,
            });
        }
        let joining = self.join_in_flight_nodes().await?;
        if !joining.is_empty() {
            return Err(MembershipError::JoinInFlight(joining));
        }
        let pending = self
            .nodes_missing_capability(LEARNER_PROTOCOL_CAPABILITY)
            .await?;
        if !pending.is_empty() {
            return Err(MembershipError::LearnerProtocolUpgradeRequired(pending));
        }
        Ok(())
    }

    /// Nodes that have redeemed a join token and not yet proven anything.
    async fn join_in_flight_nodes(&self) -> Result<Vec<String>, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<NodeIdRow, _>(join_in_flight_nodes_sql(), params!())
            .await?;
        Ok(rows.into_iter().map(|row| row.node_id).collect())
    }

    /// Members that have not heartbeated since `cutoff`.
    async fn absent_nodes(&self, cutoff: i64) -> Result<Vec<String>, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<NodeIdRow, _>(absent_nodes_sql(), params!(cutoff))
            .await?;
        Ok(rows.into_iter().map(|row| row.node_id).collect())
    }

    /// Widen the cluster back onto protocol 4 for a degraded rollback.
    ///
    /// The plan concedes that activation may be irreversible in some orderings
    /// — once learners exist and hold state, dropping protocol 5 strands them,
    /// and rollback is then a forward fix (upgrade the lagging node instead of
    /// downgrading the cluster). What is implemented here is the reversible
    /// case: no learner is present, so nothing depends on protocol 5 and the
    /// marker can simply be moved back.
    ///
    /// The refusal that protects a learner is carried by the committing
    /// statement itself, as a SQL `NOT EXISTS` over the durable role column.
    /// The Raft-metrics read below is the preflight that *names* what is in
    /// the way, exactly as activation's read-only capability pass does.
    pub async fn deactivate_learner_protocol(&self) -> Result<ProtocolChange, MembershipError> {
        let inner = self.replicated_inner()?;
        self.require_elected_leader().await?;
        let (active_min, active_max) = self.active_protocol_range().await?;
        if (active_min, active_max) == (AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN) {
            return Ok(ProtocolChange {
                changed: false,
                protocol: self.protocol_projection(Read::Quorum).await?,
            });
        }
        if (active_min, active_max) != (AUTH_LEARNER_PROTOCOL, AUTH_LEARNER_PROTOCOL) {
            return Err(MembershipError::Internal(format!(
                "cluster protocol range {active_min}..={active_max} is not the learner protocol \
                 {AUTH_LEARNER_PROTOCOL}..={AUTH_LEARNER_PROTOCOL}; refusing to widen it"
            )));
        }
        self.refuse_stranding_deactivation().await?;
        let changed = inner
            .client
            .execute(
                narrow_protocol_range_sql(Some(no_admitted_learner_predicate().to_owned())),
                params!(AUTH_PROTOCOL_MIN, AUTH_LEARNER_PROTOCOL),
            )
            .await?;
        let protocol = self.protocol_projection(Read::Quorum).await?;
        if changed == 0 && protocol.learner_protocol_active {
            // The embedded predicate rejected the write after the read-only
            // pass admitted it: a learner was admitted in between.
            self.refuse_stranding_deactivation().await?;
            return Err(MembershipError::ProtocolRangeChanged);
        }
        Ok(ProtocolChange {
            changed: changed == 1,
            protocol,
        })
    }

    /// The read-only pass behind [`Self::deactivate_learner_protocol`].
    ///
    /// Two rosters, because they answer two different questions and either one
    /// alone would let an operator strand a node. The replicated role column
    /// names every node *admitted* under protocol 5, including one that is
    /// currently down and therefore absent from nothing. Committed Raft
    /// membership names every member that carries no vote, including one whose
    /// durable row predates this column.
    ///
    /// They are reported separately rather than unioned, because they are not
    /// the same claim. The vendored client joins a voter with `add_learner`
    /// and then `become_member`, so an ordinary voter that is mid-join is a
    /// committed non-voter for a moment — true, and worth refusing on, but it
    /// is not a learner and telling an operator to "remove it" would be wrong.
    async fn refuse_stranding_deactivation(&self) -> Result<(), MembershipError> {
        let admitted = self.admitted_learner_nodes().await?;
        let non_voting = self
            .committed_non_voters()
            .await?
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>();
        if admitted.is_empty() && non_voting.is_empty() {
            return Ok(());
        }
        Err(MembershipError::LearnerProtocolInUse {
            admitted,
            non_voting,
        })
    }

    /// Nodes whose durable membership row says they were admitted as learners.
    async fn admitted_learner_nodes(&self) -> Result<Vec<String>, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<NodeIdRow, _>(admitted_learner_nodes_sql(), params!())
            .await?;
        Ok(rows.into_iter().map(|row| row.node_id).collect())
    }

    /// Restore the durable job-owner fence for state created before that
    /// invariant existed, and invalidate tokens by changing their row identity
    /// even when their revision counter is already exhausted.
    async fn backfill_removed_job_owner_fences(&self) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        inner
            .client
            .txn(vec![
                (
                    "INSERT INTO settings (key, value, updated_at) \
                     SELECT 'internal.cluster_job_owner_removed.' || removed.node_id, '1', $1 \
                     FROM (SELECT node_id FROM cluster_nodes WHERE removed_at IS NOT NULL \
                           UNION SELECT node_id FROM cluster_node_removals) AS removed \
                     WHERE 1 \
                     ON CONFLICT(key) DO UPDATE SET value = '1'"
                        .to_owned(),
                    params!(now / 1_000),
                ),
                (
                    "UPDATE job_leases SET \
                       owner_node_id = 'internal.removed-job-owner:' || owner_node_id, \
                       expires_at_ms = CASE WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END, \
                       revision = CASE WHEN revision < 9223372036854775807 \
                         THEN revision + 1 ELSE revision END, \
                       updated_at_ms = $1 \
                     WHERE EXISTS (SELECT 1 FROM settings \
                       WHERE key = 'internal.cluster_job_owner_removed.' || job_leases.owner_node_id)"
                        .to_owned(),
                    params!(now),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(())
    }

    /// Re-establish a single known removed identity without creating a new
    /// pending-removal record. This is used by idempotent removal calls for
    /// clusters whose tombstones predate the job-owner fence.
    async fn fence_removed_job_owner(&self, node_id: &str) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        let owner_fence_key = removed_job_owner_key(node_id);
        inner
            .client
            .txn(vec![
                (
                    "INSERT INTO settings (key, value, updated_at) VALUES ($1, '1', $2) \
                     ON CONFLICT(key) DO UPDATE SET value = '1', updated_at = excluded.updated_at"
                        .to_owned(),
                    params!(owner_fence_key, now / 1_000),
                ),
                (
                    "UPDATE job_leases SET \
                       owner_node_id = 'internal.removed-job-owner:' || owner_node_id, \
                       expires_at_ms = CASE WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END, \
                       revision = CASE WHEN revision < 9223372036854775807 \
                         THEN revision + 1 ELSE revision END, \
                       updated_at_ms = $1 \
                     WHERE owner_node_id = $2"
                        .to_owned(),
                    params!(now, node_id),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(())
    }

    async fn rollback_node_removal_after_failure(
        &self,
        node_id: &str,
        attempt_id: &str,
        cause: MembershipError,
    ) -> MembershipError {
        match self.rollback_node_removal(node_id, attempt_id).await {
            Ok(RemovalRollbackOutcome::RolledBack) => cause,
            Ok(RemovalRollbackOutcome::Pending) => {
                MembershipError::RemovalPending(cause.to_string())
            }
            Err(rollback_error) => MembershipError::Internal(format!(
                "{cause}; the membership proposal was not sent, but removal-fence rollback \
                 failed: {rollback_error}"
            )),
        }
    }

    async fn rollback_node_removal(
        &self,
        node_id: &str,
        attempt_id: &str,
    ) -> Result<RemovalRollbackOutcome, MembershipError> {
        let inner = self.replicated_inner()?;
        let owner_fence_key = removed_job_owner_key(node_id);
        let results = inner
            .client
            .txn(vec![
                (rollback_removal_attempt_sql(), params!(node_id, attempt_id)),
                (
                    ROLLBACK_REMOVAL_OWNER_FENCE_SQL.to_owned(),
                    params!(owner_fence_key, node_id),
                ),
                (ROLLBACK_REMOVAL_FENCE_SQL.to_owned(), params!(node_id)),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        if results.get(2).copied() != Some(1) {
            tracing::warn!(
                %node_id,
                "removal remains pending because another or legacy attempt still owns the shared fence"
            );
            return Ok(RemovalRollbackOutcome::Pending);
        }
        // If this attempt was released, the remaining statements see the exact
        // same serialized reference set. They remove the shared fences only
        // when no concurrent or ambiguous attempt still owns one.
        Ok(RemovalRollbackOutcome::RolledBack)
    }

    async fn finalize_node_removal(&self, node_id: &str) {
        let Ok(inner) = self.replicated_inner() else {
            return;
        };
        if let Ok(mut keys) = inner.activity_public_keys.lock() {
            keys.remove(node_id);
        }
        if let Ok(mut admission) = inner.activity_auth_admission.lock() {
            admission.remove(node_id);
        }
        if let Ok(mut admission) = inner.internal_auth_admission.lock() {
            admission.remove(node_id);
        }
        if let Ok(mut replays) = inner.internal_auth_replays.lock() {
            replays.remove(node_id);
        }
        if let Ok(mut replays) = inner.internal_read_replays.lock() {
            replays.remove(node_id);
        }
        inner.internal_read_authority.lock().await.remove(node_id);
        if let Err(error) = inner
            .client
            .execute(
                "UPDATE cluster_nodes SET removed_at = COALESCE(removed_at, $1) \
                 WHERE node_id = $2",
                params!(unix_ms().unwrap_or(i64::MAX), node_id),
            )
            .await
        {
            // The pending-removal row remains the authoritative durable fence.
            tracing::error!(%error, %node_id, "could not materialize final node tombstone");
        }
    }

    async fn node_is_tombstoned(&self, node_id: &str) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let rows = inner
            .client
            .query_consistent_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM cluster_nodes \
                 WHERE node_id = $1 AND (removed_at IS NOT NULL OR EXISTS (\
                   SELECT 1 FROM cluster_node_removals WHERE node_id = $1\
                 ))",
                params!(node_id),
            )
            .await?;
        Ok(rows.first().is_some_and(|row| row.count == 1))
    }

    /// Require a stable successor for the pre-commit odd-voter handoff.
    async fn await_survivor_leader(
        &self,
        departed: u64,
        remaining: &BTreeSet<u64>,
        survivor_nodes: &[(u64, String)],
        api_secret: &str,
    ) -> Result<u64, MembershipError> {
        let Ok(client) = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(1))
            .build()
        else {
            return Err(MembershipError::Internal(
                "building leader handoff client".to_owned(),
            ));
        };
        let required = remaining.len() / 2 + 1;
        let mut stable_rounds = 0_u8;
        let mut stable_leader = None;
        let deadline = Instant::now() + SURVIVOR_LEADER_WAIT;
        loop {
            let mut confirmations = BTreeMap::<u64, usize>::new();
            let observations =
                futures_util::future::join_all(survivor_nodes.iter().map(|(_, api)| {
                    let client = client.clone();
                    async move {
                        let response = client
                            .get(format!("https://{api}/cluster/metrics/sqlite"))
                            .header("X-API-SECRET", api_secret)
                            .header(reqwest::header::ACCEPT, "application/json")
                            .send()
                            .await;
                        match response {
                            Ok(response) => response
                                .json::<LeaderMetrics>()
                                .await
                                .ok()
                                .and_then(|metrics| metrics.current_leader),
                            Err(_) => None,
                        }
                    }
                }))
                .await;
            for leader in observations {
                if let Some(leader) =
                    leader.filter(|leader| *leader != departed && remaining.contains(leader))
                {
                    *confirmations.entry(leader).or_default() += 1;
                }
            }
            let elected = confirmations
                .into_iter()
                .find_map(|(leader, count)| (count >= required).then_some(leader));
            if elected.is_some() {
                if elected == stable_leader {
                    stable_rounds += 1;
                } else {
                    stable_leader = elected;
                    stable_rounds = 1;
                }
                if stable_rounds >= 3 {
                    return stable_leader.ok_or_else(|| {
                        MembershipError::Internal("leader handoff lost its winner".to_owned())
                    });
                }
            } else {
                stable_rounds = 0;
                stable_leader = None;
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(MembershipError::Internal(format!(
            "leadership did not move away from Raft voter {departed}"
        )))
    }

    /// Resolve the node's offline work, then keep going until a fresh read
    /// finds nothing left to resolve.
    ///
    /// One snapshot only describes the work that existed when the plan was
    /// drawn. The departing node keeps serving its HTTP API throughout, and a
    /// package is created owned by whichever node answered the request — so a
    /// download requested during the bounded probe wait would exit the removal
    /// owned by a node that no longer exists, holding its reservation until
    /// the seven-day expiry. That is precisely the stranded state §6.7 calls
    /// an activation blocker, so the plan is re-read rather than trusted.
    ///
    /// Rounds are bounded because each one is itself bounded by [`PROBE_WAIT`]:
    /// a node that keeps admitting new downloads faster than they can be
    /// resolved gets a refusal naming that reason, not an unbounded wait.
    async fn settle_offline_work(
        &self,
        node_id: &str,
    ) -> Result<OfflineRemovalReport, MembershipError> {
        let inner = self.replicated_inner()?;
        let mut total = OfflineRemovalReport::default();
        let mut round = 0_u32;
        loop {
            let report = self.resolve_offline_work(node_id).await?;
            total.requeued += report.requeued;
            total.failed += report.failed;
            round += 1;
            let remaining = inner
                .store
                .unresolved_offline_packages(node_id)
                .await
                .map_err(|error| MembershipError::Internal(error.to_string()))?
                .len();
            if remaining == 0 {
                return Ok(total);
            }
            if round >= OFFLINE_RESOLVE_ROUNDS {
                return Err(MembershipError::OfflineWork(format!(
                    "{remaining} offline download(s) were requested on this node while its \
                     existing work was being resolved; stop sending it new downloads and retry \
                     the removal"
                )));
            }
        }
    }

    /// Resolve every offline package the departing node owns, per §6.7.
    ///
    /// The rule has exactly two allowed outcomes and one allowed refusal:
    ///
    /// * **Requeue** a `queued` or `preparing` package on a survivor that
    ///   answered a source probe by actually opening the snapshotted bytes.
    /// * **Fail** it with `node_removed` when no survivor proved that, which
    ///   releases the reservation so the client can simply ask again.
    /// * **Refuse** the removal while a client is mid-transfer, because
    ///   cutting a download off is neither of the two outcomes above.
    ///
    /// A `ready` package is never requeued. Its bytes exist only in the
    /// departing node's cache, and §7 non-goal 4 explicitly declines to
    /// promise byte-identical transcodes across mixed encoders — so a
    /// re-produced package behind the same stable lease URL could hand a
    /// resuming downloader a different generation's bytes. Failing it stops
    /// the cluster advertising a promise it can no longer keep, and the
    /// client's retry gets a fresh package with a fresh URL.
    async fn resolve_offline_work(
        &self,
        node_id: &str,
    ) -> Result<OfflineRemovalReport, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_seconds()?;
        let packages = inner
            .store
            .unresolved_offline_packages(node_id)
            .await
            .map_err(|error| MembershipError::Internal(error.to_string()))?;
        if packages.is_empty() {
            return Ok(OfflineRemovalReport::default());
        }

        let in_flight = inner
            .store
            .offline_transfers_in_flight(node_id, now, now - TRANSFER_ACTIVE_SECONDS)
            .await
            .map_err(|error| MembershipError::Internal(error.to_string()))?;
        if in_flight > 0 {
            return Err(MembershipError::OfflineWork(format!(
                "{in_flight} offline download(s) are still transferring from this node; \
                 retry the removal once they finish or delete those packages"
            )));
        }

        // Only `queued` and `preparing` work can move, so only that work is
        // worth asking about. Candidates are the other nodes still in the
        // roster; the departing node is excluded because its answer is about
        // to stop being true.
        let movable = packages
            .iter()
            .filter(|package| package.state == "queued" || package.state == "preparing")
            .map(|package| package.id.clone())
            .collect::<Vec<_>>();
        let candidates = self
            .status()
            .await?
            .nodes
            .iter()
            .filter(|node| node.node_id != node_id && node.reachable)
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>();

        let requested_at = unix_seconds()?;
        if !movable.is_empty() && !candidates.is_empty() {
            inner
                .store
                .request_offline_source_probes(&movable, &candidates, requested_at)
                .await
                .map_err(|error| MembershipError::Internal(error.to_string()))?;
            self.await_source_probes(requested_at).await?;
        }

        let mut plan = Vec::with_capacity(packages.len());
        for package in &packages {
            let requeue_to = if movable.contains(&package.id) {
                inner
                    .store
                    .verified_offline_source_nodes(&package.id, requested_at)
                    .await
                    .map_err(|error| MembershipError::Internal(error.to_string()))?
                    .into_iter()
                    .next()
            } else {
                None
            };
            plan.push(OfflineRemovalPlanEntry {
                package_id: package.id.clone(),
                requeue_to,
            });
        }

        inner
            .store
            .resolve_offline_packages_for_removal(node_id, &plan, unix_seconds()?)
            .await
            .map_err(|error| {
                MembershipError::OfflineWork(format!(
                    "this node's offline work changed while it was being resolved, so the \
                     removal was refused rather than committing on a stale plan; retry it: \
                     {error}"
                ))
            })
    }

    /// Give the surviving nodes a bounded window to answer, then stop waiting.
    ///
    /// Not answering is a valid answer: a node that is wedged, busy, or simply
    /// does not have the mount fails to prove anything, and an unproven node
    /// is not a re-homing target. The wait exists to give a healthy node time
    /// to reply, not to keep the operator hanging until every node agrees.
    async fn await_source_probes(&self, requested_at: i64) -> Result<(), MembershipError> {
        let inner = self.replicated_inner()?;
        let deadline = tokio::time::Instant::now() + PROBE_WAIT;
        loop {
            let outstanding = inner
                .store
                .outstanding_offline_source_probes(requested_at)
                .await
                .map_err(|error| MembershipError::Internal(error.to_string()))?;
            if outstanding == 0 || tokio::time::Instant::now() >= deadline {
                return Ok(());
            }
            tokio::time::sleep(PROBE_POLL).await;
        }
    }

    /// The node-side half of the removal protocol: answer every source probe
    /// addressed to this node by actually reading the bytes.
    ///
    /// Every node runs this continuously, not just during a removal, because
    /// the asking node cannot reach into a peer's filesystem and a probe is
    /// only useful if somebody is listening when it is asked.
    pub async fn answer_offline_source_probes(&self) -> Result<u64, MembershipError> {
        let Some(inner) = self.inner.as_ref() else {
            return Ok(0);
        };
        let now = unix_seconds()?;
        let pending = inner
            .store
            .pending_offline_source_probes(&inner.identity.node_id, now - PROBE_REQUEST_TTL)
            .await
            .map_err(|error| MembershipError::Internal(error.to_string()))?;
        let mut answered = 0;
        for package in pending {
            let package_id = package.id.clone();
            let readable = tokio::task::spawn_blocking(move || source_is_readable(&package))
                .await
                .map_err(|error| MembershipError::Internal(error.to_string()))?;
            if inner
                .store
                .answer_offline_source_probe(
                    &package_id,
                    &inner.identity.node_id,
                    readable,
                    unix_seconds()?,
                )
                .await
                .map_err(|error| MembershipError::Internal(error.to_string()))?
            {
                answered += 1;
            }
        }
        Ok(answered)
    }

    /// Poll for source probes far more often than the heartbeat ticks.
    ///
    /// A removal blocks an operator while it waits for these answers, so the
    /// cadence is the responsiveness of node removal itself, not a background
    /// housekeeping interval. Idle passes cost one indexed lookup that
    /// normally returns nothing.
    pub async fn offline_source_probe_loop(self) {
        if self.inner.is_none() {
            return;
        }
        loop {
            tokio::time::sleep(PROBE_POLL).await;
            match self.answer_offline_source_probes().await {
                Ok(0) => {}
                Ok(answered) => {
                    tracing::info!(
                        answered,
                        "answered offline source probes for a node removal"
                    )
                }
                Err(error) => {
                    tracing::warn!(code = error.code(), "offline source probe pass failed")
                }
            }
        }
    }
}

/// Positive proof that *this* machine can read a package's exact source.
///
/// The whole point of the probe protocol is that a replicated `source_path` is
/// a string, not a mount. So this opens the file and reads from it rather than
/// asking whether the path exists: a stale automount entry, a directory, and a
/// permission-denied share all "exist".
///
/// Size and mtime must match the snapshot the package took at request time for
/// the same reason [`crate::store::OfflinePackageStore::create_offline_package`]
/// snapshots them: a survivor holding a *different* file at the same path is
/// not an equivalent source, and re-homing onto it would silently prepare
/// different media than the traveller asked for.
///
/// Blocking. Callers hand it to a blocking pool — a dead NAS can hold an
/// `open` for a long time, and that must not stall an async runtime.
#[must_use]
pub fn source_is_readable(package: &OfflinePackage) -> bool {
    use std::io::Read as _;

    let Ok(metadata) = std::fs::metadata(&package.source_path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != package.source_size as u64 {
        return false;
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs());
    if modified != Some(package.source_mtime as u64) {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(&package.source_path) else {
        return false;
    };
    let mut probe = [0_u8; 1];
    file.read(&mut probe).is_ok()
}

/// Decode a join token of any protocol version this binary implements.
///
/// The framing is checked before anything is decrypted: a token whose prefix
/// this build does not know is refused without a key ever being derived, which
/// is what makes an older coordinator or joiner reject the v2 flow rather than
/// misread it. `plxjoin:v3` is refused here by the same rule, on this binary.
pub fn decode_join_token(token: &str) -> Result<JoinToken, MembershipError> {
    let mut parts = token.split(':');
    if parts.next() != Some("plxjoin") {
        return Err(MembershipError::InvalidToken);
    }
    let framing = parts.next().ok_or(MembershipError::InvalidToken)?;
    if parts.clone().count() != 2 {
        return Err(MembershipError::InvalidToken);
    }
    let key_bytes = hex::decode(parts.next().ok_or(MembershipError::InvalidToken)?)
        .map_err(|_| MembershipError::InvalidToken)?;
    let encrypted = hex::decode(parts.next().ok_or(MembershipError::InvalidToken)?)
        .map_err(|_| MembershipError::InvalidToken)?;
    if key_bytes.len() != 32 || encrypted.len() <= 24 {
        return Err(MembershipError::InvalidToken);
    }
    let aad = match framing {
        "v1" => JOIN_TOKEN_AAD,
        "v2" => JOIN_TOKEN_V2_AAD,
        _ => return Err(MembershipError::InvalidToken),
    };
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key_bytes));
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&encrypted[..24]),
            chacha20poly1305::aead::Payload {
                msg: &encrypted[24..],
                aad,
            },
        )
        .map_err(|_| MembershipError::InvalidToken)?;
    let decoded = match framing {
        "v1" => {
            let payload: JoinPayload =
                serde_json::from_slice(&plaintext).map_err(|_| MembershipError::InvalidToken)?;
            if payload.version != JOIN_TOKEN_VERSION {
                return Err(MembershipError::InvalidToken);
            }
            JoinToken::V1(payload)
        }
        _ => {
            let payload: JoinPayloadV2 =
                serde_json::from_slice(&plaintext).map_err(|_| MembershipError::InvalidToken)?;
            if payload.version != JOIN_TOKEN_V2_VERSION
                || payload.protocol_min <= 0
                || payload.protocol_max < payload.protocol_min
            {
                return Err(MembershipError::InvalidToken);
            }
            JoinToken::V2(payload)
        }
    };
    if decoded.cluster_id().trim().is_empty() {
        return Err(MembershipError::InvalidToken);
    }
    Ok(decoded)
}

fn encode_join_token(payload: &JoinPayload) -> Result<String, MembershipError> {
    seal_join_token(payload, JOIN_TOKEN_PREFIX, JOIN_TOKEN_AAD)
}

fn encode_join_token_v2(payload: &JoinPayloadV2) -> Result<String, MembershipError> {
    seal_join_token(payload, JOIN_TOKEN_V2_PREFIX, JOIN_TOKEN_V2_AAD)
}

fn seal_join_token<T: Serialize>(
    payload: &T,
    prefix: &str,
    aad: &[u8],
) -> Result<String, MembershipError> {
    let mut key_bytes = [0_u8; 32];
    let mut nonce = [0_u8; 24];
    getrandom::getrandom(&mut key_bytes)
        .map_err(|error| MembershipError::Internal(error.to_string()))?;
    getrandom::getrandom(&mut nonce)
        .map_err(|error| MembershipError::Internal(error.to_string()))?;
    let plaintext = serde_json::to_vec(payload)
        .map_err(|error| MembershipError::Internal(error.to_string()))?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key_bytes));
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            chacha20poly1305::aead::Payload {
                msg: &plaintext,
                aad,
            },
        )
        .map_err(|_| MembershipError::Internal("encrypting join token".to_owned()))?;
    let mut encrypted = nonce.to_vec();
    encrypted.extend(ciphertext);
    Ok(format!(
        "{prefix}:{}:{}",
        hex::encode(key_bytes),
        hex::encode(encrypted)
    ))
}

#[must_use]
pub fn join_token_digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn is_join_token_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MembershipChangeOutcome {
    Removed,
    Promoted,
    Indeterminate,
}

enum MembershipChangeFailure {
    /// The request could not be constructed before any proposal was sent, so
    /// the caller can roll its pending removal fence back.
    Rejected(MembershipError),
    /// Transport failure or timeout after send. The proposal may still commit;
    /// the fence must remain until a retry proves the uniform new membership.
    Ambiguous(MembershipError),
}

#[derive(Serialize)]
struct RemoveVoterRequest {
    remove_voter: u64,
}

#[derive(Serialize)]
struct PromoteLearnerRequest<'a> {
    node_id: u64,
    addr_api: &'a str,
    addr_raft: &'a str,
}

#[derive(Serialize)]
struct RemoveLearnerRequest {
    node_id: u64,
    stay_as_learner: bool,
}

/// Resolve an ambiguous membership HTTP result from independent survivor
/// observations. A response timeout is not a failed Raft commit, and an old
/// membership observation cannot prove that an accepted proposal will not
/// commit later. The leader applies a node delta under Hiqlite's membership
/// lock, so a uniform quorum that excludes the target proves removal even if a
/// concurrently admitted voter changed the final set.
async fn reconcile_membership_change(
    api_secret: &str,
    departed: u64,
    membership_nodes: &[(u64, String)],
) -> MembershipChangeOutcome {
    let Ok(client) = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(1))
        .build()
    else {
        return MembershipChangeOutcome::Indeterminate;
    };
    let mut stable_rounds = 0_u8;
    for _ in 0..40 {
        let observations =
            futures_util::future::join_all(membership_nodes.iter().map(|(raft_id, api)| {
                let client = client.clone();
                let raft_id = *raft_id;
                async move {
                    let response = client
                        .get(format!("https://{api}/cluster/metrics/sqlite"))
                        .header("X-API-SECRET", api_secret)
                        .header(reqwest::header::ACCEPT, "application/json")
                        .send()
                        .await;
                    let voters = match response {
                        Ok(response) => response
                            .json::<RemoteMembershipMetrics>()
                            .await
                            .ok()
                            .and_then(RemoteMembershipMetrics::uniform_voters),
                        Err(_) => None,
                    };
                    (raft_id, voters)
                }
            }))
            .await;
        if quorum_confirms_removal(departed, &observations) {
            stable_rounds += 1;
        } else {
            stable_rounds = 0;
        }
        if stable_rounds >= 3 {
            return MembershipChangeOutcome::Removed;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    MembershipChangeOutcome::Indeterminate
}

async fn reconcile_promotion_change(
    api_secret: &str,
    promoted: u64,
    membership_nodes: &[(u64, String)],
) -> MembershipChangeOutcome {
    let Ok(client) = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(1))
        .build()
    else {
        return MembershipChangeOutcome::Indeterminate;
    };
    let mut stable_rounds = 0_u8;
    for _ in 0..40 {
        let observations =
            futures_util::future::join_all(membership_nodes.iter().map(|(raft_id, api)| {
                let client = client.clone();
                let raft_id = *raft_id;
                async move {
                    let response = client
                        .get(format!("https://{api}/cluster/metrics/sqlite"))
                        .header("X-API-SECRET", api_secret)
                        .header(reqwest::header::ACCEPT, "application/json")
                        .send()
                        .await;
                    let voters = match response {
                        Ok(response) => response
                            .json::<RemoteMembershipMetrics>()
                            .await
                            .ok()
                            .and_then(RemoteMembershipMetrics::uniform_voters),
                        Err(_) => None,
                    };
                    (raft_id, voters)
                }
            }))
            .await;
        if quorum_confirms_promotion(promoted, &observations) {
            stable_rounds += 1;
        } else {
            stable_rounds = 0;
        }
        if stable_rounds >= 3 {
            return MembershipChangeOutcome::Promoted;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    MembershipChangeOutcome::Indeterminate
}

async fn reconcile_member_removal(
    api_secret: &str,
    removed: u64,
    membership_nodes: &[(u64, String)],
) -> MembershipChangeOutcome {
    let Ok(client) = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(1))
        .build()
    else {
        return MembershipChangeOutcome::Indeterminate;
    };
    let mut stable_rounds = 0_u8;
    for _ in 0..40 {
        let observations =
            futures_util::future::join_all(membership_nodes.iter().map(|(raft_id, api)| {
                let client = client.clone();
                let raft_id = *raft_id;
                async move {
                    let membership = match client
                        .get(format!("https://{api}/cluster/metrics/sqlite"))
                        .header("X-API-SECRET", api_secret)
                        .header(reqwest::header::ACCEPT, "application/json")
                        .send()
                        .await
                    {
                        Ok(response) => response
                            .json::<RemoteMembershipMetrics>()
                            .await
                            .ok()
                            .and_then(RemoteMembershipMetrics::uniform_membership),
                        Err(_) => None,
                    };
                    (raft_id, membership)
                }
            }))
            .await;
        if quorum_confirms_member_removal(removed, &observations) {
            stable_rounds += 1;
        } else {
            stable_rounds = 0;
        }
        if stable_rounds >= 3 {
            return MembershipChangeOutcome::Removed;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    MembershipChangeOutcome::Indeterminate
}

fn quorum_confirms_removal(departed: u64, observations: &[(u64, Option<BTreeSet<u64>>)]) -> bool {
    let mut confirmations = BTreeMap::<&BTreeSet<u64>, usize>::new();
    for (observer, observed) in observations {
        let Some(voters) = observed else {
            continue;
        };
        if voters.contains(&departed) || !voters.contains(observer) {
            continue;
        }
        *confirmations.entry(voters).or_default() += 1;
    }
    confirmations
        .into_iter()
        .any(|(voters, count)| count > voters.len() / 2)
}

fn quorum_confirms_promotion(promoted: u64, observations: &[(u64, Option<BTreeSet<u64>>)]) -> bool {
    let mut confirmations = BTreeMap::<&BTreeSet<u64>, usize>::new();
    for (observer, observed) in observations {
        let Some(voters) = observed else {
            continue;
        };
        if !voters.contains(&promoted) || !voters.contains(observer) {
            continue;
        }
        *confirmations.entry(voters).or_default() += 1;
    }
    confirmations
        .into_iter()
        .any(|(voters, count)| count > voters.len() / 2)
}

type MemberSetObservation = (u64, Option<(BTreeSet<u64>, BTreeSet<u64>)>);

fn quorum_confirms_member_removal(removed: u64, observations: &[MemberSetObservation]) -> bool {
    let mut confirmations = BTreeMap::<(BTreeSet<u64>, BTreeSet<u64>), usize>::new();
    for (observer, observed) in observations {
        let Some((voters, members)) = observed else {
            continue;
        };
        if members.contains(&removed) || !voters.contains(observer) {
            continue;
        }
        *confirmations
            .entry((voters.clone(), members.clone()))
            .or_default() += 1;
    }
    confirmations
        .into_iter()
        .any(|((voters, _), count)| count > voters.len() / 2)
}

async fn request_voter_removal(
    leader_api: &str,
    api_secret: &str,
    remove_voter: u64,
) -> Result<(), MembershipChangeFailure> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| {
            MembershipChangeFailure::Rejected(MembershipError::Internal(error.to_string()))
        })?;
    let response = client
        .post(format!("https://{leader_api}/cluster/membership/sqlite"))
        .header("X-API-SECRET", api_secret)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&RemoveVoterRequest { remove_voter })
        .send()
        .await
        .map_err(|error| {
            MembershipChangeFailure::Ambiguous(MembershipError::Internal(error.to_string()))
        })?;
    if !response.status().is_success() {
        return Err(membership_response_failure(response.status()));
    }
    Ok(())
}

async fn request_learner_promotion(
    leader_api: &str,
    api_secret: &str,
    node: &Node,
) -> Result<(), MembershipChangeFailure> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| {
            MembershipChangeFailure::Rejected(MembershipError::Internal(error.to_string()))
        })?;
    let response = client
        .post(format!("https://{leader_api}/cluster/become_member/sqlite"))
        .header("X-API-SECRET", api_secret)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&PromoteLearnerRequest {
            node_id: node.id,
            addr_api: &node.addr_api,
            addr_raft: &node.addr_raft,
        })
        .send()
        .await
        .map_err(|error| {
            MembershipChangeFailure::Ambiguous(MembershipError::Internal(error.to_string()))
        })?;
    if !response.status().is_success() {
        return Err(membership_response_failure(response.status()));
    }
    Ok(())
}

async fn request_learner_removal(
    leader_api: &str,
    api_secret: &str,
    node_id: u64,
) -> Result<(), MembershipChangeFailure> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| {
            MembershipChangeFailure::Rejected(MembershipError::Internal(error.to_string()))
        })?;
    let response = client
        .delete(format!("https://{leader_api}/cluster/membership/sqlite"))
        .header("X-API-SECRET", api_secret)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&RemoveLearnerRequest {
            node_id,
            stay_as_learner: false,
        })
        .send()
        .await
        .map_err(|error| {
            MembershipChangeFailure::Ambiguous(MembershipError::Internal(error.to_string()))
        })?;
    if !response.status().is_success() {
        return Err(membership_response_failure(response.status()));
    }
    Ok(())
}

fn membership_response_failure(status: reqwest::StatusCode) -> MembershipChangeFailure {
    // Hiqlite can return an error after OpenRaft has already committed the
    // joint half of its two-step membership change. No post-send HTTP status
    // is proof that rollback is safe.
    MembershipChangeFailure::Ambiguous(MembershipError::Internal(format!(
        "Hiqlite voter reconfiguration returned HTTP {status}"
    )))
}

async fn trigger_election(candidate_api: &str, api_secret: &str) -> Result<(), MembershipError> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(1))
        .build()
        .map_err(|error| MembershipError::Internal(error.to_string()))?;
    let response = client
        .post(format!("https://{candidate_api}/cluster/elect/sqlite"))
        .header("X-API-SECRET", api_secret)
        .send()
        .await
        .map_err(|error| MembershipError::Internal(error.to_string()))?;
    if !response.status().is_success() {
        return Err(MembershipError::Internal(format!(
            "Hiqlite refused leader handoff with HTTP {}",
            response.status()
        )));
    }
    Ok(())
}

fn unix_ms() -> Result<i64, MembershipError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| MembershipError::Internal(error.to_string()))?
        .as_millis();
    i64::try_from(millis).map_err(|_| MembershipError::Internal("clock overflow".to_owned()))
}

fn unix_seconds() -> Result<i64, MembershipError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| MembershipError::Internal(error.to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| MembershipError::Internal("clock overflow".to_owned()))
}

/// Prove that the authoritative root can durably publish and remove a file.
/// A learner may replicate on storage that is merely writable; promotion is
/// the point where that machine becomes part of the quorum's durability
/// promise, so the proof is retained separately from current free space.
fn voter_storage_durability_probe(root: &Path) -> bool {
    let name = format!(".plurx-voter-preflight-{}", uuid::Uuid::new_v4());
    let path = root.join(name);
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(root)?;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        file.write_all(b"plurx voter durability preflight\n")?;
        file.sync_all()?;
        drop(file);
        std::fs::remove_file(&path)?;
        std::fs::File::open(root)?.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&path);
        tracing::warn!(%error, root = %root.display(), "voter storage durability preflight failed");
        false
    } else {
        true
    }
}

#[cfg(unix)]
fn available_storage_headroom_bytes(root: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(root.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // Safety: the CString remains live through the call and a successful
    // statvfs initializes the complete output structure.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    // Safety: guarded by the successful return above.
    let stats = unsafe { stats.assume_init() };
    let fragment = if stats.f_frsize == 0 {
        stats.f_bsize
    } else {
        stats.f_frsize
    } as u128;
    Some(
        (stats.f_bavail as u128)
            .saturating_mul(fragment)
            .min(u64::MAX as u128) as u64,
    )
}

#[cfg(not(unix))]
fn available_storage_headroom_bytes(_root: &Path) -> Option<u64> {
    None
}

struct MembershipSchemaRow {
    schema_version: i64,
}

impl From<&mut Row<'_>> for MembershipSchemaRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            schema_version: row.get("schema_version"),
        }
    }
}

struct MaxRaftIdRow {
    max_raft_id: Option<i64>,
}

impl From<&mut Row<'_>> for MaxRaftIdRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            max_raft_id: row.get("max_raft_id"),
        }
    }
}

struct JoinTokenRow {
    raft_id: i64,
    expires_at: i64,
    state: String,
    node_id: Option<String>,
    /// What this token admits. `NULL` on every row an older coordinator wrote,
    /// which is exactly the set of tokens that can only admit a voter.
    role: Option<String>,
}

impl JoinTokenRow {
    fn role(&self) -> Result<ClusterRole, MembershipError> {
        ClusterRole::from_stored(self.role.as_deref())
    }
}

impl From<&mut Row<'_>> for JoinTokenRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            raft_id: row.get("raft_id"),
            expires_at: row.get("expires_at"),
            state: row.get("state"),
            node_id: row.get("node_id"),
            role: row.get("role"),
        }
    }
}

struct RedeemingNodeRow {
    raft_id: i64,
    raft_address: String,
    api_address: String,
    removed_at: Option<i64>,
    removal_pending: bool,
}

impl From<&mut Row<'_>> for RedeemingNodeRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            raft_id: row.get("raft_id"),
            raft_address: row.get("raft_address"),
            api_address: row.get("api_address"),
            removed_at: row.get("removed_at"),
            removal_pending: row.get("removal_pending"),
        }
    }
}

struct TargetNodeRow {
    raft_id: i64,
}

struct PromotionTargetRow {
    raft_id: i64,
    admitted_role: Option<String>,
    last_seen_at: i64,
    removal_pending: bool,
    last_applied_index: Option<i64>,
    apply_lag_entries: Option<i64>,
    bounded_read_ready: bool,
    voter_storage_ready: bool,
    storage_headroom_bytes: Option<i64>,
    storage_probe_observed_at: Option<i64>,
    voter_role_persisted: bool,
    observed_at: Option<i64>,
}

struct RemovalAttemptRow {
    attempt_id: String,
}

struct PromotionAttemptRow {
    attempt_id: String,
}

struct MembershipNodeRow {
    node_id: String,
    raft_id: i64,
    api_address: String,
    hostname: String,
    admitted_role: Option<String>,
    last_seen_at: i64,
    removal_pending: bool,
    apply_lag_entries: Option<i64>,
    bounded_read_ready: bool,
    voter_storage_ready: bool,
    storage_headroom_bytes: Option<i64>,
    storage_probe_observed_at: Option<i64>,
    progress_observed_at: Option<i64>,
    maintenance_requested_at: Option<i64>,
    maintenance_acknowledged_at: Option<i64>,
    maintenance_capable: bool,
    active_media_sessions: i64,
}

struct MembershipMetricsRow {
    raft_id: u64,
    last_seen_at: i64,
    removal_pending: bool,
}

struct ActivityPeerRow {
    node_id: String,
    raft_id: u64,
    last_seen_at: i64,
    http_base: Option<String>,
}

struct NodeHostnameRow {
    node_id: String,
    hostname: String,
    api_address: String,
}

impl From<&mut Row<'_>> for NodeHostnameRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            node_id: row.get("node_id"),
            hostname: row.get("hostname"),
            api_address: row.get("api_address"),
        }
    }
}

struct MediaPeerRow {
    node_id: String,
    raft_id: u64,
    last_seen_at: i64,
    http_base: Option<String>,
    bounded_read_ready: bool,
    observed_at: Option<i64>,
}

impl From<&mut Row<'_>> for MediaPeerRow {
    fn from(row: &mut Row<'_>) -> Self {
        let raft_id: i64 = row.get("raft_id");
        Self {
            node_id: row.get("node_id"),
            raft_id: u64::try_from(raft_id).unwrap_or_default(),
            last_seen_at: row.get("last_seen_at"),
            http_base: row.get("public_http_url"),
            bounded_read_ready: row.get("bounded_read_ready"),
            observed_at: row.get("observed_at"),
        }
    }
}

impl From<&mut Row<'_>> for ActivityPeerRow {
    fn from(row: &mut Row<'_>) -> Self {
        let raft_id: i64 = row.get("raft_id");
        Self {
            node_id: row.get("node_id"),
            raft_id: u64::try_from(raft_id).unwrap_or_default(),
            last_seen_at: row.get("last_seen_at"),
            http_base: row.get("public_http_url"),
        }
    }
}

struct ActivityPublicKeyRow {
    node_id: String,
    public_key: String,
}

impl From<&mut Row<'_>> for ActivityPublicKeyRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            node_id: row.get("node_id"),
            public_key: row.get("public_key"),
        }
    }
}

struct ActivityAuthNodeRow {
    raft_id: u64,
    last_seen_at: i64,
}

impl From<&mut Row<'_>> for ActivityAuthNodeRow {
    fn from(row: &mut Row<'_>) -> Self {
        let raft_id: i64 = row.get("raft_id");
        Self {
            raft_id: u64::try_from(raft_id).unwrap_or_default(),
            last_seen_at: row.get("last_seen_at"),
        }
    }
}

struct HttpUrlRow {
    public_http_url: String,
}

struct ArtworkRepairLeaseRow {
    owner_node_id: String,
    leader_term: i64,
    generation: i64,
}

struct CountRow {
    count: i64,
}

struct ProtocolRangeRow {
    protocol_min: i64,
    protocol_max: i64,
}

impl From<&mut Row<'_>> for ProtocolRangeRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            protocol_min: row.get("protocol_min"),
            protocol_max: row.get("protocol_max"),
        }
    }
}

struct NodeIdRow {
    node_id: String,
}

impl From<&mut Row<'_>> for NodeIdRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            node_id: row.get("node_id"),
        }
    }
}

#[derive(Deserialize)]
struct LeaderMetrics {
    current_leader: Option<u64>,
}

#[derive(Deserialize)]
struct RemoteMembershipMetrics {
    membership_config: RemoteStoredMembership,
}

#[derive(Deserialize)]
struct RemoteStoredMembership {
    membership: RemoteMembership,
}

#[derive(Deserialize)]
struct RemoteMembership {
    configs: Vec<BTreeSet<u64>>,
    nodes: BTreeMap<u64, serde_json::Value>,
}

impl RemoteMembershipMetrics {
    fn uniform_voters(mut self) -> Option<BTreeSet<u64>> {
        (self.membership_config.membership.configs.len() == 1)
            .then(|| self.membership_config.membership.configs.remove(0))
    }

    fn uniform_membership(mut self) -> Option<(BTreeSet<u64>, BTreeSet<u64>)> {
        (self.membership_config.membership.configs.len() == 1).then(|| {
            (
                self.membership_config.membership.configs.remove(0),
                self.membership_config
                    .membership
                    .nodes
                    .into_keys()
                    .collect(),
            )
        })
    }
}

impl From<&mut Row<'_>> for CountRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            count: row.get("count"),
        }
    }
}

fn activity_auth_message(node_id: &str, target_node_id: &str, timestamp_ms: i64) -> Vec<u8> {
    let mut message = ACTIVITY_AUTH_CONTEXT.to_vec();
    for value in [node_id, target_node_id] {
        message.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
        message.extend_from_slice(value.as_bytes());
    }
    message.extend_from_slice(&timestamp_ms.to_be_bytes());
    message
}

fn internal_peer_auth_message(
    node_id: &str,
    target_node_id: &str,
    timestamp_ms: i64,
    nonce: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> Option<Vec<u8>> {
    if !canonical_internal_auth_nonce(nonce)
        || method.is_empty()
        || method.len() > 16
        || !method.bytes().all(|byte| byte.is_ascii_uppercase())
        || !path.starts_with('/')
        || path.len() > 512
        || path.contains('?')
        || path.contains('#')
        || path.chars().any(char::is_control)
    {
        return None;
    }
    let body_digest = Sha256::digest(body);
    let mut message = INTERNAL_PEER_AUTH_CONTEXT.to_vec();
    for value in [node_id, target_node_id, nonce, method, path] {
        message.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
        message.extend_from_slice(value.as_bytes());
    }
    message.extend_from_slice(&timestamp_ms.to_be_bytes());
    message.extend_from_slice(&body_digest);
    Some(message)
}

fn canonical_internal_auth_nonce(nonce: &str) -> bool {
    nonce.len() == INTERNAL_AUTH_NONCE_BYTES
        && uuid::Uuid::parse_str(nonce).is_ok_and(|parsed| parsed.hyphenated().to_string() == nonce)
}

impl From<&mut Row<'_>> for HttpUrlRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            public_http_url: row.get("public_http_url"),
        }
    }
}

impl From<&mut Row<'_>> for ArtworkRepairLeaseRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            owner_node_id: row.get("owner_node_id"),
            leader_term: row.get("leader_term"),
            generation: row.get("generation"),
        }
    }
}

impl From<&mut Row<'_>> for TargetNodeRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            raft_id: row.get("raft_id"),
        }
    }
}

impl From<&mut Row<'_>> for PromotionTargetRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            raft_id: row.get("raft_id"),
            admitted_role: row.get("role"),
            last_seen_at: row.get("last_seen_at"),
            removal_pending: row.get("removal_pending"),
            last_applied_index: row.get("last_applied_index"),
            apply_lag_entries: row.get("apply_lag_entries"),
            bounded_read_ready: row.get("bounded_read_ready"),
            voter_storage_ready: row.get("voter_storage_ready"),
            storage_headroom_bytes: row.get("storage_headroom_bytes"),
            storage_probe_observed_at: row.get("storage_probe_observed_at"),
            voter_role_persisted: row.get("voter_role_persisted"),
            observed_at: row.get("observed_at"),
        }
    }
}

impl From<&mut Row<'_>> for RemovalAttemptRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            attempt_id: row.get("attempt_id"),
        }
    }
}

impl From<&mut Row<'_>> for PromotionAttemptRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            attempt_id: row.get("attempt_id"),
        }
    }
}

impl From<&mut Row<'_>> for MembershipNodeRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            node_id: row.get("node_id"),
            raft_id: row.get("raft_id"),
            api_address: row.get("api_address"),
            hostname: row.get("hostname"),
            admitted_role: row.get("role"),
            last_seen_at: row.get("last_seen_at"),
            removal_pending: row.get("removal_pending"),
            apply_lag_entries: row.get("apply_lag_entries"),
            bounded_read_ready: row.get("bounded_read_ready"),
            voter_storage_ready: row.get("voter_storage_ready"),
            storage_headroom_bytes: row.get("storage_headroom_bytes"),
            storage_probe_observed_at: row.get("storage_probe_observed_at"),
            progress_observed_at: row.get("progress_observed_at"),
            maintenance_requested_at: row.get("maintenance_requested_at"),
            maintenance_acknowledged_at: row.get("maintenance_acknowledged_at"),
            maintenance_capable: row.get("maintenance_capable"),
            active_media_sessions: row.get("active_media_sessions"),
        }
    }
}

impl From<&mut Row<'_>> for MembershipMetricsRow {
    fn from(row: &mut Row<'_>) -> Self {
        let raft_id: i64 = row.get("raft_id");
        Self {
            raft_id: u64::try_from(raft_id).unwrap_or_default(),
            last_seen_at: row.get("last_seen_at"),
            removal_pending: row.get("removal_pending"),
        }
    }
}

fn artwork_auth_message(node_id: &str, timestamp_ms: i64, filename: &str) -> String {
    format!("plurx-artwork-v1\n{node_id}\n{timestamp_ms}\n{filename}")
}

/// Strip the listener port while preserving DNS names, IPv4, and bracketed
/// IPv6. Loopback is a location, not a useful numeric identity, so every
/// loopback literal has the one operator-facing spelling `localhost`.
fn advertised_host(address: &str) -> String {
    let host = if let Some(bracketed) = address.strip_prefix('[') {
        bracketed.rsplit_once("]:")
    } else {
        address.rsplit_once(':')
    };
    let host = host
        .filter(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok())
        .map(|(host, _)| host)
        .unwrap_or(address);
    if host
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
    {
        "localhost".to_owned()
    } else {
        host.to_owned()
    }
}

/// Name every roster row that can be named, and nothing that cannot.
///
/// Split out of [`MembershipManager::node_hostnames`] so the naming rules are
/// reachable without a replicated client behind them. Two rules carry weight:
/// a row that normalizes to [`UNKNOWN_HOSTNAME`] is dropped rather than
/// published, because a caller holding a node id is better served showing that
/// id than a sentinel; and the local node is named from memory, overriding its
/// own replicated row, because that row is written by the heartbeat and is
/// therefore absent for the first heartbeat interval after start.
fn roster_hostnames(
    rows: Vec<NodeHostnameRow>,
    local_node_id: &str,
    local_hostname: &str,
) -> BTreeMap<String, String> {
    let mut hostnames = rows
        .into_iter()
        .map(|row| {
            let hostname = membership_hostname(&row.hostname, &row.api_address);
            (row.node_id, hostname)
        })
        .filter(|(_, hostname)| hostname != UNKNOWN_HOSTNAME)
        .collect::<BTreeMap<_, _>>();
    if local_hostname == UNKNOWN_HOSTNAME {
        hostnames.remove(local_node_id);
    } else {
        hostnames.insert(local_node_id.to_owned(), local_hostname.to_owned());
    }
    hostnames
}

/// Reduce a machine name or FQDN to the short hostname people use at a shell.
/// An IP address is deliberately not accepted as a machine name.
fn short_hostname(raw: &str) -> Option<String> {
    let hostname = raw.trim().trim_end_matches('.');
    if hostname.parse::<IpAddr>().is_ok() {
        return None;
    }
    let label = hostname.split('.').next().unwrap_or_default().trim();
    if label.is_empty()
        || label.eq_ignore_ascii_case("localhost")
        || label.eq_ignore_ascii_case(UNKNOWN_HOSTNAME)
        || looks_like_container_id(label)
        || label.chars().any(char::is_control)
    {
        return None;
    }
    Some(label.chars().take(63).collect())
}

/// Docker's default hostname is the container id truncated to twelve hex
/// digits. It is ephemeral runtime plumbing, not a machine name, and should
/// never become the primary identity in the cluster roster.
fn looks_like_container_id(label: &str) -> bool {
    matches!(label.len(), 12 | 64) && label.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn membership_hostname(reported: &str, api_address: &str) -> String {
    membership_hostname_with_lookup(reported, api_address, cached_reverse_hostname)
}

fn membership_hostname_with_lookup(
    reported: &str,
    api_address: &str,
    reverse_lookup: impl FnOnce(IpAddr) -> Option<String>,
) -> String {
    let advertised = advertised_host(api_address);
    short_hostname(reported)
        .or_else(|| short_hostname(&advertised))
        .or_else(|| {
            advertised
                .parse::<IpAddr>()
                .ok()
                .and_then(reverse_lookup)
                .and_then(|hostname| short_hostname(&hostname))
        })
        .unwrap_or_else(|| UNKNOWN_HOSTNAME.to_owned())
}

fn cached_reverse_hostname(address: IpAddr) -> Option<String> {
    static CACHE: OnceLock<Mutex<BTreeMap<IpAddr, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some(hostname) = cache.get(&address) {
            return Some(hostname.clone());
        }
    }

    let hostname = reverse_hostname(address)?;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(address, hostname.clone());
    }
    Some(hostname)
}

#[cfg(unix)]
fn reverse_hostname(address: IpAddr) -> Option<String> {
    match address {
        IpAddr::V4(address) => {
            // SAFETY: an all-zero sockaddr is a valid base value. The family
            // and address fields below are initialized before libc reads it.
            let mut socket: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            socket.sin_family = libc::AF_INET as libc::sa_family_t;
            socket.sin_addr.s_addr = u32::from_ne_bytes(address.octets());
            #[cfg(any(
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "ios",
                target_os = "macos",
                target_os = "netbsd",
                target_os = "openbsd",
                target_os = "tvos",
                target_os = "visionos",
                target_os = "watchos"
            ))]
            {
                socket.sin_len = std::mem::size_of_val(&socket) as u8;
            }
            reverse_sockaddr(
                (&raw const socket).cast(),
                std::mem::size_of_val(&socket) as libc::socklen_t,
            )
        }
        IpAddr::V6(address) => {
            // SAFETY: an all-zero sockaddr is a valid base value. The family
            // and address fields below are initialized before libc reads it.
            let mut socket: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            socket.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            socket.sin6_addr.s6_addr = address.octets();
            #[cfg(any(
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "ios",
                target_os = "macos",
                target_os = "netbsd",
                target_os = "openbsd",
                target_os = "tvos",
                target_os = "visionos",
                target_os = "watchos"
            ))]
            {
                socket.sin6_len = std::mem::size_of_val(&socket) as u8;
            }
            reverse_sockaddr(
                (&raw const socket).cast(),
                std::mem::size_of_val(&socket) as libc::socklen_t,
            )
        }
    }
}

#[cfg(unix)]
fn reverse_sockaddr(
    address: *const libc::sockaddr,
    address_len: libc::socklen_t,
) -> Option<String> {
    let mut hostname = [0 as libc::c_char; 1025];
    // SAFETY: `address` points to a fully initialized sockaddr whose concrete
    // length is supplied alongside it. `hostname` is writable for its full
    // declared size and remains alive until the C string is copied below.
    let result = unsafe {
        libc::getnameinfo(
            address,
            address_len,
            hostname.as_mut_ptr(),
            hostname.len() as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    };
    if result != 0 {
        return None;
    }
    // SAFETY: successful getnameinfo writes a NUL-terminated hostname into
    // the provided buffer.
    unsafe { CStr::from_ptr(hostname.as_ptr()) }
        .to_str()
        .ok()
        .map(ToOwned::to_owned)
}

#[cfg(not(unix))]
fn reverse_hostname(_address: IpAddr) -> Option<String> {
    None
}

#[cfg(unix)]
pub(crate) fn system_short_hostname() -> Option<String> {
    if let Some(configured) = std::env::var("PLURX_NODE_HOSTNAME")
        .ok()
        .and_then(|hostname| short_hostname(&hostname))
    {
        return Some(configured);
    }
    let mut buffer = [0_u8; 256];
    // SAFETY: `buffer` is writable for exactly the length passed to libc and
    // stays alive until the returned bytes have been copied into a String.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    short_hostname(&String::from_utf8_lossy(&buffer[..end]))
}

#[cfg(not(unix))]
pub(crate) fn system_short_hostname() -> Option<String> {
    std::env::var("PLURX_NODE_HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .and_then(|hostname| short_hostname(&hostname))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The route gate answers 503 for `Fenced`, so the cost of an interim
    // publication is a real outage window on a healthy node, once per
    // heartbeat. These three pin the whole contract of the guard that makes
    // an interim publication unwriteable.

    #[test]
    fn a_serving_role_publication_does_not_touch_the_slot_before_it_commits() {
        let slot = AtomicU8::new(LocalServingRole::Voter.encoded());
        let publication = LocalServingRolePublication::new(&slot);
        // The reads a refresh performs happen here. A node that is serving
        // must still be serving.
        assert_eq!(
            LocalServingRole::from_encoded(slot.load(Ordering::Acquire)),
            LocalServingRole::Voter,
        );
        publication.commit(LocalServingRole::Learner);
        assert_eq!(
            LocalServingRole::from_encoded(slot.load(Ordering::Acquire)),
            LocalServingRole::Learner,
        );
    }

    #[test]
    fn an_uncommitted_serving_role_publication_fences_on_drop() {
        let slot = AtomicU8::new(LocalServingRole::Voter.encoded());
        {
            let _publication = LocalServingRolePublication::new(&slot);
            // An early `?`, a panic, or a cancelled future leaves here.
        }
        assert_eq!(
            LocalServingRole::from_encoded(slot.load(Ordering::Acquire)),
            LocalServingRole::Fenced,
            "a refresh that did not finish must leave the request path fenced",
        );
    }

    #[test]
    fn a_committed_serving_role_publication_survives_its_own_drop() {
        let slot = AtomicU8::new(LocalServingRole::Fenced.encoded());
        LocalServingRolePublication::new(&slot).commit(LocalServingRole::Voter);
        assert_eq!(
            LocalServingRole::from_encoded(slot.load(Ordering::Acquire)),
            LocalServingRole::Voter,
            "the drop that follows a commit must not re-fence the node",
        );
    }

    #[test]
    fn a_maintenance_publication_does_not_close_the_gate_before_it_commits() {
        let slot = AtomicBool::new(false);
        let publication = LocalMaintenancePublication::new(&slot);
        assert!(
            !slot.load(Ordering::Acquire),
            "a node nobody asked to drain must keep serving while the read runs",
        );
        publication.commit(false);
        assert!(!slot.load(Ordering::Acquire));
    }

    #[test]
    fn an_uncommitted_maintenance_publication_holds_the_node_in_maintenance() {
        let slot = AtomicBool::new(false);
        {
            let _publication = LocalMaintenancePublication::new(&slot);
        }
        assert!(
            slot.load(Ordering::Acquire),
            "a refresh that did not finish must leave the node in maintenance",
        );
    }

    #[test]
    fn a_committed_maintenance_publication_survives_its_own_drop() {
        let slot = AtomicBool::new(true);
        LocalMaintenancePublication::new(&slot).commit(false);
        assert!(
            !slot.load(Ordering::Acquire),
            "the drop that follows a commit must not re-close the gate",
        );
    }

    #[test]
    fn maintenance_is_replicated_and_rollout_gated() {
        assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
            statement.contains("CREATE TABLE IF NOT EXISTS cluster_node_maintenance")
                && statement.contains("acknowledged_at")
        }));
        assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
            statement
                .contains("CREATE TRIGGER IF NOT EXISTS cluster_node_maintenance_heartbeat_guard")
                && statement.contains("node maintenance requires current binary")
        }));
        assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
            statement.contains("CREATE TABLE IF NOT EXISTS cluster_operation_leases")
                && statement.contains("CHECK (operation IN ('restart', 'maintenance'))")
        }));
        for trigger in [
            "cluster_operation_lease_heartbeat_expiry",
            "cluster_operation_lease_removal_guard",
            "cluster_operation_lease_promotion_guard",
            "cluster_operation_lease_join_reservation_guard",
            "cluster_operation_lease_join_staging_guard",
        ] {
            assert!(MEMBERSHIP_SCHEMA
                .iter()
                .any(|statement| statement.contains(trigger)));
        }
        let ready = capability_ready_predicate(NODE_MAINTENANCE_CAPABILITY);
        assert!(ready.contains(NODE_MAINTENANCE_CAPABILITY));
        assert!(ready.contains("capability.last_seen_at = active.last_seen_at"));
    }

    #[test]
    fn maintenance_preserves_the_live_voter_quorum() {
        assert!(maintenance_preserves_quorum(true, true, 3, 3, 2));
        assert!(!maintenance_preserves_quorum(true, true, 3, 2, 2));
        assert!(maintenance_preserves_quorum(true, true, 4, 4, 3));
        assert!(!maintenance_preserves_quorum(true, true, 4, 3, 3));
        assert!(maintenance_preserves_quorum(true, true, 1, 1, 1));
        assert!(maintenance_preserves_quorum(false, true, 3, 1, 2));
    }

    #[test]
    fn leader_handoff_precedes_the_durable_maintenance_fence() {
        let source = include_str!("membership.rs")
            .split_once("pub async fn enter_maintenance(")
            .expect("maintenance entry")
            .1
            .split_once("pub async fn exit_maintenance(")
            .expect("maintenance entry end")
            .0;
        let handoff = source
            .find("self.handoff_leadership(target.raft_id, &status).await?")
            .expect("leader handoff");
        let commit = source
            .find("INSERT INTO cluster_node_maintenance")
            .expect("durable maintenance fence");
        assert!(handoff < commit);
        assert!(source.contains("lease.operation != \"maintenance\""));
        assert!(source.contains("Param::StmtOutputNamed(0, \"node_id\".into())"));
        assert!(source.contains("DELETE FROM cluster_operation_leases"));
    }

    #[test]
    fn restart_and_maintenance_race_for_one_replicated_outage_lease() {
        let path = std::env::temp_dir().join(format!(
            "plurx-cluster-operation-race-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let connection = rusqlite::Connection::open(&path).expect("lease fixture");
        connection
            .execute_batch(
                "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, removed_at INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', NULL), ('node-b', NULL); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removal_attempts (node_id TEXT, attempt_id TEXT); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   node_id TEXT NOT NULL, operation TEXT NOT NULL, \
                   claim_id TEXT NOT NULL UNIQUE, expires_at INTEGER NOT NULL);",
            )
            .expect("lease schema");
        drop(connection);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let race = |node_id: &'static str, operation: &'static str| {
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let connection = rusqlite::Connection::open(path).expect("racing lease client");
                connection
                    .busy_timeout(Duration::from_secs(5))
                    .expect("busy timeout");
                barrier.wait();
                connection
                    .execute(
                        ACQUIRE_CLUSTER_OPERATION_LEASE_SQL,
                        rusqlite::params![
                            node_id,
                            operation,
                            uuid::Uuid::new_v4().to_string(),
                            1_000_i64,
                            100_i64
                        ],
                    )
                    .expect("serialized lease race")
            })
        };
        let restart = race("node-a", "restart");
        let maintenance = race("node-b", "maintenance");
        barrier.wait();
        let changed = restart.join().expect("restart claimant")
            + maintenance.join().expect("maintenance claimant");
        assert_eq!(changed, 1, "exactly one planned outage may commit");

        let connection = rusqlite::Connection::open(&path).expect("read lease winner");
        let winner_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM cluster_operation_leases", [], |row| {
                row.get(0)
            })
            .expect("winner count");
        assert_eq!(winner_count, 1);
        connection
            .execute("UPDATE cluster_operation_leases SET expires_at = 99", [])
            .expect("expire first claimant");
        assert_eq!(
            connection
                .execute(
                    ACQUIRE_CLUSTER_OPERATION_LEASE_SQL,
                    rusqlite::params!["node-b", "maintenance", "successor", 2_000_i64, 100_i64],
                )
                .expect("replace expired claimant"),
            1
        );
        assert!(production_source()
            .contains("DELETE FROM cluster_operation_leases WHERE expires_at <= $1"));
        drop(connection);
        std::fs::remove_file(path).expect("remove lease fixture");
    }

    #[test]
    fn previous_release_lifecycle_writes_cannot_cross_an_outage_lease() {
        let connection = rusqlite::Connection::open_in_memory().expect("sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_operation_leases (\
                   singleton INTEGER PRIMARY KEY, node_id TEXT NOT NULL, \
                   operation TEXT NOT NULL, claim_id TEXT NOT NULL, expires_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_node_promotions (\
                   node_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL, \
                   barrier_index INTEGER, started_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_join_tokens (\
                   token_hash TEXT PRIMARY KEY, state TEXT NOT NULL); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY);",
            )
            .expect("previous-release lifecycle fixture");
        for trigger in [
            EXPIRE_OPERATION_LEASE_FROM_HEARTBEAT_SQL,
            PROTECT_OPERATION_LEASE_FROM_REMOVAL_SQL,
            PROTECT_OPERATION_LEASE_FROM_PROMOTION_SQL,
            PROTECT_OPERATION_LEASE_FROM_JOIN_RESERVATION_SQL,
            PROTECT_OPERATION_LEASE_FROM_JOIN_STAGING_SQL,
        ] {
            connection
                .execute_batch(trigger)
                .expect("install lease guard");
        }
        connection
            .execute_batch(
                "INSERT INTO cluster_join_tokens VALUES ('old-join', 'issued'); \
                 INSERT INTO cluster_nodes VALUES ('old-voter', 100); \
                 INSERT INTO cluster_operation_leases VALUES \
                   (1, 'node-a', 'restart', 'claim-a', 1000);",
            )
            .expect("active outage lease");

        // These are the lifecycle-begin write shapes used by the preceding
        // release, which has no lease predicate of its own.
        assert!(connection
            .execute(
                "INSERT INTO cluster_node_removal_attempts VALUES ($1, $2)",
                rusqlite::params!["node-c", "old-removal"],
            )
            .is_err());
        assert!(connection
            .execute(
                "INSERT INTO cluster_node_promotions VALUES ($1, $2, NULL, $3)",
                rusqlite::params!["node-c", "old-promotion", 1_i64],
            )
            .is_err());
        assert!(connection
            .execute(
                "UPDATE cluster_join_tokens SET state = 'redeeming' \
                 WHERE token_hash = 'old-join' AND state = 'issued'",
                [],
            )
            .is_err());
        assert!(connection
            .execute(
                "INSERT INTO cluster_node_join_staging VALUES ('old-join')",
                [],
            )
            .is_err());

        // The preceding release knows nothing about the lease table, but its
        // ordinary heartbeat update fires the schema-owned expiry cleanup.
        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = 1001 \
                 WHERE node_id = 'old-voter'",
                [],
            )
            .expect("previous-release heartbeat clears expired lease");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM cluster_operation_leases", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("expired lease count"),
            0
        );
        assert_eq!(
            connection
                .execute(
                    "INSERT INTO cluster_node_removal_attempts VALUES ($1, $2)",
                    rusqlite::params!["node-c", "old-removal"],
                )
                .expect("removal resumes after release"),
            1
        );
        assert_eq!(
            connection
                .execute(
                    "UPDATE cluster_join_tokens SET state = 'redeeming' \
                     WHERE token_hash = 'old-join' AND state = 'issued'",
                    [],
                )
                .expect("join resumes after release"),
            1
        );
    }

    #[test]
    fn delayed_acquisition_cannot_extend_the_local_fence_past_the_lease() {
        let requested = Duration::from_secs(60);
        let replicated_expiry = 65_000_i64;
        let after_slow_consensus = 5_800_i64;
        let local_expiry =
            bounded_local_operation_expiry(after_slow_consensus, requested, replicated_expiry)
                .expect("remaining lease time");
        assert_eq!(
            local_expiry,
            u64::try_from(replicated_expiry).expect("positive fixture expiry")
        );
        assert!(
            bounded_local_operation_expiry(replicated_expiry, requested, replicated_expiry)
                .is_none()
        );
    }

    #[test]
    fn maintenance_exit_is_one_atomic_current_process_and_drain_proof() {
        let mut connection = rusqlite::Connection::open_in_memory().expect("sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY, requested_at INTEGER NOT NULL, acknowledged_at INTEGER); \
                 CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 CREATE TABLE cluster_node_progress (node_id TEXT PRIMARY KEY, observed_at INTEGER NOT NULL, apply_lag_entries INTEGER); \
                 CREATE TABLE cluster_node_capabilities (node_id TEXT, capability TEXT, last_seen_at INTEGER, PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE media_sessions (owner_node_id TEXT, state TEXT, lease_expires_at_ms INTEGER); \
                 INSERT INTO cluster_node_maintenance VALUES ('node-a', 90, NULL); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 100, NULL); \
                 INSERT INTO cluster_node_progress VALUES ('node-a', 100, 0); \
                 INSERT INTO cluster_node_capabilities VALUES ('node-a', 'node_maintenance_v1', 100);",
            )
            .expect("maintenance exit fixture");
        let clear = |connection: &rusqlite::Connection| {
            connection
                .execute(
                    EXIT_MAINTENANCE_SQL,
                    rusqlite::params!["node-a", 95, NODE_MAINTENANCE_CAPABILITY, 100],
                )
                .expect("maintenance exit")
        };

        assert_eq!(clear(&connection), 0, "acknowledgement is mandatory");
        connection
            .execute(
                "UPDATE cluster_node_maintenance SET acknowledged_at = 100",
                [],
            )
            .expect("acknowledge");
        connection
            .execute("UPDATE cluster_nodes SET last_seen_at = 90", [])
            .expect("make heartbeat stale");
        assert_eq!(
            clear(&connection),
            0,
            "a stopped or stale process is refused"
        );
        connection
            .execute("UPDATE cluster_nodes SET last_seen_at = 100", [])
            .expect("restore heartbeat");
        connection
            .execute(
                "INSERT INTO media_sessions VALUES ('node-a', 'active', 101)",
                [],
            )
            .expect("active media");
        assert_eq!(clear(&connection), 0, "active replicated media is refused");
        connection
            .execute("UPDATE media_sessions SET state = 'ended'", [])
            .expect("drain media");

        let transaction = connection.transaction().expect("exit transaction");
        assert_eq!(clear(&transaction), 1, "all current proofs permit exit");
        transaction.rollback().expect("simulate failed commit");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM cluster_node_maintenance", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .expect("maintenance row"),
            1,
            "a failed transaction leaves the durable fence intact"
        );
    }

    #[test]
    fn every_membership_lifecycle_begin_excludes_planned_outages() {
        assert!(begin_removal_attempt_sql(false)
            .contains("NOT EXISTS (SELECT 1 FROM cluster_node_maintenance)"));
        assert!(begin_removal_attempt_sql(false)
            .contains("NOT EXISTS (SELECT 1 FROM cluster_operation_leases)"));
        let source = production_source();
        let redeem = source
            .split_once("async fn redeem_for_role(")
            .expect("join redemption")
            .1
            .split_once("async fn upsert_hostname(")
            .expect("join redemption end")
            .0;
        assert!(
            redeem
                .matches("NOT EXISTS (SELECT 1 FROM cluster_node_maintenance)")
                .count()
                >= 5,
            "every token reservation/proof branch must carry the atomic maintenance exclusion"
        );
        assert!(
            redeem
                .matches("NOT EXISTS (SELECT 1 FROM cluster_operation_leases)")
                .count()
                >= 5,
            "every token reservation/proof branch must carry the atomic outage-lease exclusion"
        );
        let promotion = source
            .split_once("pub async fn promote_learner(")
            .expect("learner promotion")
            .1
            .split_once("async fn promotion_target(")
            .expect("learner promotion end")
            .0;
        assert!(promotion.contains("NOT EXISTS (SELECT 1 FROM cluster_node_maintenance)"));
        assert!(promotion.contains("NOT EXISTS (SELECT 1 FROM cluster_operation_leases)"));
        assert!(promotion.contains("MembershipError::MaintenanceConflict"));
    }

    #[test]
    fn maintenance_rejects_a_heartbeat_without_the_current_binary_intent() {
        let connection = rusqlite::Connection::open_in_memory().expect("sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY, requested_at INTEGER NOT NULL, acknowledged_at INTEGER); \
                 CREATE TABLE cluster_node_maintenance_heartbeat_intents (node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL);",
            )
            .expect("maintenance tables");
        let trigger = MEMBERSHIP_SCHEMA
            .iter()
            .find(|statement| statement.contains("cluster_node_maintenance_heartbeat_guard"))
            .expect("maintenance heartbeat trigger");
        connection.execute_batch(trigger).expect("guard trigger");
        connection
            .execute_batch(
                "INSERT INTO cluster_nodes VALUES ('node-a', 100); \
                 INSERT INTO cluster_node_maintenance VALUES ('node-a', 100, 100);",
            )
            .expect("maintained node");
        let old = connection.execute(
            "UPDATE cluster_nodes SET last_seen_at = 200 WHERE node_id = 'node-a'",
            [],
        );
        assert!(
            old.is_err(),
            "a binary without the intent must not heartbeat through maintenance"
        );
        connection
            .execute_batch(
                "INSERT INTO cluster_node_maintenance_heartbeat_intents VALUES ('node-a', 200); \
                 UPDATE cluster_nodes SET last_seen_at = 200 WHERE node_id = 'node-a';",
            )
            .expect("current heartbeat");
    }

    #[test]
    fn maintenance_acknowledgement_follows_the_target_local_read() {
        let source = production_source();
        let heartbeat = source
            .split_once("async fn commit_heartbeat")
            .expect("heartbeat")
            .1
            .split_once("async fn refresh_local_maintenance")
            .expect("end heartbeat")
            .0;
        let local_read = heartbeat
            .find("let maintenance_requested = self.refresh_local_maintenance")
            .expect("local maintenance read");
        let guard = heartbeat
            .find("if maintenance_requested")
            .expect("acknowledgement guard");
        let acknowledgement = heartbeat
            .find("UPDATE cluster_node_maintenance SET acknowledged_at")
            .expect("acknowledgement statement");
        assert!(local_read < guard && guard < acknowledgement, "{heartbeat}");
    }

    #[test]
    fn maintenance_and_election_refusals_are_typed_for_operator_recovery() {
        assert_eq!(
            MembershipError::MaintenanceResumeUnsafe("node-b".to_owned()).code(),
            "maintenance_resume_unsafe"
        );
        assert_eq!(
            MembershipError::ElectionQuorumUnavailable.code(),
            "election_quorum_unavailable"
        );
        assert!(!production_source().contains("force_reconfigure"));
    }

    #[test]
    fn passive_membership_metrics_distinguish_unavailable_from_fresh_cluster_state() {
        let unavailable = PassiveMembershipMetrics::default().snapshot();
        assert!(!unavailable.replicated);
        assert!(!unavailable.valid);
        assert!(unavailable.sample.is_none());

        let metrics = PassiveMembershipMetrics::replicated();
        let sample = MembershipMetricsSample {
            voters: 4,
            learners: 0,
            heartbeat_fresh_voters: 3,
            heartbeat_stale_voters: 1,
            removals_pending: 0,
            local_is_voter: true,
        };
        metrics.record(sample);
        let view = metrics.snapshot();
        assert!(view.replicated);
        assert!(view.valid);
        assert_eq!(view.age_seconds, Some(0));
        assert_eq!(view.sample, Some(sample));
        assert_eq!(view.errors, 0);

        metrics.record_error();
        assert_eq!(metrics.snapshot().errors, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_gate_pins_commit_window_and_retries_failures() {
        let gate = HeartbeatWriteGate::default();
        let commits = std::sync::atomic::AtomicUsize::new(0);

        assert!(gate
            .run(async {
                commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .await
            .expect("leading heartbeat"));
        tokio::time::advance(HEARTBEAT_COALESCE_WINDOW - Duration::from_millis(1)).await;
        assert!(!gate
            .run(async {
                commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .await
            .expect("inside-window heartbeat"));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(gate
            .run(async {
                commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .await
            .expect("boundary heartbeat"));
        assert_eq!(commits.load(std::sync::atomic::Ordering::Relaxed), 2);

        tokio::time::advance(HEARTBEAT_COALESCE_WINDOW).await;
        let failed = gate
            .run(async {
                Err(MembershipError::Internal(
                    "injected heartbeat failure".to_owned(),
                ))
            })
            .await;
        assert!(failed.is_err());
        assert!(gate
            .run(async {
                commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .await
            .expect("heartbeat retry"));
        assert_eq!(commits.load(std::sync::atomic::Ordering::Relaxed), 3);
    }

    #[test]
    fn reachability_expires_immediately_after_the_allowed_missed_beats() {
        assert_eq!(
            NODE_REACHABLE_WINDOW_MS as u128,
            HEARTBEAT_INTERVAL.as_millis() * 3,
            "reachability must remain explicitly coupled to heartbeat cadence"
        );
        let last_seen_at = 1_000_000;
        assert!(node_is_reachable(
            last_seen_at + NODE_REACHABLE_WINDOW_MS,
            last_seen_at
        ));
        assert!(!node_is_reachable(
            last_seen_at + NODE_REACHABLE_WINDOW_MS + 1,
            last_seen_at
        ));
        assert_eq!(
            reachable_after(last_seen_at + NODE_REACHABLE_WINDOW_MS + 1),
            last_seen_at + 1
        );
    }

    #[test]
    fn activity_key_cache_replaces_one_peer_without_exceeding_its_bound() {
        let mut keys = (0..MAX_ACTIVITY_PEERS)
            .map(|index| (format!("node-{index:03}"), vec![index as u8; 32]))
            .collect::<BTreeMap<_, _>>();

        let evicted =
            insert_bounded_activity_public_key(&mut keys, "node-064".to_owned(), vec![0x64; 32]);

        assert_eq!(keys.len(), MAX_ACTIVITY_PEERS);
        assert_eq!(evicted.as_deref(), Some("node-000"));
        assert!(!keys.contains_key("node-000"));
        assert_eq!(keys.get("node-064"), Some(&vec![0x64; 32]));
    }

    #[test]
    fn activity_key_lookup_admission_reopens_after_one_window() {
        let started = Instant::now();
        let mut admission = ActivityAuthAdmission {
            window_started: started,
            checks: 0,
        };

        for _ in 0..MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND {
            assert!(admission.admit(started, MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND));
        }
        assert!(!admission.admit(started, MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND));
        assert!(admission.admit(
            started + Duration::from_secs(1),
            MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND
        ));
    }

    #[test]
    fn internal_media_read_preverification_has_a_bounded_burst() {
        let started = Instant::now();
        let mut admission = InternalReadAdmission {
            window_started: started,
            checks: 0,
        };
        for _ in 0..MAX_INTERNAL_READ_PREVERIFY_PER_SECOND {
            assert!(admission.admit(started));
        }
        assert!(!admission.admit(started));
        assert!(admission.admit(started + Duration::from_secs(1)));
    }

    #[test]
    fn exact_request_preverification_has_a_hard_global_rate() {
        let admission = Mutex::new(ActivityAuthAdmission {
            window_started: Instant::now(),
            checks: 0,
        });
        for _ in 0..MAX_INTERNAL_PREVERIFY_PER_SECOND {
            assert!(admit_internal_preverification(&admission).expect("admission"));
        }
        assert!(!admit_internal_preverification(&admission).expect("saturation"));
    }

    #[test]
    fn exact_internal_replay_is_rejected_for_the_entire_auth_window() {
        let started = Instant::now();
        let mut replays = InternalAuthReplayWindow::default();

        assert!(replays.admit("nonce-a", started));
        assert!(!replays.admit("nonce-a", started + Duration::from_secs(29)));
        assert!(replays.admit(
            "nonce-a",
            started + Duration::from_millis(ACTIVITY_AUTH_WINDOW_MS as u64 + 1)
        ));
    }

    #[test]
    fn exact_internal_replay_allows_distinct_nonces_at_the_same_timestamp() {
        let started = Instant::now();
        let mut replays = InternalAuthReplayWindow::default();

        assert!(replays.admit("nonce-a", started));
        assert!(replays.admit("nonce-b", started));
    }

    #[test]
    fn exact_internal_replay_cache_rejects_instead_of_evicting_live_proofs() {
        let started = Instant::now();
        let mut replays = InternalAuthReplayWindow::default();
        for index in 0..MAX_INTERNAL_REPLAYS_PER_PEER {
            assert!(replays.admit(&format!("nonce-{index}"), started));
        }
        assert!(!replays.admit("overflow", started));
        assert!(!replays.admit("nonce-0", started));
    }

    #[test]
    fn internal_read_replay_is_rejected_for_the_entire_read_window() {
        let started = Instant::now();
        let mut replays = InternalReadReplayWindow::default();

        assert!(replays.admit("nonce-a", started));
        assert!(!replays.admit("nonce-a", started + Duration::from_millis(4_999)));
        assert!(replays.admit(
            "nonce-a",
            started + Duration::from_millis(INTERNAL_READ_AUTH_WINDOW_MS as u64 + 1)
        ));
    }

    #[test]
    fn internal_read_replay_cache_rejects_instead_of_evicting_live_proofs() {
        let started = Instant::now();
        let mut replays = InternalReadReplayWindow::default();
        for index in 0..MAX_INTERNAL_READ_REPLAYS_PER_PEER {
            assert!(replays.admit(&format!("nonce-{index}"), started));
        }
        assert!(!replays.admit("overflow", started));
        assert!(!replays.admit("nonce-0", started));
    }

    #[test]
    fn every_post_send_membership_error_keeps_the_removal_fence() {
        for status in [
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::CONFLICT,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(matches!(
                membership_response_failure(status),
                MembershipChangeFailure::Ambiguous(_)
            ));
        }
    }

    #[test]
    fn removal_wire_request_is_a_node_delta() {
        assert_eq!(
            serde_json::to_value(RemoveVoterRequest { remove_voter: 7 })
                .expect("serialize removal delta"),
            serde_json::json!({"remove_voter": 7})
        );
    }

    #[test]
    fn a_uniform_new_quorum_proves_the_target_was_removed() {
        let voters = BTreeSet::from([2, 3, 4]);
        assert!(quorum_confirms_removal(
            1,
            &[
                (2, Some(voters.clone())),
                (3, Some(voters.clone())),
                (1, Some(BTreeSet::from([1, 2, 3]))),
            ]
        ));
        assert!(!quorum_confirms_removal(
            1,
            &[
                (2, Some(voters.clone())),
                (3, Some(BTreeSet::from([2, 3, 5]))),
                (4, None),
            ]
        ));
        assert!(!quorum_confirms_removal(
            1,
            &[
                (2, Some(BTreeSet::from([1, 2, 3]))),
                (3, Some(BTreeSet::from([1, 2, 3]))),
            ]
        ));
    }

    #[test]
    fn even_voter_leaders_commit_without_a_pre_handoff() {
        assert_eq!(
            leader_self_leave_sequence(3),
            LeaderSelfLeaveSequence::HandoffThenCommit
        );
        assert_eq!(
            leader_self_leave_sequence(4),
            LeaderSelfLeaveSequence::CommitDirectly
        );
        assert_eq!(
            leader_self_leave_sequence(5),
            LeaderSelfLeaveSequence::HandoffThenCommit
        );
        assert_eq!(
            leader_self_leave_sequence(6),
            LeaderSelfLeaveSequence::CommitDirectly
        );
    }

    /// Seed a three-voter cluster whose nodes have all heartbeated once, on an
    /// unactivated `cluster_meta` range. `ready` names the nodes whose current
    /// binary proved the learner protocol.
    fn protocol_fixture(ready: &[&str]) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_meta (\
                   singleton INTEGER PRIMARY KEY, schema_version INTEGER, \
                   protocol_min INTEGER, protocol_max INTEGER, migrated_at INTEGER); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER, \
                   last_seen_at INTEGER, removed_at INTEGER, role TEXT); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_heartbeat_intents (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_join_tokens (node_id TEXT, state TEXT); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, started_at INT); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 INSERT INTO cluster_meta VALUES (1, 11, 4, 4, 0); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 1, 100, NULL, 'voter'); \
                 INSERT INTO cluster_nodes VALUES ('node-b', 2, 200, NULL, NULL); \
                 INSERT INTO cluster_nodes VALUES ('node-c', 3, 300, NULL, 'voter');",
            )
            .expect("seed a three-voter cluster on the unactivated range");
        for node_id in ready {
            connection
                .execute(
                    "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                     SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
                    rusqlite::params![node_id, LEARNER_PROTOCOL_CAPABILITY],
                )
                .expect("couple the capability to that node's current heartbeat");
        }
        connection
    }

    fn active_range(connection: &rusqlite::Connection) -> (i64, i64) {
        connection
            .query_row(
                "SELECT protocol_min, protocol_max FROM cluster_meta WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read the active protocol range")
    }

    fn unready_nodes(connection: &rusqlite::Connection) -> Vec<String> {
        let sql = capability_unready_nodes_sql(LEARNER_PROTOCOL_CAPABILITY);
        let mut statement = connection
            .prepare(&sql)
            .expect("prepare the unready roster");
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("run the unready roster")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect the unready roster");
        rows
    }

    /// Every node in [`protocol_fixture`] heartbeated at or before 300, so a
    /// cutoff below that treats all of them as present. Tests that want the
    /// absence rule to bite pass their own.
    const FIXTURE_PRESENT_CUTOFF: i64 = 1;

    fn activate_at(connection: &rusqlite::Connection, cutoff: i64) -> usize {
        connection
            .execute(
                &narrow_protocol_range_sql(Some(activation_guard_predicate())),
                rusqlite::params![AUTH_LEARNER_PROTOCOL, AUTH_PROTOCOL_MIN, cutoff],
            )
            .expect("run the activation statement")
    }

    fn activate(connection: &rusqlite::Connection) -> usize {
        activate_at(connection, FIXTURE_PRESENT_CUTOFF)
    }

    fn deactivate(connection: &rusqlite::Connection) -> usize {
        connection
            .execute(
                &narrow_protocol_range_sql(Some(no_admitted_learner_predicate().to_owned())),
                rusqlite::params![AUTH_PROTOCOL_MIN, AUTH_LEARNER_PROTOCOL],
            )
            .expect("run the deactivation statement")
    }

    fn absent_nodes(connection: &rusqlite::Connection, cutoff: i64) -> Vec<String> {
        let mut statement = connection
            .prepare(absent_nodes_sql())
            .expect("prepare the absent roster");
        let rows = statement
            .query_map(rusqlite::params![cutoff], |row| row.get::<_, String>(0))
            .expect("run the absent roster")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect the absent roster");
        rows
    }

    fn joining_nodes(connection: &rusqlite::Connection) -> Vec<String> {
        let mut statement = connection
            .prepare(join_in_flight_nodes_sql())
            .expect("prepare the join-in-flight roster");
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("run the join-in-flight roster")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect the join-in-flight roster");
        rows
    }

    fn admitted_learners(connection: &rusqlite::Connection) -> Vec<String> {
        let mut statement = connection
            .prepare(admitted_learner_nodes_sql())
            .expect("prepare the learner roster");
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("run the learner roster")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect the learner roster");
        rows
    }

    /// The role column is additive and nullable, so every row an older
    /// coordinator wrote reads back as the voter it is. Only an explicit
    /// `'learner'` names a node this cluster admitted under protocol 5.
    #[test]
    fn a_membership_row_without_a_role_is_a_voter() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        assert!(
            admitted_learners(&connection).is_empty(),
            "a NULL role must not be read as a learner"
        );
        assert_eq!(
            ClusterRole::from_stored(None).expect("null role"),
            ClusterRole::Voter
        );
        assert_eq!(
            ClusterRole::from_stored(Some("voter")).expect("voter role"),
            ClusterRole::Voter
        );
        assert_eq!(
            ClusterRole::from_stored(Some("learner")).expect("learner role"),
            ClusterRole::Learner
        );
        // Not a guess and not a default: a value this binary does not
        // understand is a state it must not act on.
        assert!(ClusterRole::from_stored(Some("observer")).is_err());
    }

    /// The refusal that keeps deactivation from stranding a learner rides in
    /// the committing statement, not only in a preflight an admission can win
    /// a race against.
    #[test]
    fn deactivation_is_refused_by_its_own_statement_while_a_learner_is_admitted() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        assert_eq!(activate(&connection), 1);

        connection
            .execute(
                "INSERT INTO cluster_nodes VALUES ('node-learner', 4, 400, NULL, 'learner')",
                [],
            )
            .expect("admit a learner");
        assert_eq!(
            admitted_learners(&connection),
            vec!["node-learner".to_owned()]
        );
        assert_eq!(
            deactivate(&connection),
            0,
            "an admitted learner blocks deactivation"
        );
        assert_eq!(active_range(&connection), (5, 5));

        // Removal tombstones the row rather than deleting it, so the guard has
        // to read `removed_at` too — otherwise a cluster could never roll back
        // once it had ever admitted a learner.
        connection
            .execute(
                "UPDATE cluster_nodes SET removed_at = 500 WHERE node_id = 'node-learner'",
                [],
            )
            .expect("remove the learner");
        assert!(admitted_learners(&connection).is_empty());
        assert_eq!(
            deactivate(&connection),
            1,
            "a removed learner strands nothing"
        );
        assert_eq!(active_range(&connection), (4, 4));
    }

    /// The committing statement — not just the preflight — carries the
    /// capability precondition, and it is idempotent in both directions.
    #[test]
    fn activation_commits_once_and_only_with_every_voter_proven() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        assert!(unready_nodes(&connection).is_empty());
        assert_eq!(active_range(&connection), (4, 4));

        assert_eq!(activate(&connection), 1, "a proven cluster activates");
        assert_eq!(active_range(&connection), (5, 5));

        // Idempotent: the second call matches no row precisely because the
        // range has already moved, so a retried request is not a second write.
        assert_eq!(activate(&connection), 0, "activation is idempotent");
        assert_eq!(active_range(&connection), (5, 5));

        assert_eq!(deactivate(&connection), 1, "the reversible case rolls back");
        assert_eq!(active_range(&connection), (4, 4));
        assert_eq!(deactivate(&connection), 0, "deactivation is idempotent");
        assert_eq!(active_range(&connection), (4, 4));
    }

    /// One voter that has never proven the capability blocks activation, and
    /// the roster the refusal is built from names exactly that voter.
    #[test]
    fn one_unproven_voter_blocks_activation_and_is_named() {
        let connection = protocol_fixture(&["node-a", "node-c"]);
        assert_eq!(unready_nodes(&connection), vec!["node-b".to_owned()]);
        assert_eq!(activate(&connection), 0, "an unproven voter blocks it");
        assert_eq!(active_range(&connection), (4, 4));

        connection
            .execute(
                "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
                rusqlite::params!["node-b", LEARNER_PROTOCOL_CAPABILITY],
            )
            .expect("the last voter heartbeats on the new binary");
        assert!(unready_nodes(&connection).is_empty());
        assert_eq!(activate(&connection), 1);
    }

    /// A node that redeemed a join token and has not yet proven anything
    /// blocks activation, and is named as *joining* rather than as behind.
    ///
    /// This is the lockout: `redeem` publishes the staging row and the
    /// `cluster_nodes` row before the joiner has started Raft or run a single
    /// compatibility check, so a node that redeems on an older binary and is
    /// then interrupted is a committed voter with no capability row that the
    /// pending roster deliberately cannot see. Activating past it leaves a
    /// voter that counts for quorum and can never open its store again — on a
    /// 3→4 growth that takes fault tolerance to zero.
    ///
    /// The roster stays honest — a mid-join node is genuinely not "behind" —
    /// and the *commit* is what refuses.
    #[test]
    fn a_join_in_flight_blocks_activation_without_making_the_pending_roster_lie() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        connection
            .execute_batch(
                "INSERT INTO cluster_nodes VALUES ('node-joining', 4, 300, NULL, 'voter'); \
                 INSERT INTO cluster_node_join_staging VALUES ('node-joining');",
            )
            .expect("redeem a join token for a fourth node");

        assert!(
            unready_nodes(&connection).is_empty(),
            "a mid-join node has not heartbeated, so it is not 'behind'"
        );
        assert_eq!(joining_nodes(&connection), vec!["node-joining".to_owned()]);
        assert_eq!(
            activate(&connection),
            0,
            "the committing statement refuses while a join is in flight"
        );
        assert_eq!(active_range(&connection), (4, 4));

        // The joiner's first heartbeat clears the staging row; that is the
        // moment it becomes visible to the capability rule instead.
        connection
            .execute(
                "DELETE FROM cluster_node_join_staging WHERE node_id = 'node-joining'",
                [],
            )
            .expect("the joiner heartbeats");
        assert!(joining_nodes(&connection).is_empty());
        assert_eq!(
            unready_nodes(&connection),
            vec!["node-joining".to_owned()],
            "and now it is the capability rule that holds activation"
        );
        assert_eq!(activate(&connection), 0);

        connection
            .execute(
                "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
                rusqlite::params!["node-joining", LEARNER_PROTOCOL_CAPABILITY],
            )
            .expect("the joined node proves the protocol");
        assert_eq!(activate(&connection), 1);
    }

    /// A join that is never coming back must not hold activation for good.
    ///
    /// Two exits, because a refusal with no exit is a lockout of its own. The
    /// operator can tombstone the node, and time can answer for them: a staged
    /// node's `last_seen_at` is its redemption timestamp and stops advancing
    /// the moment the join is abandoned, so it becomes *absent* — refused with
    /// "start it or remove it" rather than "wait for the join".
    #[test]
    fn an_abandoned_join_stops_holding_activation() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        connection
            .execute_batch(
                "INSERT INTO cluster_nodes VALUES ('node-abandoned', 4, 300, NULL, 'voter'); \
                 INSERT INTO cluster_node_join_staging VALUES ('node-abandoned');",
            )
            .expect("redeem a token for a node that never arrives");
        assert_eq!(activate(&connection), 0);

        // Exit one: time. Nothing about this node has advanced since it
        // redeemed, and the refusal that names it says what to do about that.
        connection
            .execute_batch(
                "UPDATE cluster_nodes SET last_seen_at = 400 WHERE node_id != 'node-abandoned'; \
                 UPDATE cluster_node_capabilities SET last_seen_at = 400;",
            )
            .expect("the three real voters keep heartbeating and proving");
        assert_eq!(
            absent_nodes(&connection, 301),
            vec!["node-abandoned".to_owned()]
        );
        assert_eq!(
            activate_at(&connection, 301),
            0,
            "still refused — but now as an absent node, not as a join to wait for"
        );

        // Exit two: the operator removes it. Neither rule names a tombstoned
        // node, and the leftover staging row does not resurrect it.
        connection
            .execute(
                "UPDATE cluster_nodes SET removed_at = 999 WHERE node_id = 'node-abandoned'",
                [],
            )
            .expect("tombstone the abandoned join");
        assert!(joining_nodes(&connection).is_empty());
        assert!(absent_nodes(&connection, 301).is_empty());
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM cluster_node_join_staging",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count staging rows"),
            1,
            "the staging row is still there; the tombstone is what stops it counting"
        );
        assert_eq!(activate(&connection), 1);
        assert_eq!(active_range(&connection), (5, 5));
    }

    /// A node that proved the capability and then stopped is not a proof.
    ///
    /// The capability equality says which binary wrote the *last* heartbeat.
    /// It says nothing about whether that node still exists, so a node that
    /// was powered off after proving stays "ready" forever — and a binary
    /// rolled back while the node is down never writes the desynchronising
    /// heartbeat the whole rule leans on. It simply comes back up unable to
    /// speak protocol 5 in a cluster that has already narrowed onto it.
    ///
    /// A wall-clock window is a weak proof of presence. It is a perfectly good
    /// proof of absence, and absence is all this has to establish.
    #[test]
    fn a_node_that_proved_the_protocol_and_then_stopped_blocks_activation() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        // Every node is proven and the fixture's timestamps are all current:
        // the earliest heartbeat is 100, so a cutoff of 100 leaves nobody
        // strictly behind it.
        assert!(unready_nodes(&connection).is_empty());
        assert!(absent_nodes(&connection, 100).is_empty());

        // node-c last heartbeated at 300 and the others later; a cutoff of 301
        // is "node-c has said nothing for the whole window".
        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = 400 WHERE node_id IN ('node-a','node-b')",
                [],
            )
            .expect("the two live nodes keep heartbeating");
        connection
            .execute(
                "UPDATE cluster_node_capabilities SET last_seen_at = 400 \
                 WHERE node_id IN ('node-a','node-b')",
                [],
            )
            .expect("and keep proving the protocol as they do");

        assert_eq!(
            absent_nodes(&connection, 301),
            vec!["node-c".to_owned()],
            "the stopped node is named, so the refusal can say to start or remove it"
        );
        assert!(
            unready_nodes(&connection).is_empty(),
            "its capability row still looks like a proof; only the clock disproves it"
        );
        assert_eq!(
            activate_at(&connection, 301),
            0,
            "a member that has gone silent blocks the commit"
        );
        assert_eq!(active_range(&connection), (4, 4));

        // Start it again and it proves itself the ordinary way.
        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = 500 WHERE node_id = 'node-c'",
                [],
            )
            .expect("node-c comes back");
        connection
            .execute(
                "UPDATE cluster_node_capabilities SET last_seen_at = 500 \
                 WHERE node_id = 'node-c'",
                [],
            )
            .expect("on a binary that still knows the protocol");
        assert!(absent_nodes(&connection, 301).is_empty());
        assert_eq!(activate_at(&connection, 301), 1);
        assert_eq!(active_range(&connection), (5, 5));
    }

    /// Removing the node instead of starting it also unblocks activation, and
    /// a tombstoned node never counts as absent.
    #[test]
    fn a_removed_node_is_not_an_absent_one() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = 1000 \
                 WHERE node_id IN ('node-a','node-b')",
                [],
            )
            .expect("the survivors keep heartbeating");
        connection
            .execute(
                "UPDATE cluster_node_capabilities SET last_seen_at = 1000 \
                 WHERE node_id IN ('node-a','node-b')",
                [],
            )
            .expect("and keep proving the protocol");
        connection
            .execute(
                "UPDATE cluster_nodes SET removed_at = 999 WHERE node_id = 'node-c'",
                [],
            )
            .expect("remove the stopped node");
        assert!(
            absent_nodes(&connection, 1_000).is_empty(),
            "a removed node is nobody's problem, however long ago it spoke"
        );
        assert_eq!(activate_at(&connection, 1_000), 1);
    }

    /// The absence window is coupled to the heartbeat cadence rather than
    /// picked, and is deliberately much wider than the reachability window a
    /// human reads: this decides a one-way narrowing, not a health badge.
    #[test]
    fn the_absence_window_is_a_stated_multiple_of_the_heartbeat_interval() {
        assert_eq!(
            PROTOCOL_CHANGE_ABSENCE_WINDOW_MS as u128,
            HEARTBEAT_INTERVAL.as_millis() * 12
        );
        const {
            assert!(
                PROTOCOL_CHANGE_ABSENCE_WINDOW_MS > NODE_REACHABLE_WINDOW_MS,
                "a node that is merely unreachable must not block activation"
            );
        }
    }

    /// An old staged learner redemption still blocks rollback.
    ///
    /// `redeem` commits `role = 'learner'` before the joiner has joined Raft
    /// at all, so a redemption that then failed — port conflict, crash,
    /// operator ^C — leaves a row for an authorized node that may resume.
    ///
    /// A staging row plus age is not proof of abandonment: the authorized
    /// process may be paused after redemption and resume after any wall-clock
    /// cutoff. Until an explicit learner removal atomically cancels and
    /// tombstones the admission, rollback must fail closed.
    #[test]
    fn a_staged_learner_redemption_blocks_rollback_regardless_of_age() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        assert_eq!(activate(&connection), 1);

        connection
            .execute_batch(
                "INSERT INTO cluster_nodes VALUES ('node-abandoned', 4, 400, NULL, 'learner'); \
                 INSERT INTO cluster_node_join_staging VALUES ('node-abandoned');",
            )
            .expect("redeem a learner token for a node that never arrives");

        assert_eq!(
            admitted_learners(&connection),
            vec!["node-abandoned".to_owned()],
            "the durable admission is protected"
        );
        assert_eq!(deactivate(&connection), 0);
        assert_eq!(active_range(&connection), (5, 5));

        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = -999999999 \
                 WHERE node_id = 'node-abandoned'",
                [],
            )
            .expect("make the staged admission arbitrarily old");
        assert_eq!(admitted_learners(&connection), vec!["node-abandoned"]);
        assert_eq!(deactivate(&connection), 0);
        assert_eq!(active_range(&connection), (5, 5));
    }

    /// A learner that actually joined keeps blocking rollback however long it
    /// has been quiet, because it is a real member holding real state.
    #[test]
    fn a_learner_that_finished_joining_blocks_rollback_however_stale_it_is() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        assert_eq!(activate(&connection), 1);
        connection
            .execute(
                "INSERT INTO cluster_nodes VALUES ('node-learner', 4, 400, NULL, 'learner')",
                [],
            )
            .expect("admit a learner whose first heartbeat cleared its staging row");
        assert_eq!(
            admitted_learners(&connection),
            vec!["node-learner".to_owned()],
            "no staging row means the admission finished; staleness is irrelevant"
        );
        assert_eq!(deactivate(&connection), 0);
        assert_eq!(active_range(&connection), (5, 5));
    }

    /// The additive columns land together, because the window between them
    /// breaks the whole join surface rather than one statement in it.
    ///
    /// Both `ALTER`s were separate Raft commands. Between them
    /// `cluster_nodes.role` existed and `cluster_join_tokens.role` did not —
    /// and every statement on the join surface names both, so for the length
    /// of that window nothing on it could even *prepare*: SQLite reports `no
    /// such column: role` at preparation, not at execution.
    ///
    /// This reconstructs the half-applied state and shows that the surface is
    /// unusable in it, that the probe finds exactly the missing column, and
    /// that applying the remainder as one statement list closes it.
    #[test]
    fn a_half_applied_additive_column_breaks_the_join_surface_it_is_not_on() {
        let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, last_seen_at INTEGER); \
                 CREATE TABLE cluster_join_tokens (token_hash TEXT PRIMARY KEY, node_id TEXT);",
            )
            .expect("seed the pre-upgrade tables");

        let missing = |connection: &rusqlite::Connection| {
            MEMBERSHIP_ADDITIVE_COLUMNS
                .iter()
                .filter(|column| {
                    connection
                        .query_row(
                            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                            rusqlite::params![column.table, column.column],
                            |row| row.get::<_, i64>(0),
                        )
                        .expect("probe the column")
                        == 0
                })
                .map(|column| format!("{}.{}", column.table, column.column))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            missing(&connection),
            vec![
                "cluster_nodes.role".to_owned(),
                "cluster_join_tokens.role".to_owned()
            ],
            "the probe has to be able to see both, or it cannot decide anything"
        );

        // The half-applied state: statement [0] committed, [1] did not.
        connection
            .execute(MEMBERSHIP_ADDITIVE_COLUMNS[0].statement, [])
            .expect("apply only the first column");
        assert_eq!(
            missing(&connection),
            vec!["cluster_join_tokens.role".to_owned()],
            "the probe names exactly what is left, so the remainder is applyable"
        );

        // And the surface is not merely degraded — it will not prepare.
        let refused = connection
            .prepare("SELECT raft_id, expires_at, state, node_id, role FROM cluster_join_tokens")
            .expect_err("the token read must not prepare against a half-applied schema")
            .to_string();
        assert!(refused.contains("role"), "{refused}");

        // Applying the remainder — which is what one transaction does — closes
        // it, and the probe is then satisfied.
        for column in MEMBERSHIP_ADDITIVE_COLUMNS.iter().skip(1) {
            connection
                .execute(column.statement, [])
                .expect("apply the rest in one go");
        }
        assert!(missing(&connection).is_empty());
        connection
            .prepare("SELECT node_id, role FROM cluster_join_tokens")
            .expect("the join surface prepares once every column exists");
    }

    /// The fail-closed branch of the job-authority decision: refuse, and say
    /// so somewhere a machine can read.
    ///
    /// `metrics_db()` borrows a local watch channel and cannot fail, so no real
    /// daemon reaches this arm today and no fixture can force it. That is the
    /// reason it needs both a named function and a counter rather than only a
    /// log line: a counter at zero is the standing evidence that the arm is
    /// still unreachable, and one that moves is a node quietly declining the
    /// cluster's scheduled work.
    #[test]
    fn unreadable_membership_declines_cluster_work_and_is_counted() {
        let before = CLUSTER_JOB_AUTHORITY_UNREADABLE.load(Ordering::Relaxed);
        assert!(
            decide_cluster_job_authority(Ok(true)),
            "a committed voter runs the cluster's singleton work"
        );
        assert!(!decide_cluster_job_authority(Ok(false)));
        assert_eq!(
            CLUSTER_JOB_AUTHORITY_UNREADABLE.load(Ordering::Relaxed),
            before,
            "an answered question is not an unreadable one"
        );

        assert!(
            !decide_cluster_job_authority(Err(MembershipError::Unavailable)),
            "membership that cannot be read must fail closed"
        );
        assert_eq!(
            CLUSTER_JOB_AUTHORITY_UNREADABLE.load(Ordering::Relaxed),
            before + 1
        );
        assert!(prometheus_cluster_job_authority()
            .contains("plurx_cluster_job_authority_unreadable_total "));
    }

    /// `finalize`'s role assertion, in every state committed membership can
    /// be in.
    ///
    /// Both arms survived mutation before this existed. Weakening the learner
    /// arm to `is_member` let a *voter* finalize a learner token; weakening
    /// the voter arm to `is_member` let a voter finalize while it was still
    /// Raft's intermediate learner — before it had the vote the cluster was
    /// already counting on. Neither state is reachable from a one-voter
    /// replicated fixture, which is exactly why both survived: the live test
    /// only ever produces the two states that pass.
    #[test]
    fn finalizing_requires_the_membership_the_token_actually_admits() {
        // (is_member, is_voter) → what each role may finalize on.
        let cases = [
            // Not in committed membership at all: nothing finalizes.
            (false, false, false, false),
            // A committed member with no vote: a learner, and only a learner.
            (true, false, false, true),
            // A committed voter: a voter, and only a voter. `is_member` is
            // true here too, which is what made the weakened arms pass.
            (true, true, true, false),
        ];
        for (is_member, is_voter, voter_ok, learner_ok) in cases {
            assert_eq!(
                role_is_admitted(ClusterRole::Voter, is_member, is_voter),
                voter_ok,
                "voter token with member={is_member} voter={is_voter}"
            );
            assert_eq!(
                role_is_admitted(ClusterRole::Learner, is_member, is_voter),
                learner_ok,
                "learner token with member={is_member} voter={is_voter}"
            );
        }

        // Stated as the two mutations rather than only as a table, so the
        // failure message names what went wrong.
        assert!(
            !role_is_admitted(ClusterRole::Voter, true, false),
            "a voter must not finalize while it is still Raft's intermediate \
             learner and carries no vote"
        );
        assert!(
            !role_is_admitted(ClusterRole::Learner, true, true),
            "a node in the voter set was not admitted by the learner protocol"
        );
    }

    #[test]
    fn a_transient_joining_voter_is_not_reported_as_read_worker_capacity() {
        assert!(counts_as_ready_read_worker(&NodeRole::Learner, false, true));
        assert!(
            !counts_as_ready_read_worker(&NodeRole::Voter, false, true),
            "a voter-token join is only transiently non-voting"
        );
        assert!(!counts_as_ready_read_worker(&NodeRole::Learner, true, true));
    }

    /// The learner operations act on the learner protocol, named as itself.
    ///
    /// `AUTH_PROTOCOL_MAX` and `AUTH_LEARNER_PROTOCOL` are the same number
    /// today, and `AUTH_LEARNER_PROTOCOL` exists precisely because the first
    /// moves with every future protocol and the second must not. No
    /// behavioural test can tell them apart while they agree, so the call
    /// sites are pinned here instead: the day protocol 6 lands, an
    /// `AUTH_PROTOCOL_MAX` left in any of these would silently retarget
    /// activation, deactivation, and the status projection at it.
    #[test]
    fn the_learner_operations_name_the_learner_protocol_not_the_newest_one() {
        let source = production_source();
        let between = |from: &str, to: &str| {
            source
                .split_once(from)
                .unwrap_or_else(|| panic!("{from} is missing"))
                .1
                .split_once(to)
                .unwrap_or_else(|| panic!("{to} is missing after {from}"))
                .0
                .to_owned()
        };

        let projection = between(
            "async fn protocol_projection(",
            "\n    /// Narrow the cluster",
        );
        assert!(
            projection.contains("learner_protocol_active: (active_min, active_max)\n                == (AUTH_LEARNER_PROTOCOL, AUTH_LEARNER_PROTOCOL)"),
            "the status projection must compare against the learner protocol: {projection}"
        );
        // `binary_max` genuinely is "the newest protocol this build knows".
        assert!(projection.contains("binary_max: AUTH_PROTOCOL_MAX"));

        for (name, body) in [
            (
                "activate_learner_protocol",
                between(
                    "pub async fn activate_learner_protocol(",
                    "/// The read-only pass behind",
                ),
            ),
            (
                "deactivate_learner_protocol",
                between(
                    "pub async fn deactivate_learner_protocol(",
                    "/// The read-only pass behind",
                ),
            ),
        ] {
            assert!(
                !body.contains("AUTH_PROTOCOL_MAX"),
                "{name} must name AUTH_LEARNER_PROTOCOL, not the moving maximum: {body}"
            );
            assert!(body.contains("AUTH_LEARNER_PROTOCOL"), "{name}: {body}");
        }
    }

    /// A member with no vote is not "not found".
    ///
    /// `DELETE /cluster/nodes/{id}` used to fall through to `NodeNotFound` and
    /// answer 404 for a node the roster lists — which reads as "you typed the
    /// wrong id" for a node the operator is looking straight at. The
    /// voter-only compatibility method keeps a typed refusal; the public
    /// `remove_node` dispatcher now selects learner removal instead.
    #[test]
    fn removing_a_member_with_no_vote_is_refused_by_name_rather_than_as_not_found() {
        let refusal = MembershipError::NonVoterRemovalUnsupported("node-learner".to_owned());
        assert_eq!(refusal.code(), "cluster_non_voter_removal_unsupported");
        assert_ne!(refusal.code(), MembershipError::NodeNotFound.code());
        assert!(refusal.to_string().contains("node-learner"), "{refusal}");
    }

    #[test]
    fn learner_removal_uses_the_shipped_hiqlite_delete_route() {
        let request = production_source()
            .split_once("async fn request_learner_removal(")
            .expect("learner removal request")
            .1
            .split_once("fn membership_response_failure(")
            .expect("end of learner removal request")
            .0
            .to_owned();

        assert!(request
            .contains(".delete(format!(\"https://{leader_api}/cluster/membership/sqlite\"))"));
        assert!(!request.contains("/cluster/leave/"));
    }

    /// A join refused by the cluster's own rules is not a migration failure.
    ///
    /// The refusal reached operators as `schema migration failed:
    /// learner_protocol_inactive: …`, which points at the database when the
    /// actual answer is "run the activation command".
    #[test]
    fn a_join_the_cluster_refuses_is_not_reported_as_a_migration_failure() {
        let refused = crate::error::StoreError::JoinRefused(
            "learner_protocol_inactive: activate it first".to_owned(),
        )
        .to_string();
        assert!(
            !refused.contains("migration"),
            "a policy refusal must not be dressed as a schema failure: {refused}"
        );
        assert!(refused.contains("learner_protocol_inactive"), "{refused}");
    }

    /// An admission is a compare-and-swap on the protocol range, not a read
    /// followed by a hopeful write.
    ///
    /// `redeem` reads the active range and refuses a learner on an unactivated
    /// cluster, but there are at least two more linearizable round-trips
    /// between that read and the admitting transaction —
    /// `deactivate_learner_protocol` can narrow the range to `4..=4` in that
    /// window and have the learner row commit anyway. That learner is admitted
    /// into a cluster that has just rolled back and cannot restart until an
    /// explicit removal. The code's own comment beside
    /// `narrow_protocol_range_sql` states this principle; the join did not
    /// apply it.
    ///
    /// Both directions matter, so this is equality on min *and* max: a
    /// protocol-4-only binary must not be admitted into a cluster that
    /// activated while its request was in flight either.
    #[test]
    fn an_admission_is_rolled_back_when_the_protocol_range_moves_under_it() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        let guarded = format!(
            "INSERT INTO cluster_nodes (node_id, raft_id, last_seen_at, removed_at, role) \
             SELECT 'node-joining', 4, 400, NULL, 'learner' WHERE {}",
            unchanged_protocol_range_predicate(1, 2)
        );
        let undo = || {
            connection
                .execute(
                    "DELETE FROM cluster_nodes WHERE node_id = 'node-joining'",
                    [],
                )
                .expect("undo the admission");
        };

        // Authorized against 4..=4, and the range has not moved.
        assert_eq!(
            connection
                .execute(&guarded, rusqlite::params![4_i64, 4_i64])
                .expect("run the guarded admission"),
            1
        );
        undo();

        // A concurrent activation narrowed it: this join was authorized
        // against a range that no longer exists.
        assert_eq!(activate(&connection), 1);
        assert_eq!(active_range(&connection), (5, 5));
        assert_eq!(
            connection
                .execute(&guarded, rusqlite::params![4_i64, 4_i64])
                .expect("run the guarded admission"),
            0,
            "an admission authorized against the old range must not commit"
        );

        // And the mirror case: authorized against 5..=5, deactivated under it.
        assert_eq!(
            connection
                .execute(&guarded, rusqlite::params![5_i64, 5_i64])
                .expect("run the guarded admission"),
            1
        );
        undo();
        assert_eq!(deactivate(&connection), 1);
        assert_eq!(
            connection
                .execute(&guarded, rusqlite::params![5_i64, 5_i64])
                .expect("run the guarded admission"),
            0,
            "a learner admitted into a rolled-back cluster is unrecoverable"
        );
    }

    /// Every statement that can publish a join carries the range guard.
    ///
    /// The predicate above only helps where it is used, and `redeem` has three
    /// admitting shapes — with a public HTTP origin, without one, and the
    /// legacy partial-redemption resume. Each builds the *first* statement of
    /// the transaction, which every later statement consumes, so the guard
    /// belongs on all three or the transaction can half-commit.
    #[test]
    fn every_admitting_statement_carries_the_protocol_range_guard() {
        let source = production_source();
        let redeem = source
            .split_once("pub async fn redeem(")
            .expect("redeem")
            .1
            .split_once("async fn upsert_hostname(")
            .expect("the method after redeem")
            .0;
        assert_eq!(
            redeem
                .matches("unchanged_protocol_range_predicate(")
                .count(),
            3,
            "each of redeem's three admitting shapes must carry the range guard"
        );
    }

    /// The case the heartbeat coupling exists to catch, constructed on purpose.
    ///
    /// A node was upgraded, wrote its capability, and was then rolled back to a
    /// binary that still heartbeats but knows nothing about protocol 5. The
    /// capability row survives with its old timestamp while `cluster_nodes`
    /// moves on. Any rule that only asked "does a capability row exist?" would
    /// activate onto a cluster containing a binary that cannot participate.
    #[test]
    fn a_capability_stranded_by_a_rollback_is_not_a_current_proof() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        assert_eq!(
            activate(&connection),
            1,
            "the proven cluster would activate"
        );
        connection
            .execute(
                "UPDATE cluster_meta SET protocol_min = 4, protocol_max = 4",
                [],
            )
            .expect("rewind the fixture to the unactivated range");

        // The rolled-back binary heartbeats: cluster_nodes advances, the
        // protocol-5 capability row it does not know about does not.
        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = last_seen_at + 1 \
                 WHERE node_id = 'node-b'",
                [],
            )
            .expect("old binary heartbeat");

        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM cluster_node_capabilities \
                     WHERE node_id = 'node-b' AND capability = ?1",
                    rusqlite::params![LEARNER_PROTOCOL_CAPABILITY],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count the stranded capability row"),
            1,
            "the stale row is still present; only its timestamp disproves it"
        );
        assert_eq!(unready_nodes(&connection), vec!["node-b".to_owned()]);
        assert_eq!(
            activate(&connection),
            0,
            "a capability older than its node's heartbeat is not a current proof"
        );
        assert_eq!(active_range(&connection), (4, 4));
    }

    /// A binary old enough to predate heartbeat intents is caught one step
    /// earlier: the replicated trigger drops every capability that node holds,
    /// including the protocol-5 one, the moment it writes a heartbeat.
    #[test]
    fn a_pre_intent_binary_heartbeat_invalidates_the_learner_capability() {
        let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
        connection
            .execute_batch(MARK_LEGACY_NODE_HEARTBEAT_DURING_REMOVAL_SQL)
            .expect("install the replicated legacy heartbeat marker");
        assert!(unready_nodes(&connection).is_empty());

        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = 999 WHERE node_id = 'node-c'",
                [],
            )
            .expect("a binary that writes no heartbeat intent");

        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM cluster_node_capabilities WHERE node_id = 'node-c'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count node-c capabilities"),
            0,
            "the trigger invalidates every capability the legacy writer's node held"
        );
        assert_eq!(unready_nodes(&connection), vec!["node-c".to_owned()]);
        assert_eq!(activate(&connection), 0);
    }

    // A mid-join node's effect on the pending roster *and* on the commit is
    // pinned by `a_join_in_flight_blocks_activation_without_making_the_pending_
    // roster_lie` above. The staging exclusion keeps the roster honest; it is
    // deliberately no longer permission to activate.

    /// The roster route answers during quorum loss; that is when an operator
    /// most needs it. Adding the protocol block to it must not turn it into a
    /// leader read, and the paths that commit a change must not become local
    /// ones. The separate-process membership scenario proves the first half
    /// end to end; this pins both halves where they are decided.
    #[test]
    fn the_roster_projection_reads_locally_and_the_decisions_do_not() {
        let source = production_source();
        let between = |from: &str, to: &str| {
            source
                .split_once(from)
                .unwrap_or_else(|| panic!("{from} is missing"))
                .1
                .split_once(to)
                .unwrap_or_else(|| panic!("{to} is missing after {from}"))
                .0
                .to_owned()
        };

        let status = between(
            "pub async fn status(&self)",
            "\n    /// Promote one ready learner",
        );
        assert!(
            !status.contains("query_consistent_map"),
            "the roster must stay readable without a quorum"
        );
        let projection = between(
            "pub async fn protocol_status(&self)",
            "\n    async fn protocol_projection",
        );
        assert!(
            projection.contains("Read::Local") && !projection.contains("Read::Quorum"),
            "the roster's protocol block must read the applied local state"
        );

        for decision in [
            between(
                "pub async fn activate_learner_protocol",
                "\n    /// Widen the cluster back",
            ),
            between(
                "pub async fn deactivate_learner_protocol",
                "\n    /// Restore the durable job-owner fence",
            ),
        ] {
            assert!(
                !decision.contains("Read::Local"),
                "a protocol change must never be decided from unapplied local state"
            );
            assert!(
                decision.contains("active_protocol_range") && decision.contains("Read::Quorum"),
                "a protocol change reads the range and its roster through the leader"
            );
        }
    }

    /// Deactivation must refuse before it can strand a member that only exists
    /// under the protocol being removed.
    ///
    /// This covers the half of the refusal that comes from committed Raft
    /// membership, which still runs as the preflight that names what is in the
    /// way — including a member whose durable row predates the role column.
    /// The replicated half rides in the committing statement and is covered by
    /// `deactivation_is_refused_by_its_own_statement_while_a_learner_is_admitted`;
    /// the end-to-end version, against a real admitted learner, is in
    /// `cluster::migration`.
    #[test]
    fn deactivation_is_refused_for_any_committed_member_without_a_vote() {
        let voters = BTreeSet::from([1_u64, 2, 3]);
        assert!(
            non_voting_members(&voters, [1_u64, 2, 3].into_iter()).is_empty(),
            "a voter-only cluster strands nothing"
        );
        assert_eq!(
            non_voting_members(&voters, [1_u64, 2, 3, 9, 7, 9].into_iter()),
            vec![7, 9],
            "every committed member without a vote is named, once, in order"
        );
        let refusal = MembershipError::LearnerProtocolInUse {
            admitted: Vec::new(),
            non_voting: vec!["7".to_owned(), "9".to_owned()],
        };
        assert_eq!(refusal.code(), "learner_protocol_in_use");
        assert!(refusal.to_string().contains("7, 9"), "{refusal}");
        assert_ne!(
            refusal.code(),
            MembershipError::LearnerProtocolUpgradeRequired(Vec::new()).code(),
            "an operator must be able to tell the two refusals apart"
        );
    }

    /// The two rosters name two different things, and the message has to keep
    /// them apart.
    ///
    /// The vendored client joins a voter with `add_learner` and only then
    /// `become_member`, so an ordinary voter is a committed non-voter for a
    /// moment. Unioning the rosters told the operator that voter had been
    /// "admitted as a learner" and that they should remove it — advice that
    /// would have destroyed a healthy join.
    #[test]
    fn a_mid_join_voter_is_not_reported_as_an_admitted_learner() {
        let mid_join = MembershipError::LearnerProtocolInUse {
            admitted: Vec::new(),
            non_voting: vec!["4".to_owned()],
        };
        let message = mid_join.to_string();
        assert!(
            !message.contains("admitted as learners"),
            "a mid-join voter must not be labelled an admitted learner: {message}"
        );
        assert!(message.contains("mid-join"), "{message}");
        assert!(message.contains('4'), "{message}");

        let learner = MembershipError::LearnerProtocolInUse {
            admitted: vec!["node-learner".to_owned()],
            non_voting: Vec::new(),
        };
        let message = learner.to_string();
        assert!(message.contains("admitted as learners"), "{message}");
        assert!(
            !message.contains("mid-join"),
            "an empty roster must leave no dangling label: {message}"
        );

        let both = MembershipError::LearnerProtocolInUse {
            admitted: vec!["node-learner".to_owned()],
            non_voting: vec!["4".to_owned()],
        };
        let message = both.to_string();
        assert!(message.contains("node-learner"), "{message}");
        assert!(message.contains("mid-join"), "{message}");
    }

    /// A lost compare-and-swap must not tell an operator to "upgrade these
    /// nodes: []". When the re-read roster comes back empty, what actually
    /// happened is that the range moved.
    ///
    /// The variant is asserted first, and then the two call sites that have to
    /// reach it. The call sites are pinned as text because the branch is a lost
    /// race between two linearizable writes on one leader: there is no fixture
    /// that loses it on demand, and asserting the variant alone left the whole
    /// finding revertible — putting `LearnerProtocolUpgradeRequired(Vec::new())`
    /// back kept every test green.
    #[test]
    fn a_lost_protocol_race_names_the_race_rather_than_an_empty_roster() {
        let refusal = MembershipError::ProtocolRangeChanged;
        assert_eq!(refusal.code(), "cluster_protocol_range_changed");
        let message = refusal.to_string();
        assert!(message.contains("retry"), "{message}");
        assert!(
            !message.contains("[]"),
            "an empty roster must never reach an operator: {message}"
        );

        let source = production_source();
        let between = |from: &str, to: &str| {
            source
                .split_once(from)
                .unwrap_or_else(|| panic!("{from} is missing"))
                .1
                .split_once(to)
                .unwrap_or_else(|| panic!("{to} is missing after {from}"))
                .0
                .to_owned()
        };
        for (name, body) in [
            (
                "activate_learner_protocol",
                between(
                    "pub async fn activate_learner_protocol(",
                    "/// The read-only pass behind",
                ),
            ),
            (
                "deactivate_learner_protocol",
                between(
                    "pub async fn deactivate_learner_protocol(",
                    "/// The read-only pass behind",
                ),
            ),
        ] {
            assert!(
                body.contains("MembershipError::ProtocolRangeChanged"),
                "{name} must say the range moved when its re-read names nobody: {body}"
            );
            assert!(
                !body.contains("LearnerProtocolUpgradeRequired")
                    && !body.contains("LearnerProtocolInUse"),
                "{name} must build its named refusals in the read-only pass, never inline with \
                 a roster it has not re-read: {body}"
            );
        }
    }

    /// Making the capability predicate generic must not have changed the
    /// removal capability's meaning; this pins the removal predicate's text
    /// against the literal that shipped before the refactor.
    #[test]
    fn the_generic_capability_predicate_preserves_the_removal_rule() {
        const SHIPPED: &str = "NOT EXISTS (SELECT 1 FROM cluster_nodes AS active \
       WHERE active.removed_at IS NULL \
         AND NOT EXISTS (SELECT 1 FROM cluster_node_join_staging AS staged \
           WHERE staged.node_id = active.node_id) \
         AND NOT EXISTS (SELECT 1 FROM cluster_node_capabilities AS capability \
           WHERE capability.node_id = active.node_id \
             AND capability.capability = 'membership_removal_attempt_refs_v1' \
             AND capability.last_seen_at = active.last_seen_at))";
        assert_eq!(
            capability_ready_predicate(REMOVAL_ATTEMPT_CAPABILITY),
            SHIPPED
        );
        assert_ne!(
            capability_ready_predicate(LEARNER_PROTOCOL_CAPABILITY),
            SHIPPED,
            "the two capabilities must be proven independently"
        );
    }

    /// The heartbeat is where the proof is made. If the capability write ever
    /// leaves that transaction, an upgraded-then-rolled-back node can look
    /// current, so pin the coupling itself.
    #[test]
    fn the_learner_capability_is_written_inside_the_heartbeat_transaction() {
        let commit_heartbeat = production_source()
            .split_once("async fn commit_heartbeat")
            .expect("commit_heartbeat")
            .1
            .split_once("\n    /// Publish the public half")
            .expect("end of commit_heartbeat")
            .0
            .to_owned();
        assert!(
            commit_heartbeat.contains("LEARNER_PROTOCOL_CAPABILITY"),
            "the learner capability must be written by the heartbeat itself"
        );
        // One statement list, submitted once. The heartbeat builds its
        // statements before it submits them so the capability write can be
        // omitted by a validation build, but every statement it builds still
        // reaches the same single Raft transaction.
        assert_eq!(
            commit_heartbeat.matches(".txn(").count(),
            1,
            "and inside the heartbeat's single Raft transaction"
        );
        assert!(
            commit_heartbeat.contains(".txn(statements)"),
            "which must be the statement list the heartbeat just built"
        );
        assert_eq!(
            commit_heartbeat
                .matches("LEARNER_PROTOCOL_CAPABILITY")
                .count(),
            1,
            "exactly one place may stamp this capability with a heartbeat time"
        );
    }

    /// The heartbeat may skip its capability proof only in a build the daemon
    /// never produces. A production `plurx-core` compiles a constant `false`
    /// instead, so there is no environment variable to set and no branch to
    /// take — which is what keeps the emulation from becoming a way for a real
    /// node to look current while running an old binary.
    #[test]
    fn the_capability_proof_can_only_be_skipped_by_a_validation_build() {
        let source = production_source();
        assert_eq!(
            source
                .matches("fn emulate_pre_learner_protocol_heartbeat")
                .count(),
            2,
            "one gated definition and one production constant"
        );
        assert_eq!(
            source
                .matches("if !emulate_pre_learner_protocol_heartbeat() {")
                .count(),
            1,
            "and exactly one caller, in the heartbeat"
        );
        assert!(source.contains(
            "#[cfg(feature = \"cluster-validation\")]\nfn emulate_pre_learner_protocol_heartbeat("
        ));
        assert!(source.contains(
            "#[cfg(not(feature = \"cluster-validation\"))]\nconst fn \
             emulate_pre_learner_protocol_heartbeat() -> bool {\n    false\n}"
        ));
        assert_eq!(
            source
                .matches("PLURX_VALIDATION_PRE_LEARNER_HEARTBEAT")
                .count(),
            1,
            "the emulation is reachable only through its own gated definition"
        );
    }

    /// This module's own source, so a text-shape assertion cannot be satisfied
    /// by the string literal that states it.
    fn production_source() -> String {
        include_str!("membership.rs")
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("the test module")
            .0
            .to_owned()
    }

    #[test]
    fn removal_attempt_and_active_media_session_are_mutually_exclusive() {
        let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (singleton INTEGER PRIMARY KEY); \
                 CREATE TABLE media_sessions (owner_node_id TEXT, state TEXT, \
                   lease_expires_at_ms INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 10, NULL); \
                 INSERT INTO cluster_node_capabilities VALUES (\
                   'node-a', 'membership_removal_attempt_refs_v1', 10); \
                 INSERT INTO media_sessions VALUES ('node-a', 'active', 100);",
            )
            .expect("seed active media owner");

        assert_eq!(
            connection
                .execute(
                    &begin_removal_attempt_sql(false),
                    rusqlite::params!["node-a", "blocked-attempt", 50],
                )
                .expect("refuse removal while media is active"),
            0
        );
        connection
            .execute(
                "UPDATE media_sessions SET state = 'ended' WHERE owner_node_id = 'node-a'",
                [],
            )
            .expect("drain media owner");
        assert_eq!(
            connection
                .execute(
                    &begin_removal_attempt_sql(false),
                    rusqlite::params!["node-a", "admitted-attempt", 50],
                )
                .expect("admit removal after media drains"),
            1
        );
        connection
            .execute("DELETE FROM cluster_node_removal_attempts", [])
            .expect("reset removal attempt");
        connection
            .execute("INSERT INTO cluster_node_maintenance VALUES ('node-b')", [])
            .expect("begin maintenance");
        assert_eq!(
            connection
                .execute(
                    &begin_removal_attempt_sql(true),
                    rusqlite::params!["node-a", "maintenance-blocked"],
                )
                .expect("maintenance blocks removal"),
            0
        );
        connection
            .execute("DELETE FROM cluster_node_maintenance", [])
            .expect("finish maintenance");
        connection
            .execute("DELETE FROM cluster_node_removal_attempts", [])
            .expect("reset removal attempt");
        connection
            .execute(
                "UPDATE media_sessions SET state = 'active' WHERE owner_node_id = 'node-a'",
                [],
            )
            .expect("restore active media owner");
        assert_eq!(
            connection
                .execute(
                    &begin_removal_attempt_sql(true),
                    rusqlite::params!["node-a", "draining-attempt"],
                )
                .expect("draining removal binds only the placeholders it emits"),
            1
        );
    }

    #[test]
    fn removal_media_fence_first_writes_replaced_and_blocks_terminal_projection() {
        let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE media_sessions (\
                   session_id TEXT PRIMARY KEY, owner_node_id TEXT NOT NULL, state TEXT NOT NULL, \
                   terminal_reason TEXT, publication_ready_at_ms INTEGER NOT NULL, \
                   lease_expires_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-a', 'attempt-a'); \
                 INSERT INTO media_sessions VALUES \
                   ('new-cause', 'node-a', 'active', NULL, 700, 900, 100), \
                   ('prior-cause', 'node-a', 'active', 'admin_stop', 800, 900, 100), \
                   ('other-owner', 'node-b', 'active', NULL, 900, 900, 100);",
            )
            .expect("seed removal media fence");

        assert_eq!(
            connection
                .execute(
                    BEGIN_REMOVAL_MEDIA_FENCE_SQL,
                    rusqlite::params![
                        500,
                        crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                        "node-a",
                        "attempt-a",
                    ],
                )
                .expect("apply exact removal media fence"),
            2
        );

        let route = |session_id: &str| {
            connection
                .query_row(
                    "SELECT state, terminal_reason, publication_ready_at_ms, \
                            lease_expires_at_ms, updated_at_ms \
                       FROM media_sessions WHERE session_id = ?1",
                    [session_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .expect("read fenced media route")
        };
        assert_eq!(
            route("new-cause"),
            (
                "ended".to_owned(),
                Some("replaced".to_owned()),
                crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                500,
                500
            )
        );
        assert_eq!(
            route("prior-cause"),
            (
                "ended".to_owned(),
                Some("admin_stop".to_owned()),
                crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                500,
                500,
            )
        );
        assert_eq!(
            route("other-owner"),
            ("active".to_owned(), None, 900, 900, 100)
        );
    }

    #[test]
    fn one_attempt_cannot_roll_back_a_concurrent_removal_fence() {
        fn rollback(connection: &mut rusqlite::Connection, node_id: &str, attempt_id: &str) {
            let transaction = connection.transaction().expect("rollback transaction");
            transaction
                .execute(
                    &rollback_removal_attempt_sql(),
                    rusqlite::params![node_id, attempt_id],
                )
                .expect("release attempt when rollout permits");
            transaction
                .execute(
                    ROLLBACK_REMOVAL_OWNER_FENCE_SQL,
                    rusqlite::params![removed_job_owner_key(node_id), node_id],
                )
                .expect("conditionally release owner fence");
            transaction
                .execute(ROLLBACK_REMOVAL_FENCE_SQL, rusqlite::params![node_id])
                .expect("conditionally release removal fence");
            transaction.commit().expect("commit rollback");
        }

        let mut connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, started_at INTEGER); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_node_removal_intents (\
                   node_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER, \
                   last_seen_at INTEGER, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_heartbeat_intents (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (singleton INTEGER PRIMARY KEY); \
                 CREATE TABLE cluster_join_tokens (node_id TEXT, state TEXT); \
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER); \
                 CREATE TABLE job_leases (owner_node_id TEXT, expires_at_ms INTEGER, \
                   revision INTEGER, updated_at_ms INTEGER); \
                 CREATE TABLE media_sessions (owner_node_id TEXT, state TEXT, \
                   lease_expires_at_ms INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 1, 10, NULL); \
                 INSERT INTO cluster_nodes VALUES ('node-old', 2, 20, NULL); \
                 INSERT INTO cluster_nodes VALUES ('staged-join', 99, 30, NULL); \
                 INSERT INTO cluster_node_join_staging VALUES ('staged-join'); \
                 INSERT INTO cluster_join_tokens VALUES ('staged-join', 'redeeming'); \
                 INSERT INTO cluster_node_capabilities VALUES (\
                   'node-a', 'membership_removal_attempt_refs_v1', 10); \
                 INSERT INTO cluster_node_removals VALUES ('node-a', 1); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-a', 'attempt-a'); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-a', 'attempt-b'); \
                 INSERT INTO settings VALUES (\
                   'internal.cluster_job_owner_removed.node-a', '1', 0); \
                 INSERT INTO job_leases VALUES ('blocked', 100, 1, 0);",
            )
            .expect("seed concurrent attempts");
        connection
            .execute_batch(PROTECT_REMOVAL_FENCE_DELETE_SQL)
            .expect("install legacy removal guard");
        connection
            .execute_batch(PROTECT_REMOVAL_OWNER_FENCE_DELETE_SQL)
            .expect("install legacy owner guard");
        connection
            .execute_batch(MARK_LEGACY_NODE_INSERT_DURING_REMOVAL_SQL)
            .expect("install legacy insert marker");
        connection
            .execute_batch(MARK_LEGACY_NODE_HEARTBEAT_DURING_REMOVAL_SQL)
            .expect("install legacy heartbeat marker");
        connection
            .execute_batch(MARK_LEGACY_NODE_FINALIZE_DURING_REMOVAL_SQL)
            .expect("install legacy finalize marker");

        // A rolling cluster refuses a fresh removal before it creates any
        // attempt or shared fence. The staged join is excluded; node-old is
        // the active finalized node that lacks the capability.
        let transaction = connection
            .transaction()
            .expect("mixed-version begin transaction");
        assert_eq!(
            transaction
                .execute(
                    &begin_removal_attempt_sql(false),
                    rusqlite::params!["blocked", "blocked-attempt", 0],
                )
                .expect("gate mixed-version begin"),
            0
        );
        transaction
            .execute(
                BEGIN_REMOVAL_INTENT_SQL,
                rusqlite::params!["blocked", "blocked-attempt"],
            )
            .expect("guard mixed-version removal intent");
        transaction
            .execute(
                BEGIN_REMOVAL_FENCE_SQL,
                rusqlite::params!["blocked", 40, "blocked-attempt"],
            )
            .expect("guard mixed-version removal fence");
        transaction
            .execute(
                BEGIN_REMOVAL_OWNER_FENCE_SQL,
                rusqlite::params![
                    "internal.cluster_job_owner_removed.blocked",
                    40,
                    "blocked",
                    "blocked-attempt"
                ],
            )
            .expect("guard mixed-version owner fence");
        transaction
            .execute(
                BEGIN_REMOVAL_JOB_FENCE_SQL,
                rusqlite::params![40, "blocked", "blocked-attempt"],
            )
            .expect("guard mixed-version job fence");
        transaction
            .execute(
                CLEAR_REMOVAL_INTENT_SQL,
                rusqlite::params!["blocked", "blocked-attempt"],
            )
            .expect("consume mixed-version removal intent");
        transaction.commit().expect("commit refused begin");
        let blocked_side_effects: i64 = connection
            .query_row(
                "SELECT \
                   (SELECT COUNT(*) FROM cluster_node_removal_attempts \
                     WHERE node_id = 'blocked') + \
                   (SELECT COUNT(*) FROM cluster_node_removal_intents \
                     WHERE node_id = 'blocked') + \
                   (SELECT COUNT(*) FROM cluster_node_removals WHERE node_id = 'blocked') + \
                   (SELECT COUNT(*) FROM settings \
                     WHERE key = 'internal.cluster_job_owner_removed.blocked') + \
                   (SELECT COUNT(*) FROM job_leases \
                     WHERE owner_node_id != 'blocked' OR revision != 1)",
                [],
                |row| row.get(0),
            )
            .expect("count refused begin side effects");
        assert_eq!(blocked_side_effects, 0);

        // An old binary's unconditional rollback cannot erase a fence that a
        // reference-aware attempt currently protects.
        assert_eq!(
            connection
                .execute(
                    "DELETE FROM cluster_node_removals WHERE node_id = 'node-a'",
                    [],
                )
                .expect("legacy removal delete"),
            0
        );
        assert_eq!(
            connection
                .execute(
                    "DELETE FROM settings WHERE key = \
                     'internal.cluster_job_owner_removed.node-a'",
                    [],
                )
                .expect("legacy owner-fence delete"),
            0
        );

        connection
            .execute(
                "INSERT INTO cluster_node_capabilities VALUES ($1, $2, $3)",
                rusqlite::params!["node-old", REMOVAL_ATTEMPT_CAPABILITY, 20],
            )
            .expect("finish capability rollout");

        // The exact current heartbeat sequence carries a transaction-local
        // intent, so the legacy trigger neither deletes its capability nor
        // pins the active removal.
        let transaction = connection
            .transaction()
            .expect("current heartbeat transaction");
        transaction
            .execute(
                "INSERT INTO cluster_node_heartbeat_intents VALUES ($1, $2) \
                 ON CONFLICT(node_id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                rusqlite::params!["node-a", 11],
            )
            .expect("publish heartbeat intent");
        transaction
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = 11 WHERE node_id = 'node-a'",
                [],
            )
            .expect("apply current heartbeat");
        transaction
            .execute(
                "INSERT INTO cluster_node_capabilities VALUES ($1, $2, $3) \
                 ON CONFLICT(node_id, capability) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                rusqlite::params!["node-a", REMOVAL_ATTEMPT_CAPABILITY, 11],
            )
            .expect("refresh current capability");
        transaction
            .execute(
                "DELETE FROM cluster_node_join_staging WHERE node_id = 'node-a'",
                [],
            )
            .expect("clear current staging marker");
        transaction
            .execute(
                "DELETE FROM cluster_node_heartbeat_intents WHERE node_id = 'node-a' \
                 AND last_seen_at = 11",
                [],
            )
            .expect("consume heartbeat intent");
        transaction.commit().expect("commit current heartbeat");
        let legacy_refs: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_removal_attempts \
                 WHERE node_id = 'node-a' AND attempt_id = 'internal.legacy-writer'",
                [],
                |row| row.get(0),
            )
            .expect("count current-heartbeat legacy refs");
        assert_eq!(legacy_refs, 0);

        rollback(&mut connection, "node-a", "attempt-a");
        let shared_fence: i64 = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM cluster_node_removals WHERE node_id = 'node-a') + \
                        (SELECT COUNT(*) FROM settings WHERE key = \
                          'internal.cluster_job_owner_removed.node-a')",
                [],
                |row| row.get(0),
            )
            .expect("count concurrent shared fence");
        assert_eq!(shared_fence, 2);
        rollback(&mut connection, "node-a", "attempt-b");

        let remaining_attempts: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_removal_attempts WHERE node_id = 'node-a'",
                [],
                |row| row.get(0),
            )
            .expect("count remaining attempts");
        let fences: i64 = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM cluster_node_removals WHERE node_id = 'node-a') + \
                        (SELECT COUNT(*) FROM settings WHERE key = \
                          'internal.cluster_job_owner_removed.node-a')",
                [],
                |row| row.get(0),
            )
            .expect("count remaining fences");
        assert_eq!(remaining_attempts, 0);
        assert_eq!(fences, 0);

        // If a legacy heartbeat appears after begin, the replicated trigger
        // invalidates its capability and pins an unreleasable reference before
        // that process can send an untracked removal proposal.
        connection
            .execute_batch(
                "INSERT INTO cluster_node_removals VALUES ('node-gap', 2); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-gap', 'gap-attempt'); \
                 INSERT INTO settings VALUES (\
                   'internal.cluster_job_owner_removed.node-gap', '1', 0);",
            )
            .expect("seed capability-gap attempt");
        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = 21 WHERE node_id = 'node-old'",
                [],
            )
            .expect("apply legacy heartbeat");
        connection
            .execute(
                "INSERT INTO cluster_node_capabilities VALUES ($1, $2, $3) \
                 ON CONFLICT(node_id, capability) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                rusqlite::params!["node-old", REMOVAL_ATTEMPT_CAPABILITY, 21],
            )
            .expect("upgrade legacy heartbeat writer");
        rollback(&mut connection, "node-gap", "gap-attempt");

        let remaining_attempts: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_removal_attempts \
                 WHERE node_id = 'node-gap'",
                [],
                |row| row.get(0),
            )
            .expect("count final attempts");
        let fences: i64 = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM cluster_node_removals \
                          WHERE node_id = 'node-gap') + \
                        (SELECT COUNT(*) FROM settings WHERE key = \
                          'internal.cluster_job_owner_removed.node-gap')",
                [],
                |row| row.get(0),
            )
            .expect("count final fences");
        assert_eq!(remaining_attempts, 1);
        assert_eq!(fences, 2);
    }

    #[test]
    fn legacy_join_finalize_pins_an_active_removal_fence() {
        let mut connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, started_at INTEGER); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_join_tokens (node_id TEXT, state TEXT); \
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('legacy-join', 30, NULL); \
                 INSERT INTO cluster_node_join_staging VALUES ('legacy-join'); \
                 INSERT INTO cluster_join_tokens VALUES ('legacy-join', 'redeeming'); \
                 INSERT INTO cluster_node_removals VALUES ('target', 1); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('target', 'attempt'); \
                 INSERT INTO settings VALUES (\
                   'internal.cluster_job_owner_removed.target', '1', 0);",
            )
            .expect("seed staged legacy finalize");
        connection
            .execute_batch(MARK_LEGACY_NODE_FINALIZE_DURING_REMOVAL_SQL)
            .expect("install legacy finalize marker");

        connection
            .execute(
                "UPDATE cluster_join_tokens SET state = 'redeemed' \
                 WHERE node_id = 'legacy-join'",
                [],
            )
            .expect("finalize staged legacy join");
        let staging_and_legacy_ref: (i64, i64) = connection
            .query_row(
                "SELECT \
                   (SELECT COUNT(*) FROM cluster_node_join_staging \
                     WHERE node_id = 'legacy-join'), \
                   (SELECT COUNT(*) FROM cluster_node_removal_attempts \
                     WHERE node_id = 'target' \
                       AND attempt_id = 'internal.legacy-writer')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read finalize protection");
        assert_eq!(staging_and_legacy_ref, (0, 1));

        let transaction = connection.transaction().expect("rollback transaction");
        transaction
            .execute(
                &rollback_removal_attempt_sql(),
                rusqlite::params!["target", "attempt"],
            )
            .expect("release normal attempt");
        transaction
            .execute(
                ROLLBACK_REMOVAL_OWNER_FENCE_SQL,
                rusqlite::params![removed_job_owner_key("target"), "target"],
            )
            .expect("retain shared owner fence");
        transaction
            .execute(ROLLBACK_REMOVAL_FENCE_SQL, rusqlite::params!["target"])
            .expect("retain shared removal fence");
        transaction.commit().expect("commit rollback");

        let fences: i64 = connection
            .query_row(
                "SELECT \
                   (SELECT COUNT(*) FROM cluster_node_removals \
                     WHERE node_id = 'target') + \
                   (SELECT COUNT(*) FROM settings WHERE key = \
                     'internal.cluster_job_owner_removed.target')",
                [],
                |row| row.get(0),
            )
            .expect("count pinned shared fences");
        assert_eq!(fences, 2);
    }

    #[test]
    fn replicated_guard_refuses_legacy_removal_and_backfills_existing_fences() {
        let mut connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, started_at INTEGER); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_node_removal_intents (\
                   node_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL); \
                 INSERT INTO cluster_node_removals VALUES ('already-pending', 1);",
            )
            .expect("seed pre-upgrade removal fence");
        let transaction = connection.transaction().expect("install guard transaction");
        transaction
            .execute(BACKFILL_REMOVAL_ATTEMPT_REFS_SQL, [])
            .expect("backfill pre-upgrade attempt reference");
        transaction
            .execute_batch(REQUIRE_REMOVAL_INTENT_SQL)
            .expect("install removal-intent guard");
        transaction.commit().expect("commit guard installation");

        let backfilled: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_removal_attempts \
                 WHERE node_id = 'already-pending' \
                   AND attempt_id = 'internal.preexisting'",
                [],
                |row| row.get(0),
            )
            .expect("count backfilled reference");
        assert_eq!(backfilled, 1);
        assert!(connection
            .execute(
                "INSERT INTO cluster_node_removals VALUES ('legacy-new', 2)",
                [],
            )
            .is_err());
        assert!(connection
            .execute(
                "INSERT INTO cluster_node_removals VALUES ('already-pending', 2) \
                 ON CONFLICT(node_id) DO NOTHING",
                [],
            )
            .is_err());

        let transaction = connection.transaction().expect("current begin transaction");
        transaction
            .execute(
                "INSERT INTO cluster_node_removal_attempts VALUES ('current', 'uuid-attempt')",
                [],
            )
            .expect("insert current attempt");
        transaction
            .execute(
                BEGIN_REMOVAL_INTENT_SQL,
                rusqlite::params!["current", "uuid-attempt"],
            )
            .expect("insert current intent");
        transaction
            .execute(
                "INSERT INTO cluster_node_removals VALUES ('current', 3)",
                [],
            )
            .expect("insert current removal fence");
        transaction
            .execute(
                CLEAR_REMOVAL_INTENT_SQL,
                rusqlite::params!["current", "uuid-attempt"],
            )
            .expect("consume current intent");
        transaction.commit().expect("commit current begin");

        let current_state: (i64, i64) = connection
            .query_row(
                "SELECT \
                   (SELECT COUNT(*) FROM cluster_node_removals \
                     WHERE node_id = 'current'), \
                   (SELECT COUNT(*) FROM cluster_node_removal_intents \
                     WHERE node_id = 'current')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read current removal state");
        assert_eq!(current_state, (1, 0));
    }

    fn payload() -> JoinPayload {
        JoinPayload {
            version: JOIN_TOKEN_VERSION,
            cluster_id: "cluster-a".to_owned(),
            raft_id: 2,
            expires_at: 123,
            bootstrap_http: "http://127.0.0.1:32400".to_owned(),
            bootstrap: vec![ClusterPeer {
                raft_id: 1,
                raft_address: "127.0.0.1:32401".to_owned(),
                api_address: "127.0.0.1:32402".to_owned(),
            }],
            secrets: JoinSecretPayload {
                raft: "r".repeat(64),
                api: "a".repeat(64),
                credential_key: "c".repeat(64),
            },
            activation_marker: ActivationMarker {
                marker_version: 1,
                cluster_id: "cluster-a".to_owned(),
                source_backup_sha256: "f".repeat(64),
                source_schema_version: 18,
                replicated_schema_version: AUTH_SCHEMA_VERSION,
                imported_rows: 1,
                table_hashes: vec![crate::store::SqliteImportTableDigest {
                    table: "settings".to_owned(),
                    row_count: 1,
                    sha256: "e".repeat(64),
                }],
                admitted_role: None,
            },
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: AUTH_PROTOCOL_MIN,
        }
    }

    /// The v2 payload for the same fixture cluster, as a learner token.
    fn learner_payload() -> JoinPayloadV2 {
        let v1 = payload();
        JoinPayloadV2 {
            version: JOIN_TOKEN_V2_VERSION,
            cluster_id: v1.cluster_id,
            raft_id: v1.raft_id,
            expires_at: v1.expires_at,
            bootstrap_http: v1.bootstrap_http,
            bootstrap: v1.bootstrap,
            secrets: v1.secrets,
            activation_marker: v1.activation_marker,
            schema_version: v1.schema_version,
            protocol_min: AUTH_LEARNER_PROTOCOL,
            protocol_max: AUTH_LEARNER_PROTOCOL,
            role: ClusterRole::Learner,
        }
    }

    #[test]
    fn join_token_round_trips_without_plaintext_payload() {
        let expected = payload();
        let token = encode_join_token(&expected).expect("encode token");
        assert!(token.starts_with(JOIN_TOKEN_PREFIX));
        assert!(!token.contains("cluster-a"));
        assert!(!token.contains(&"r".repeat(64)));
        assert_eq!(
            decode_join_token(&token).expect("decode token"),
            JoinToken::V1(expected)
        );
    }

    /// The previous release's join-token decoder, transcribed.
    ///
    /// A v1-only coordinator or joiner has to refuse a v2 token rather than
    /// misread it, and "it obviously will" is not a proof. This is the exact
    /// shipped v1 rule — prefix, then AAD, then version — so the assertions
    /// below test the refusal rather than assuming it.
    fn decode_join_token_as_the_previous_release(
        token: &str,
    ) -> Result<JoinPayload, MembershipError> {
        let mut parts = token.split(':');
        if parts.next() != Some("plxjoin")
            || parts.next() != Some("v1")
            || parts.clone().count() != 2
        {
            return Err(MembershipError::InvalidToken);
        }
        let key_bytes = hex::decode(parts.next().ok_or(MembershipError::InvalidToken)?)
            .map_err(|_| MembershipError::InvalidToken)?;
        let encrypted = hex::decode(parts.next().ok_or(MembershipError::InvalidToken)?)
            .map_err(|_| MembershipError::InvalidToken)?;
        if key_bytes.len() != 32 || encrypted.len() <= 24 {
            return Err(MembershipError::InvalidToken);
        }
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&key_bytes));
        let plaintext = cipher
            .decrypt(
                XNonce::from_slice(&encrypted[..24]),
                chacha20poly1305::aead::Payload {
                    msg: &encrypted[24..],
                    aad: JOIN_TOKEN_AAD,
                },
            )
            .map_err(|_| MembershipError::InvalidToken)?;
        let payload: JoinPayload =
            serde_json::from_slice(&plaintext).map_err(|_| MembershipError::InvalidToken)?;
        if payload.version != JOIN_TOKEN_VERSION || payload.cluster_id.trim().is_empty() {
            return Err(MembershipError::InvalidToken);
        }
        Ok(payload)
    }

    /// Decrypt a token's payload under an explicit associated data, so a test
    /// can assert on the *layer* that refused rather than on a code every
    /// layer shares.
    ///
    /// Every refusal in `decode_join_token` is `join_token_invalid`, which is
    /// the right thing for an attacker to see and useless for telling three
    /// independent defences apart. This reaches past the decoder and asks the
    /// AEAD directly.
    fn aead_opens(token: &str, aad: &[u8]) -> bool {
        let parts = token.split(':').collect::<Vec<_>>();
        assert_eq!(parts.len(), 4, "token framing changed: {token}");
        let key_bytes = hex::decode(parts[2]).expect("token key");
        let encrypted = hex::decode(parts[3]).expect("token ciphertext");
        XChaCha20Poly1305::new(Key::from_slice(&key_bytes))
            .decrypt(
                XNonce::from_slice(&encrypted[..24]),
                chacha20poly1305::aead::Payload {
                    msg: &encrypted[24..],
                    aad,
                },
            )
            .is_ok()
    }

    /// Three independent refusals, each asserted at the layer that produces
    /// it.
    ///
    /// The prefix stops the ordinary case. Rewriting the prefix defeats that
    /// check and the AEAD's associated data stops it instead. Re-sealing the
    /// same payload under v1's key derivation defeats *that*, and the version
    /// field stops it. An old build has to fail all three ways, because a
    /// single one of them could be lost to a future refactor.
    ///
    /// Asserting `.code() == "join_token_invalid"` three times cannot tell
    /// which layer fired — every layer returns it — so collapsing
    /// `JOIN_TOKEN_V2_AAD` onto v1's value left this test green while the
    /// second defence no longer existed. The AEAD is now asked directly.
    #[test]
    fn a_previous_release_refuses_a_v2_token_three_separate_ways() {
        let token = encode_join_token_v2(&learner_payload()).expect("encode a v2 token");
        assert!(token.starts_with(JOIN_TOKEN_V2_PREFIX));
        assert_eq!(
            decode_join_token_as_the_previous_release(&token)
                .expect_err("a v1-only build must refuse the v2 prefix")
                .code(),
            "join_token_invalid"
        );

        // 2. Prefix relabelled to v1: the associated data no longer matches,
        //    so the payload cannot even be decrypted.
        let relabelled = token.replacen(JOIN_TOKEN_V2_PREFIX, JOIN_TOKEN_PREFIX, 1);
        // The layer itself, not the shared code the decoder returns: a v2
        // token's ciphertext does not open under v1's associated data, and
        // does open under its own. Two assertions, because only the pair
        // proves the difference is the AAD rather than a broken token.
        assert!(
            !aead_opens(&relabelled, JOIN_TOKEN_AAD),
            "a v2 token must not decrypt under v1's associated data"
        );
        assert!(
            aead_opens(&relabelled, JOIN_TOKEN_V2_AAD),
            "and it must decrypt under its own, or the test above proves nothing"
        );
        assert_ne!(
            JOIN_TOKEN_AAD, JOIN_TOKEN_V2_AAD,
            "the two framings must not share associated data"
        );
        assert_eq!(
            decode_join_token_as_the_previous_release(&relabelled)
                .expect_err("a relabelled v2 token must fail its AEAD check")
                .code(),
            "join_token_invalid"
        );
        // And this build refuses it too: a v1 frame is read with the v1 rules.
        assert_eq!(
            decode_join_token(&relabelled)
                .expect_err("this build must not read a v2 payload through the v1 frame")
                .code(),
            "join_token_invalid"
        );

        // 3. Re-sealed under v1's associated data, so decryption succeeds and
        //    only the version field is left to refuse it.
        let resealed = seal_join_token(&learner_payload(), JOIN_TOKEN_PREFIX, JOIN_TOKEN_AAD)
            .expect("re-seal the learner payload under v1 framing");
        assert_eq!(
            decode_join_token_as_the_previous_release(&resealed)
                .expect_err("version 2 is not a v1 payload")
                .code(),
            "join_token_invalid"
        );
        assert_eq!(
            decode_join_token(&resealed)
                .expect_err("nor is it one for this build")
                .code(),
            "join_token_invalid"
        );

        // The v1 token this build still mints stays readable by that release.
        let voter = encode_join_token(&payload()).expect("encode a v1 token");
        let decoded = decode_join_token_as_the_previous_release(&voter)
            .expect("the previous release still reads a voter token");
        assert_eq!(decoded, payload());
    }

    /// The v2 frame's own version gate, reached through the production
    /// decoder.
    ///
    /// Every v2-version assertion above goes through the v1 arm — a payload
    /// re-sealed under v1 framing is read by v1's rules — so the production v2
    /// arm's `payload.version != JOIN_TOKEN_V2_VERSION` was never executed and
    /// deleting it kept everything green. A payload sealed with the *v2*
    /// prefix and the *v2* associated data is the only thing that reaches it:
    /// framing and AEAD both pass, and the version field is all that is left.
    #[test]
    fn the_v2_frame_refuses_a_payload_that_is_not_version_two() {
        let mut wrong_version = learner_payload();
        wrong_version.version = JOIN_TOKEN_V2_VERSION + 1;
        let token = seal_join_token(&wrong_version, JOIN_TOKEN_V2_PREFIX, JOIN_TOKEN_V2_AAD)
            .expect("seal a v2-framed payload carrying the wrong version");

        // The two layers before the version gate both pass, so the refusal
        // below can only come from the gate itself.
        assert!(token.starts_with(JOIN_TOKEN_V2_PREFIX));
        assert!(
            aead_opens(&token, JOIN_TOKEN_V2_AAD),
            "the AEAD must accept this token, or the version gate is unreached"
        );
        assert_eq!(
            decode_join_token(&token)
                .expect_err("a v2 frame must refuse a payload that is not version 2")
                .code(),
            "join_token_invalid"
        );

        // The same shape one version below, so the gate is equality and not a
        // minimum that a future v3 payload would sail through.
        let mut older = learner_payload();
        older.version = JOIN_TOKEN_V2_VERSION - 1;
        let token = seal_join_token(&older, JOIN_TOKEN_V2_PREFIX, JOIN_TOKEN_V2_AAD)
            .expect("seal a v2-framed payload carrying an older version");
        assert!(decode_join_token(&token).is_err());

        // And the honest payload still decodes, so none of the above is
        // passing for an unrelated reason.
        let good = encode_join_token_v2(&learner_payload()).expect("encode a v2 token");
        assert_eq!(
            decode_join_token(&good).expect("a real v2 token decodes"),
            JoinToken::V2(learner_payload())
        );
    }

    /// A token says what it admits, and a v1 token can only ever say "voter".
    #[test]
    fn a_decoded_token_reports_its_role_and_the_range_it_was_minted_for() {
        let voter = encode_join_token(&payload()).expect("encode a v1 token");
        let voter = decode_join_token(&voter).expect("decode the v1 token");
        assert_eq!(voter.role(), ClusterRole::Voter);
        assert_eq!(
            voter.declared_protocol_range(),
            (AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN),
            "a v1 token's one scalar is the one-element range it always was"
        );

        let learner = encode_join_token_v2(&learner_payload()).expect("encode a v2 token");
        assert!(
            !learner.contains("learner"),
            "the role must not be plaintext"
        );
        let learner = decode_join_token(&learner).expect("decode the v2 token");
        assert_eq!(learner.role(), ClusterRole::Learner);
        assert_eq!(
            learner.declared_protocol_range(),
            (AUTH_LEARNER_PROTOCOL, AUTH_LEARNER_PROTOCOL)
        );
    }

    /// A framing this build has never heard of is refused before a key is
    /// derived — which is what a joiner relies on to leave nothing on disk.
    #[test]
    fn an_unknown_token_framing_is_refused() {
        let token = encode_join_token_v2(&learner_payload()).expect("encode a v2 token");
        let future = token.replacen(JOIN_TOKEN_V2_PREFIX, "plxjoin:v3", 1);
        assert_eq!(
            decode_join_token(&future)
                .expect_err("a future framing is not something this build may guess at")
                .code(),
            "join_token_invalid"
        );
    }

    /// `membership.json` versions and roles are one fact written twice, and a
    /// record where the two disagree is refused rather than resolved.
    #[test]
    fn a_membership_record_version_names_exactly_one_role() {
        assert_eq!(local_membership_version(ClusterRole::Voter), 1);
        assert_eq!(local_membership_version(ClusterRole::Learner), 2);
        assert!(local_membership_version_matches_role(1, ClusterRole::Voter));
        assert!(local_membership_version_matches_role(
            2,
            ClusterRole::Learner
        ));
        // A voter keeps writing version 1 so the previous release can still
        // read it; a learner writes version 2 so that release refuses it.
        assert!(!local_membership_version_matches_role(
            1,
            ClusterRole::Learner
        ));
        assert!(!local_membership_version_matches_role(
            2,
            ClusterRole::Voter
        ));
        assert!(!local_membership_version_matches_role(
            3,
            ClusterRole::Voter
        ));
    }

    #[test]
    fn malformed_and_tampered_tokens_have_one_stable_refusal() {
        assert_eq!(
            decode_join_token("not-a-token")
                .expect_err("malformed")
                .code(),
            "join_token_invalid"
        );
        let token = encode_join_token(&payload()).expect("encode token");
        let mut bytes = token.into_bytes();
        let last = bytes.last_mut().expect("token byte");
        *last = if *last == b'0' { b'1' } else { b'0' };
        let tampered = String::from_utf8(bytes).expect("ascii token");
        assert_eq!(
            decode_join_token(&tampered).expect_err("tampered").code(),
            "join_token_invalid"
        );
    }

    #[test]
    fn stable_membership_refusal_codes_are_operator_visible() {
        assert_eq!(MembershipError::ExpiredToken.code(), "join_token_expired");
        assert_eq!(MembershipError::ReusedToken.code(), "join_token_reused");
        assert_eq!(
            MembershipError::ClusterOperationPending.code(),
            "cluster_operation_pending"
        );
        assert_eq!(
            MembershipError::QuorumLoss.code(),
            "removal_would_lose_quorum"
        );
        // Activation's three refusals are three different operator actions —
        // wait, start-or-remove, upgrade — so they cannot share a code.
        assert_eq!(
            MembershipError::JoinInFlight(vec!["node-joining".to_owned()]).code(),
            "join_in_flight"
        );
        assert_eq!(
            MembershipError::LearnerProtocolNodeAbsent {
                nodes: vec!["node-c".to_owned()],
                minutes: 2,
            }
            .code(),
            "learner_protocol_node_absent"
        );
        assert_eq!(
            MembershipError::LearnerProtocolUpgradeRequired(vec!["node-b".to_owned()]).code(),
            "learner_protocol_upgrade_required"
        );
        // And each names what is in the way.
        assert!(
            MembershipError::JoinInFlight(vec!["node-joining".to_owned()])
                .to_string()
                .contains("node-joining")
        );
        let absent = MembershipError::LearnerProtocolNodeAbsent {
            nodes: vec!["node-c".to_owned()],
            minutes: 2,
        }
        .to_string();
        assert!(absent.contains("node-c"), "{absent}");
        assert!(absent.contains("2 minutes"), "{absent}");
    }

    #[test]
    fn leader_change_remains_typed_without_changing_the_public_error_contract() {
        let error = MembershipError::from(hiqlite::Error::LeaderChange(
            "routing converged after election".into(),
        ));
        assert!(matches!(error, MembershipError::LeaderChanged(_)));
        assert_eq!(error.code(), "membership_internal");
        assert_eq!(
            error.to_string(),
            "cluster membership operation failed: LeaderChange: routing converged after election"
        );
    }

    #[test]
    fn repair_observation_cannot_cross_the_generation_cas_boundary() {
        let source = production_source();
        let observation = source
            .split_once("pub async fn observe_artwork_source_repair")
            .expect("observation method")
            .1
            .split_once("pub async fn claim_artwork_source_repair")
            .expect("claim method after observation")
            .0;
        assert!(observation.contains("query_consistent_map"));
        assert!(!observation.contains(".execute("));
    }

    #[test]
    fn advertised_host_hides_the_port_and_names_loopback() {
        assert_eq!(advertised_host("plurx-a.lan:32402"), "plurx-a.lan");
        assert_eq!(advertised_host("192.0.2.40:32402"), "192.0.2.40");
        assert_eq!(advertised_host("127.0.0.1:32402"), "localhost");
        assert_eq!(advertised_host("[::1]:32402"), "localhost");
        assert_eq!(advertised_host("[2001:db8::40]:32402"), "2001:db8::40");
        assert_eq!(advertised_host("legacy-host"), "legacy-host");
        assert_eq!(advertised_host("legacy:host"), "legacy:host");
    }

    #[test]
    fn hostname_is_short_and_never_an_ip_address() {
        assert_eq!(
            short_hostname("living-room.example.net"),
            Some("living-room".to_owned())
        );
        assert_eq!(short_hostname("nuc4.local."), Some("nuc4".to_owned()));
        assert_eq!(short_hostname("192.0.2.40"), None);
        assert_eq!(membership_hostname("", "plurx-a.lan:32402"), "plurx-a");
        assert_eq!(membership_hostname("", "127.0.0.1:32402"), "unknown-host");
    }

    fn hostname_row(node_id: &str, hostname: &str, api_address: &str) -> NodeHostnameRow {
        NodeHostnameRow {
            node_id: node_id.to_owned(),
            hostname: hostname.to_owned(),
            api_address: api_address.to_owned(),
        }
    }

    #[test]
    fn the_roster_names_every_node_it_can_and_omits_the_ones_it_cannot() {
        let named = roster_hostnames(
            vec![
                hostname_row("node-a", "nuc3.lan", "192.168.4.7:32402"),
                hostname_row("node-b", "", "m6.lan:32402"),
                // No reported name, and an address that is a bare loopback IP:
                // nothing here names a machine.
                hostname_row("node-c", "", "127.0.0.1:32402"),
            ],
            "node-a",
            "nuc3",
        );
        assert_eq!(named.get("node-a").map(String::as_str), Some("nuc3"));
        assert_eq!(named.get("node-b").map(String::as_str), Some("m6"));
        // The point of omitting it: a caller holding "node-c" shows that id,
        // which at least identifies the machine, rather than a sentinel that
        // names every unnamed node identically.
        assert_eq!(named.get("node-c"), None);
        assert!(!named.values().any(|hostname| hostname == UNKNOWN_HOSTNAME));
    }

    #[test]
    fn the_local_node_is_named_from_memory_before_its_heartbeat_lands() {
        // The local row is written by the heartbeat, so for the first heartbeat
        // interval after start the table has no name for this node at all.
        let named = roster_hostnames(
            vec![hostname_row("node-b", "m6.lan", "192.168.4.14:32402")],
            "node-a",
            "nuc3",
        );
        assert_eq!(named.get("node-a").map(String::as_str), Some("nuc3"));
        assert_eq!(named.get("node-b").map(String::as_str), Some("m6"));
    }

    #[test]
    fn the_local_node_prefers_its_own_name_over_a_stale_replicated_row() {
        let named = roster_hostnames(
            vec![hostname_row("node-a", "old-name", "192.168.4.7:32402")],
            "node-a",
            "nuc3",
        );
        assert_eq!(named.get("node-a").map(String::as_str), Some("nuc3"));
    }

    #[test]
    fn a_local_node_that_cannot_name_itself_is_omitted_not_sentinelled() {
        // A container whose hostname is its own truncated id, on an address
        // that reverses to nothing. Publishing "unknown-host" here would put
        // that word in the operator's Node column.
        let named = roster_hostnames(
            vec![hostname_row("node-a", "9f2c1b0a4d5e", "127.0.0.1:32402")],
            "node-a",
            UNKNOWN_HOSTNAME,
        );
        assert_eq!(named.get("node-a"), None);
        assert!(named.is_empty());
    }

    #[test]
    fn the_hostname_read_stays_cheap_and_delegates_its_naming_rules() {
        let source = production_source();
        let accessor = source
            .split_once("pub async fn node_hostnames(")
            .expect("node_hostnames accessor")
            .1
            .split_once("\n    /// Resolve the directly observable")
            .expect("method after node_hostnames")
            .0;
        // The roster's own name table, and only the live rows in it.
        assert!(accessor.contains("LEFT JOIN cluster_node_hostnames"));
        assert!(accessor.contains("WHERE node.removed_at IS NULL"));
        // The naming rules live in one tested place. An accessor that filtered
        // or defaulted inline would leave `roster_hostnames` asserting nothing
        // about what the cluster actually publishes.
        assert!(accessor.contains("roster_hostnames("));
        assert!(!accessor.contains(UNKNOWN_HOSTNAME));
        assert!(!accessor.contains("membership_hostname("));
        // The activity page polls this every few seconds. `status()` pulls Raft
        // metrics, protocol status and five joins for the same names.
        assert!(!accessor.contains("metrics_db()"));
        assert!(!accessor.contains("protocol_status()"));
        assert!(!accessor.contains("self.status()"));
    }

    #[test]
    fn malformed_redeem_fields_are_rejected_before_any_cluster_lookup() {
        let valid = RedeemJoinRequest {
            token_digest: "a".repeat(64),
            raft_id: 4,
            node_id: "node-d".to_owned(),
            hostname: "node-d.example.net".to_owned(),
            raft_address: "node-d:32401".to_owned(),
            api_address: "node-d:32402".to_owned(),
            http_base: "http://node-d:32400".to_owned(),
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: AUTH_PROTOCOL_MIN,
            protocol_min: AUTH_PROTOCOL_MIN,
            protocol_max: AUTH_PROTOCOL_MAX,
        };
        assert_eq!(
            validate_redeem_join_request(&valid).expect("valid request"),
            Some("http://node-d:32400".to_owned())
        );
        for malformed in [
            RedeemJoinRequest {
                token_digest: "not-a-digest".to_owned(),
                ..valid.clone()
            },
            RedeemJoinRequest {
                node_id: "x".repeat(MAX_PEER_NODE_ID_BYTES + 1),
                ..valid.clone()
            },
            RedeemJoinRequest {
                raft_address: "node-d-without-a-port".to_owned(),
                ..valid.clone()
            },
            RedeemJoinRequest {
                api_address: format!("node-d:{}", "9".repeat(MAX_JOIN_SOCKET_ADDRESS_BYTES)),
                ..valid.clone()
            },
        ] {
            assert!(matches!(
                validate_redeem_join_request(&malformed),
                Err(MembershipError::InvalidToken)
            ));
        }
        assert!(matches!(
            validate_redeem_join_request(&RedeemJoinRequest {
                http_base: format!("http://node-d/{}", "x".repeat(MAX_JOIN_HTTP_ORIGIN_BYTES)),
                ..valid
            }),
            Err(MembershipError::InvalidHttpEndpoint)
        ));

        let source = production_source();
        let redeem = source
            .split_once("pub async fn redeem(")
            .expect("redeem method")
            .1
            .split_once("async fn upsert_hostname(")
            .expect("method after redeem")
            .0;
        let cheap = redeem
            .find("validate_redeem_join_request(request)")
            .expect("cheap validation");
        let possession = redeem
            .find("token_record(&request.token_digest)")
            .expect("token lookup");
        let protocol = redeem
            .find("active_protocol_range()")
            .expect("protocol range lookup");
        assert!(cheap < possession && possession < protocol);
    }

    #[test]
    fn docker_container_ids_are_not_machine_hostnames() {
        assert_eq!(short_hostname("1cb4bdb624dc"), None);
        assert_eq!(short_hostname(&"a".repeat(64)), None);
    }

    #[test]
    fn reverse_dns_supplies_a_short_name_for_an_ip_only_node() {
        let hostname =
            membership_hostname_with_lookup("unknown-host", "192.168.4.7:32402", |address| {
                assert_eq!(address, "192.168.4.7".parse::<IpAddr>().expect("ip"));
                Some("nuc3.home.arpa".to_owned())
            });

        assert_eq!(hostname, "nuc3");
    }

    #[test]
    fn reported_hostname_wins_without_a_reverse_lookup() {
        let hostname =
            membership_hostname_with_lookup("nuc4.example.net", "192.168.4.8:32402", |_| {
                panic!("reported hostnames must not trigger reverse DNS")
            });

        assert_eq!(hostname, "nuc4");
    }

    #[test]
    fn join_credentials_and_cluster_secrets_are_redacted_from_debug() {
        let payload = payload();
        let token = encode_join_token(&payload).expect("encode token");
        let digest = join_token_digest(&token);
        let issued = IssuedJoinToken {
            token: token.clone(),
            expires_at: payload.expires_at,
            raft_id: payload.raft_id,
        };
        let redeem = RedeemJoinRequest {
            token_digest: digest.clone(),
            raft_id: payload.raft_id,
            node_id: "node-b".to_owned(),
            hostname: "node-b.example.net".to_owned(),
            raft_address: "node-b:32401".to_owned(),
            api_address: "node-b:32402".to_owned(),
            http_base: "http://node-b:32400".to_owned(),
            schema_version: payload.schema_version,
            protocol_version: payload.protocol_version,
            protocol_min: AUTH_PROTOCOL_MIN,
            protocol_max: AUTH_PROTOCOL_MAX,
        };
        let finalize = FinalizeJoinRequest {
            token_digest: digest.clone(),
            raft_id: payload.raft_id,
            node_id: "node-b".to_owned(),
        };

        for rendered in [
            format!("{issued:?}"),
            format!("{redeem:?}"),
            format!("{finalize:?}"),
            format!("{payload:?}"),
        ] {
            assert!(rendered.contains("<redacted>"), "{rendered}");
            assert!(!rendered.contains(&token), "{rendered}");
            assert!(!rendered.contains(&digest), "{rendered}");
            assert!(!rendered.contains(&"r".repeat(64)), "{rendered}");
            assert!(!rendered.contains(&"a".repeat(64)), "{rendered}");
            assert!(!rendered.contains(&"c".repeat(64)), "{rendered}");
        }
    }

    #[test]
    fn exact_internal_authority_binds_method_path_and_raw_body() {
        let nonce = "123e4567-e89b-42d3-a456-426614174000";
        let message = internal_peer_auth_message(
            "node-a",
            "node-b",
            42,
            nonce,
            "POST",
            "/internal/v1/media/offers",
            b"{}",
        )
        .expect("valid exact route");
        for changed in [
            internal_peer_auth_message(
                "node-c",
                "node-b",
                42,
                nonce,
                "POST",
                "/internal/v1/media/offers",
                b"{}",
            ),
            internal_peer_auth_message(
                "node-a",
                "node-b",
                42,
                nonce,
                "GET",
                "/internal/v1/media/offers",
                b"{}",
            ),
            internal_peer_auth_message(
                "node-a",
                "node-b",
                42,
                nonce,
                "POST",
                "/internal/v1/media/snapshot",
                b"{}",
            ),
            internal_peer_auth_message(
                "node-a",
                "node-b",
                42,
                nonce,
                "POST",
                "/internal/v1/media/offers",
                b"{ }",
            ),
            internal_peer_auth_message(
                "node-a",
                "node-b",
                42,
                "123e4567-e89b-42d3-a456-426614174001",
                "POST",
                "/internal/v1/media/offers",
                b"{}",
            ),
        ] {
            assert_ne!(message, changed.expect("valid mutation"));
        }
        assert!(internal_peer_auth_message(
            "node-a",
            "node-b",
            42,
            nonce,
            "post",
            "/internal/v1/media/offers",
            b"{}"
        )
        .is_none());
        assert!(internal_peer_auth_message(
            "node-a",
            "node-b",
            42,
            nonce,
            "POST",
            "/internal/v1/media/offers?credential=secret",
            b"{}"
        )
        .is_none());
        assert!(internal_peer_auth_message(
            "node-a",
            "node-b",
            42,
            "NOT-A-CANONICAL-UUID",
            "POST",
            "/internal/v1/media/offers",
            b"{}"
        )
        .is_none());
    }
}
