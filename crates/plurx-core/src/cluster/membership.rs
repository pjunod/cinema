//! M3 cluster membership lifecycle.
//!
//! Membership is cluster infrastructure, not a second application store. The
//! Hiqlite client remains the one replicated write path; this coordinator owns
//! only the small amount of state needed to admit nodes, describe them without
//! exposing listener ports or secrets, and remove a voter safely.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[cfg(unix)]
use std::ffi::CStr;
use std::net::IpAddr;
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

use crate::cluster::coordination::removed_job_owner_key;
use crate::domain::{OfflinePackage, OfflineRemovalPlanEntry, OfflineRemovalReport};
use crate::store::{ArtworkRepairFence, Store, AUTH_PROTOCOL_VERSION, AUTH_SCHEMA_VERSION};

use super::migration::status::{ReplicationMonitor, ReplicationStatus};
use super::migration::ActivationMarker;
use super::ClusterIdentity;

const JOIN_TOKEN_PREFIX: &str = "plxjoin:v1";
const JOIN_TOKEN_AAD: &[u8] = b"plurx-cluster-join-v1";
const JOIN_TOKEN_VERSION: u32 = 1;
const MEMBERSHIP_SCHEMA_VERSION: i64 = 1;
const NODE_REACHABLE_WINDOW_MS: i64 = 30_000;
const ARTWORK_AUTH_WINDOW_MS: i64 = 60_000;
/// The vendored Raft configuration sends heartbeats every 500 ms and cannot
/// elect a successor before 1,500 ms. Requiring an acknowledgement inside two
/// heartbeats makes the old leader ineligible before a new term can exist.
const ARTWORK_LEADER_QUORUM_FRESH_MS: u64 = 1_000;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
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
const MAX_ACTIVITY_AUTH_CHECKS_PER_SECOND: u8 = 2;
const MAX_INTERNAL_AUTH_CHECKS_PER_SECOND: u8 = 128;
const MAX_INTERNAL_REPLAYS_PER_PEER: usize = 4_096;
const MAX_PEER_NODE_ID_BYTES: usize = 256;
const INTERNAL_AUTH_NONCE_BYTES: usize = 36;
const ED25519_SIGNATURE_HEX_BYTES: usize = 128;
const MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND: u8 = 4;

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
    #[error(
        "node removal remains pending after the membership change was rejected: {0}; finish upgrading every cluster node and retry this removal"
    )]
    RemovalPending(String),
    #[error("node was not found in current cluster membership")]
    NodeNotFound,
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
    /// The removal was refused because this node's offline work could not be
    /// resolved by the §6.7 rule. The payload is the operator-visible reason;
    /// lifting the blanket refusal must not turn removal into "always
    /// succeeds", so what stopped it has to be sayable.
    #[error("node owns offline work that could not be resolved: {0}")]
    OfflineWork(String),
    #[error("cluster membership operation failed: {0}")]
    Internal(String),
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
            Self::RemovalPending(_) => "membership_removal_pending",
            Self::NodeNotFound => "cluster_node_not_found",
            Self::LeaderRemoval => "cluster_leader_removal_refused",
            Self::SelfRemovalRequiresLeave => "self_removal_requires_leave",
            Self::LeaveNodeMismatch => "leave_node_mismatch",
            Self::LocalNodeNotActive => "local_node_not_active",
            Self::QuorumLoss => "removal_would_lose_quorum",
            Self::OfflineWork(_) => "node_owns_offline_work",
            Self::Internal(_) => "membership_internal",
        }
    }
}

impl From<hiqlite::Error> for MembershipError {
    fn from(error: hiqlite::Error) -> Self {
        Self::Internal(error.to_string())
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
    pub protocol_version: i64,
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
    pub role: NodeRole,
    pub is_leader: bool,
    pub reachable: bool,
    pub last_seen_at: i64,
    /// A durable removal fence still owns this node. The voter can remain in
    /// Raft membership after a rejected or indeterminate request, so expose
    /// the fence instead of rendering the node as fully operational.
    pub removal_pending: bool,
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

const CAPABILITY_READY_PREDICATE: &str = "NOT EXISTS (SELECT 1 FROM cluster_nodes AS active \
       WHERE active.removed_at IS NULL \
         AND NOT EXISTS (SELECT 1 FROM cluster_node_join_staging AS staged \
           WHERE staged.node_id = active.node_id) \
         AND NOT EXISTS (SELECT 1 FROM cluster_node_capabilities AS capability \
           WHERE capability.node_id = active.node_id \
             AND capability.capability = 'membership_removal_attempt_refs_v1' \
             AND capability.last_seen_at = active.last_seen_at))";

fn begin_removal_attempt_sql() -> String {
    format!(
        "INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) \
         SELECT $1, $2 WHERE {CAPABILITY_READY_PREDICATE}"
    )
}

fn rollback_removal_attempt_sql() -> String {
    format!(
        "DELETE FROM cluster_node_removal_attempts WHERE node_id = $1 AND attempt_id = $2 \
         AND {CAPABILITY_READY_PREDICATE}"
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
    store: Arc<dyn Store>,
    identity: ClusterIdentity,
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
    activity_key_lookup_admission: Mutex<ActivityAuthAdmission>,
    activation_marker: ActivationMarker,
    replication: ReplicationMonitor,
    /// First local observation of an older-term claim. `Instant` deliberately
    /// never crosses a process boundary: a successor waits the entire lease
    /// regardless of either host's wall clock.
    artwork_claim_observed_at: Mutex<BTreeMap<i64, (i64, i64, Instant)>>,
}

struct ActivityAuthAdmission {
    window_started: Instant,
    checks: u8,
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

    #[allow(clippy::too_many_arguments)]
    pub async fn replicated(
        client: Client,
        store: Arc<dyn Store>,
        identity: ClusterIdentity,
        local: ClusterPeer,
        bootstrap_http: String,
        artwork_http: String,
        secrets: JoinSecrets,
        activity_signing_key: ActivitySigningKey,
        activation_marker: ActivationMarker,
    ) -> Result<Self, MembershipError> {
        let replication = ReplicationMonitor::replicated(client.clone());
        let local_hostname = membership_hostname(
            system_short_hostname().as_deref().unwrap_or_default(),
            &local.api_address,
        );
        let manager = Self {
            inner: Some(Arc::new(ReplicatedMembership {
                client,
                store,
                identity,
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
                activity_key_lookup_admission: Mutex::new(ActivityAuthAdmission {
                    window_started: Instant::now(),
                    checks: 0,
                }),
                activation_marker,
                replication,
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
        for statement in MEMBERSHIP_SCHEMA {
            inner.client.execute(*statement, params!()).await?;
        }
        // One replicated SQLite transaction closes both upgrade directions:
        // fences written before this schema gain a durable legacy reference,
        // and the trigger rejects every later old-coordinator insert. No Raft
        // write can interleave between the backfill and trigger installation.
        inner
            .client
            .txn(vec![
                (BACKFILL_REMOVAL_ATTEMPT_REFS_SQL.to_owned(), params!()),
                (REQUIRE_REMOVAL_INTENT_SQL.to_owned(), params!()),
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
        self.publish_activity_signing_key().await?;
        self.refresh_activity_public_keys().await?;
        self.publish_http_url().await
    }

    pub async fn issue_token(&self, ttl: Duration) -> Result<IssuedJoinToken, MembershipError> {
        let inner = self.replicated_inner()?;
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
            let payload = JoinPayload {
                version: JOIN_TOKEN_VERSION,
                cluster_id: inner.identity.cluster_id.clone(),
                raft_id,
                expires_at,
                bootstrap_http: inner.bootstrap_http.clone(),
                bootstrap: bootstrap.clone(),
                secrets: JoinSecretPayload {
                    raft: inner.secrets.raft.clone(),
                    api: inner.secrets.api.clone(),
                    credential_key: inner.secrets.credential_key.clone(),
                },
                activation_marker: inner.activation_marker.clone(),
                schema_version: AUTH_SCHEMA_VERSION,
                protocol_version: AUTH_PROTOCOL_VERSION,
            };
            let token = encode_join_token(&payload)?;
            let token_hash = join_token_digest(&token);
            let inserted = inner
                .client
                .execute(
                    "INSERT INTO cluster_join_tokens \
                     (token_hash, raft_id, expires_at, state, node_id, created_at, redeemed_at) \
                     SELECT $1, $2, $3, 'issued', NULL, $4, NULL \
                     WHERE NOT EXISTS (\
                       SELECT 1 FROM cluster_join_tokens \
                       WHERE raft_id = $2 AND state IN ('issued', 'redeeming') AND expires_at > $4\
                     )",
                    params!(token_hash, raft_id as i64, expires_at, now),
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
        let inner = self.replicated_inner()?;
        if request.schema_version != AUTH_SCHEMA_VERSION
            || request.protocol_version != AUTH_PROTOCOL_VERSION
        {
            return Err(MembershipError::Incompatible);
        }
        if !is_join_token_digest(&request.token_digest) {
            return Err(MembershipError::InvalidToken);
        }
        let http_base = if request.http_base.is_empty() {
            None
        } else {
            Some(
                normalize_internal_http_base(&request.http_base)
                    .ok_or(MembershipError::InvalidHttpEndpoint)?,
            )
        };
        let now = unix_ms()?;
        let record = self.token_record(&request.token_digest).await?;
        if record.raft_id != request.raft_id as i64 {
            return Err(MembershipError::InvalidToken);
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
        let mut statements = Vec::new();
        let proof_statement_index = if let Some(http_base) = http_base.as_deref() {
            statements.push((
                "INSERT INTO cluster_node_http (node_id, public_http_url) \
                 SELECT $1, $2 WHERE EXISTS (\
                   SELECT 1 FROM cluster_join_tokens token \
                   WHERE token.token_hash = $3 AND token.raft_id = $4 \
                     AND ((token.state = 'issued' AND token.expires_at > $5) \
                       OR (token.state = 'redeeming' AND token.node_id = $1))) \
                 AND NOT EXISTS (SELECT 1 FROM cluster_node_http_claims claim \
                   WHERE claim.node_id = $1 AND claim.public_http_url != $2) \
                 AND NOT EXISTS (SELECT 1 FROM cluster_nodes WHERE node_id = $1) \
                 AND NOT EXISTS (\
                   SELECT 1 FROM cluster_node_http owner_http \
                   JOIN cluster_nodes owner_node ON owner_node.node_id = owner_http.node_id \
                   WHERE owner_http.public_http_url = $2 AND owner_http.node_id != $1 \
                     AND owner_node.removed_at IS NULL \
                     AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removing \
                       WHERE removing.node_id = owner_node.node_id)) \
                 ON CONFLICT(node_id) DO UPDATE SET public_http_url = excluded.public_http_url \
                 RETURNING node_id"
                    .to_owned(),
                params!(
                    request.node_id.as_str(),
                    http_base,
                    request.token_digest.as_str(),
                    request.raft_id as i64,
                    now
                ),
            ));
            if resume_legacy_partial {
                0
            } else {
                statements.push((
                    "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = $1 \
                     WHERE token_hash = $2 AND state = 'issued' AND expires_at > $3 \
                       AND NOT EXISTS (SELECT 1 FROM cluster_nodes WHERE node_id = $1) \
                     RETURNING node_id"
                        .to_owned(),
                    vec![
                        Param::StmtOutputNamed(0, "node_id".into()),
                        Param::Text(request.token_digest.clone()),
                        Param::Integer(now),
                    ],
                ));
                1
            }
        } else if resume_legacy_partial {
            statements.push((
                "UPDATE cluster_join_tokens SET node_id = node_id \
                 WHERE node_id = $1 AND token_hash = $2 AND raft_id = $3 \
                   AND state = 'redeeming' \
                   AND NOT EXISTS (SELECT 1 FROM cluster_nodes WHERE node_id = $1) \
                 RETURNING node_id"
                    .to_owned(),
                params!(
                    request.node_id.as_str(),
                    request.token_digest.as_str(),
                    request.raft_id as i64
                ),
            ));
            0
        } else {
            statements.push((
                "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = $1 \
                 WHERE token_hash = $2 AND state = 'issued' AND expires_at > $3 \
                   AND NOT EXISTS (SELECT 1 FROM cluster_nodes WHERE node_id = $1) \
                 RETURNING node_id"
                    .to_owned(),
                params!(request.node_id.as_str(), request.token_digest.as_str(), now),
            ));
            0
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
                 (node_id, raft_id, raft_address, api_address, last_seen_at, removed_at) \
                 VALUES ($1, $2, $3, $4, $5, NULL)"
                    .to_owned(),
                vec![
                    Param::StmtOutputNamed(proof_statement_index, "node_id".into()),
                    Param::Integer(request.raft_id as i64),
                    Param::Text(request.raft_address.clone()),
                    Param::Text(request.api_address.clone()),
                    Param::Integer(now),
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
        let transaction = inner.client.txn(statements).await;
        match transaction {
            Ok(results) => {
                results.into_iter().collect::<Result<Vec<_>, _>>()?;
            }
            Err(error) if error.to_string().contains("StmtIndex(") => {
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
        let inner = self.replicated_inner()?;
        if !is_join_token_digest(&request.token_digest) {
            return Err(MembershipError::InvalidToken);
        }
        let record = self.token_record(&request.token_digest).await?;
        if record.node_id.as_deref() != Some(&request.node_id)
            || record.raft_id != request.raft_id as i64
        {
            return Err(MembershipError::ReservedToken);
        }
        if record.state == "redeemed" {
            return Ok(());
        }
        let metrics = inner.client.metrics_db().await?;
        if !metrics
            .membership_config
            .voter_ids()
            .any(|id| id == request.raft_id)
        {
            return Err(MembershipError::Internal(
                "joining node has not committed voter membership".to_owned(),
            ));
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
                "SELECT raft_id, expires_at, state, node_id FROM cluster_join_tokens \
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
        let now = unix_ms()?;
        inner
            .client
            .txn(vec![
                (
                    "INSERT INTO cluster_node_heartbeat_intents (node_id, last_seen_at) \
                     VALUES ($1, $2) ON CONFLICT(node_id) DO UPDATE SET \
                       last_seen_at = excluded.last_seen_at"
                        .to_owned(),
                    params!(inner.identity.node_id.as_str(), now),
                ),
                (
                    "INSERT INTO cluster_nodes \
                     (node_id, raft_id, raft_address, api_address, last_seen_at, removed_at) \
                     VALUES ($1, $2, $3, $4, $5, NULL) \
                     ON CONFLICT(node_id) DO UPDATE SET last_seen_at = excluded.last_seen_at, \
                       removed_at = NULL WHERE cluster_nodes.removed_at IS NULL"
                        .to_owned(),
                    params!(
                        inner.identity.node_id.as_str(),
                        inner.identity.raft_id as i64,
                        inner.local.raft_address.as_str(),
                        inner.local.api_address.as_str(),
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
                    "DELETE FROM cluster_node_join_staging WHERE node_id = $1".to_owned(),
                    params!(inner.identity.node_id.as_str()),
                ),
                (
                    "DELETE FROM cluster_node_heartbeat_intents WHERE node_id = $1 \
                     AND last_seen_at = $2"
                        .to_owned(),
                    params!(inner.identity.node_id.as_str(), now),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
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

    pub async fn local_node_is_committed_voter(&self) -> Result<bool, MembershipError> {
        let inner = self.replicated_inner()?;
        let metrics = inner.client.metrics_db().await?;
        Ok(metrics
            .membership_config
            .voter_ids()
            .any(|raft_id| raft_id == inner.identity.raft_id))
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

    /// Public HTTP bases for currently reachable peers, used only to recover
    /// node-local materialized bytes such as artwork. The ordinary membership
    /// status deliberately continues to omit addresses.
    pub async fn reachable_peer_http_urls(&self) -> Result<Vec<String>, MembershipError> {
        let Some(inner) = self.inner.as_ref() else {
            return Ok(Vec::new());
        };
        let reachable_after = unix_ms()?.saturating_sub(NODE_REACHABLE_WINDOW_MS);
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
        let reachable_after = now.saturating_sub(NODE_REACHABLE_WINDOW_MS);
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
                reachable: now.saturating_sub(row.last_seen_at) <= NODE_REACHABLE_WINDOW_MS,
            })
            .collect())
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
        let reachable_after = now.saturating_sub(NODE_REACHABLE_WINDOW_MS);
        let rows = inner
            .client
            .query_consistent_map::<ActivityAuthNodeRow, _>(
                "SELECT node.raft_id \
                 FROM cluster_nodes node \
                 WHERE node.node_id = $1 AND node.removed_at IS NULL \
                   AND node.last_seen_at >= $2 \
                   AND NOT EXISTS (SELECT 1 FROM cluster_node_removals removal \
                     WHERE removal.node_id = node.node_id)",
                params!(node_id, reachable_after),
            )
            .await?;
        Ok(rows.len() == 1
            && metrics
                .membership_config
                .voter_ids()
                .any(|raft_id| raft_id == rows[0].raft_id))
    }

    pub async fn heartbeat_loop(self) {
        if self.inner.is_none() {
            return;
        }
        loop {
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
            match self.heartbeat().await {
                Err(error) => {
                    tracing::warn!(code = error.code(), "cluster node heartbeat failed");
                }
                Ok(()) => {
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
                "SELECT n.node_id, n.raft_id, n.api_address, n.last_seen_at, \
                        COALESCE(h.hostname, '') AS hostname, \
                        EXISTS (SELECT 1 FROM cluster_node_removals AS removal \
                          WHERE removal.node_id = n.node_id) AS removal_pending \
                 FROM cluster_nodes n \
                 LEFT JOIN cluster_node_hostnames h ON h.node_id = n.node_id \
                 WHERE n.removed_at IS NULL ORDER BY n.raft_id",
                params!(),
            )
            .await?;
        let nodes = rows
            .into_iter()
            .filter(|row| members.contains(&(row.raft_id as u64)))
            .map(|row| ClusterNodeRecord {
                node_id: row.node_id,
                hostname: membership_hostname(&row.hostname, &row.api_address),
                advertised_host: advertised_host(&row.api_address),
                raft_id: row.raft_id as u64,
                role: if voters.contains(&(row.raft_id as u64)) {
                    NodeRole::Voter
                } else {
                    NodeRole::Learner
                },
                is_leader: metrics.current_leader == Some(row.raft_id as u64),
                reachable: now.saturating_sub(row.last_seen_at) <= NODE_REACHABLE_WINDOW_MS,
                last_seen_at: row.last_seen_at,
                removal_pending: row.removal_pending,
            })
            .collect::<Vec<_>>();
        let availability = match voters.len() {
            0 | 1 => ClusterAvailability::SingleNode,
            2 => ClusterAvailability::DegradedReconfiguration,
            _ => ClusterAvailability::HighAvailability,
        };
        Ok(MembershipStatus {
            local_node_id: inner.identity.node_id.clone(),
            availability,
            nodes,
            replication: inner.replication.status().await,
        })
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
        let removal_attempt = self.begin_node_removal(node_id).await?;
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
                    MembershipChangeOutcome::Indeterminate => {
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
        let removal_attempt = self.begin_node_removal(&node_id).await?;
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
                    MembershipChangeOutcome::Indeterminate => {
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

    async fn begin_node_removal(&self, node_id: &str) -> Result<String, MembershipError> {
        let inner = self.replicated_inner()?;
        let now = unix_ms()?;
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let owner_fence_key = removed_job_owner_key(node_id);
        let results = inner
            .client
            .txn(vec![
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
                (
                    begin_removal_attempt_sql(),
                    params!(node_id, attempt_id.as_str()),
                ),
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
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        if results.get(1).copied() != Some(1) {
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
                    "SELECT CASE WHEN {CAPABILITY_READY_PREDICATE} \
                     THEN 1 ELSE 0 END AS count"
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

pub fn decode_join_token(token: &str) -> Result<JoinPayload, MembershipError> {
    let mut parts = token.split(':');
    if parts.next() != Some("plxjoin") || parts.next() != Some("v1") || parts.clone().count() != 2 {
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

fn encode_join_token(payload: &JoinPayload) -> Result<String, MembershipError> {
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
                aad: JOIN_TOKEN_AAD,
            },
        )
        .map_err(|_| MembershipError::Internal("encrypting join token".to_owned()))?;
    let mut encrypted = nonce.to_vec();
    encrypted.extend(ciphertext);
    Ok(format!(
        "{JOIN_TOKEN_PREFIX}:{}:{}",
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
}

impl From<&mut Row<'_>> for JoinTokenRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            raft_id: row.get("raft_id"),
            expires_at: row.get("expires_at"),
            state: row.get("state"),
            node_id: row.get("node_id"),
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

struct MembershipNodeRow {
    node_id: String,
    raft_id: i64,
    api_address: String,
    hostname: String,
    last_seen_at: i64,
    removal_pending: bool,
}

struct ActivityPeerRow {
    node_id: String,
    raft_id: u64,
    last_seen_at: i64,
    http_base: Option<String>,
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
}

impl From<&mut Row<'_>> for ActivityAuthNodeRow {
    fn from(row: &mut Row<'_>) -> Self {
        let raft_id: i64 = row.get("raft_id");
        Self {
            raft_id: u64::try_from(raft_id).unwrap_or_default(),
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
}

impl RemoteMembershipMetrics {
    fn uniform_voters(mut self) -> Option<BTreeSet<u64>> {
        (self.membership_config.membership.configs.len() == 1)
            .then(|| self.membership_config.membership.configs.remove(0))
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

impl From<&mut Row<'_>> for MembershipNodeRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            node_id: row.get("node_id"),
            raft_id: row.get("raft_id"),
            api_address: row.get("api_address"),
            hostname: row.get("hostname"),
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
        || label.eq_ignore_ascii_case("unknown-host")
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
        .unwrap_or_else(|| "unknown-host".to_owned())
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
                 CREATE TABLE cluster_join_tokens (node_id TEXT, state TEXT); \
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER); \
                 CREATE TABLE job_leases (owner_node_id TEXT, expires_at_ms INTEGER, \
                   revision INTEGER, updated_at_ms INTEGER); \
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
                    &begin_removal_attempt_sql(),
                    rusqlite::params!["blocked", "blocked-attempt"],
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
            },
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: AUTH_PROTOCOL_VERSION,
        }
    }

    #[test]
    fn join_token_round_trips_without_plaintext_payload() {
        let expected = payload();
        let token = encode_join_token(&expected).expect("encode token");
        assert!(token.starts_with(JOIN_TOKEN_PREFIX));
        assert!(!token.contains("cluster-a"));
        assert!(!token.contains(&"r".repeat(64)));
        assert_eq!(decode_join_token(&token).expect("decode token"), expected);
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
            MembershipError::QuorumLoss.code(),
            "removal_would_lose_quorum"
        );
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
