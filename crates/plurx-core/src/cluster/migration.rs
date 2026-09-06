//! Crash-safe preparation for the one-time SQLite-to-Hiqlite import.
//!
//! The coordinator keeps SQLite authoritative until a fully imported one-voter
//! target has a durable completion marker. It then stops that voter, renames
//! the target atomically, fsyncs the data directory, and reopens the active
//! target before the daemon may start producers or bind HTTP.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(feature = "hiqlite-store")]
use std::borrow::Cow;
#[cfg(feature = "hiqlite-store")]
use std::collections::BTreeSet;
#[cfg(feature = "hiqlite-store")]
use std::io::Seek;
#[cfg(feature = "hiqlite-store")]
use std::net::{IpAddr, SocketAddr};
#[cfg(feature = "hiqlite-store")]
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::store::SQLITE_SCHEMA_VERSION;

#[cfg(feature = "hiqlite-store")]
use crate::config::{Config, DEFAULT_INSTALL_SNAPSHOT_TIMEOUT_SECS};
#[cfg(feature = "hiqlite-store")]
use crate::secrets::{self, CredentialKey, SealedRowCensus};
#[cfg(feature = "hiqlite-store")]
use crate::store::{
    CatalogueReader, HiqliteAuthStore, SettingsStore, SqliteImportReport, SqliteImportTableDigest,
    SqliteStore, Store, TraktStore, AUTH_SCHEMA_MIGRATION_SOURCE, AUTH_SCHEMA_VERSION,
};
#[cfg(feature = "hiqlite-store")]
use hiqlite::tls::ServerTlsConfig;
#[cfg(feature = "hiqlite-store")]
use hiqlite::{Client, Node, NodeConfig};
#[cfg(feature = "hiqlite-store")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "hiqlite-store")]
use super::membership::{
    decode_join_token, join_token_digest, local_membership_version,
    local_membership_version_matches_role, ActivitySigningKey, ClusterPeer, ClusterRole,
    FinalizeJoinRequest, JoinSecrets, JoinToken, LocalMembership, MembershipManager,
    RedeemJoinRequest,
};

pub const SQLITE_FILENAME: &str = "plurx.db";
pub const MIGRATION_DIRNAME: &str = "migration";
pub const HIQLITE_INCOMING_DIRNAME: &str = "hiqlite.incoming";
pub const HIQLITE_ACTIVE_DIRNAME: &str = "hiqlite";
pub const ACTIVATION_MARKER_FILENAME: &str = "activation.json";
/// Breadcrumb proving this data directory already handed authority to Hiqlite.
///
/// It lives beside `plurx.db` rather than inside the target, because its whole
/// job is to survive the target's disappearance.
pub const ACTIVATED_SOURCE_FILENAME: &str = "hiqlite-activated.json";
#[cfg(feature = "hiqlite-store")]
const ACTIVATION_ATTEMPT_FILENAME: &str = "hiqlite-activation.in-progress";
#[cfg(feature = "hiqlite-store")]
const RAFT_SECRET_FILENAME: &str = "secret_raft";
#[cfg(feature = "hiqlite-store")]
const API_SECRET_FILENAME: &str = "secret_api";
#[cfg(feature = "hiqlite-store")]
const ACTIVITY_SIGNING_KEY_FILENAME: &str = "activity_http_signing_key";
#[cfg(feature = "hiqlite-store")]
const HIQLITE_DATABASE_FILENAME: &str = "plurx.db";
#[cfg(feature = "hiqlite-store")]
const DAEMON_LOCK_FILENAME: &str = ".plurxd.lock";
#[cfg(feature = "hiqlite-store")]
const LOCAL_MEMBERSHIP_FILENAME: &str = "membership.json";
#[cfg(feature = "hiqlite-store")]
const HIQLITE_READDRESS_BACKUP_DIRNAME: &str = "hiqlite.before-readdress";
#[cfg(feature = "hiqlite-store")]
const HIQLITE_READDRESS_MARKER_FILENAME: &str = "hiqlite-readdress.json";
#[cfg(feature = "hiqlite-store")]
const HIQLITE_HEALTH_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(feature = "hiqlite-store")]
const MEMBERSHIP_ADMISSION_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(feature = "hiqlite-store")]
const SNAPSHOT_CATCHUP_GRACE: Duration = Duration::from_secs(45);
// OpenRaft gives an AppendEntries RPC one heartbeat interval. Once a leader is
// running this Hiqlite transport it lets that RPC use the whole hard deadline,
// and this 800 ms window admits the observed 600-700 ms durable crash-recovery
// responses. OpenRaft's idle heartbeat poll is at most 3/2 of this value
// (1,200 ms), still below the previous release's 1,500 ms election floor, so
// cleanly stopped voters remain safe during a rolling update or rollback. A
// crash-recovering majority needs a coordinated update: an old leader still
// owns the old 375 ms sender deadline and cannot be fixed by its new follower.
#[cfg(feature = "hiqlite-store")]
const HIQLITE_HEARTBEAT_INTERVAL_MS: u64 = 800;
#[cfg(feature = "hiqlite-store")]
const HIQLITE_ELECTION_TIMEOUT_MIN_MS: u64 = 2_400;
#[cfg(feature = "hiqlite-store")]
const HIQLITE_ELECTION_TIMEOUT_MAX_MS: u64 = 4_000;
/// Accepted end-to-end window for an exact-state write to survive leader loss.
#[cfg(all(feature = "hiqlite-store", test))]
pub(crate) const REPLICATED_LEADER_RECOVERY_BUDGET: Duration = Duration::from_secs(16);
/// Hiqlite Raft WAL segment size used by every plurx voter.
///
/// Hiqlite 0.14 accepts one serialized Raft entry up to `wal_size - 34` bytes:
/// its WAL writer subtracts the fixed segment metadata before checking each
/// entry independently. The 16 MiB segment therefore leaves 16,777,182 usable
/// bytes. OpenRaft's configured `max_payload_entries = 128` limits the number
/// of entries in one append request, not the size of an individual entry, and
/// Hiqlite carries that request in a WebSocket binary frame whose length field
/// supports this size without a smaller application cap.
///
/// Import transactions deliberately stay far below the usable entry capacity
/// at the replication-time ceiling measured before this headroom increase.
/// Keep all voters and import bounds tied to this constant so a retune cannot
/// make the contract exercise a different limit than production.
#[cfg(feature = "hiqlite-store")]
pub const HIQLITE_WAL_SIZE_BYTES: u32 = 16 * 1024 * 1024;
/// Bytes Hiqlite WAL 0.14 reserves before one serialized log entry.
#[cfg(feature = "hiqlite-store")]
const HIQLITE_WAL_SEGMENT_RESERVED_BYTES: usize = 34;
/// Largest serialized Raft entry accepted by the configured production WAL.
#[cfg(feature = "hiqlite-store")]
pub const HIQLITE_WAL_USABLE_PAYLOAD_BYTES: usize =
    HIQLITE_WAL_SIZE_BYTES as usize - HIQLITE_WAL_SEGMENT_RESERVED_BYTES;
#[cfg(feature = "hiqlite-store")]
const ACTIVATION_MARKER_VERSION: u32 = 1;
#[cfg(feature = "hiqlite-store")]
const ACTIVATION_FAILPOINT_ENV: &str = "PLURX_CLUSTER_ACTIVATION_FAILPOINT";
#[cfg(feature = "hiqlite-store")]
const ACTIVATION_CRASH_EXIT: i32 = 86;
/// How many source backups `migration/` keeps, newest first.
const MIGRATION_BACKUP_RETENTION: usize = 3;

/// Immutable source material for the row-import and parity phases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedSqliteImport {
    pub source_path: PathBuf,
    pub backup_path: PathBuf,
    pub backup_sha256: String,
    pub schema_version: i64,
    pub cluster_id: String,
}

/// The durable proof that the incoming target passed the complete import gate.
///
/// This file is fsynced before the directory becomes active. Startup never
/// treats the presence of Hiqlite files alone as activation: a missing,
/// malformed, or identity-mismatched marker is an ambiguous target and fails
/// closed instead of silently falling back to stale SQLite state.
#[cfg(feature = "hiqlite-store")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationMarker {
    pub marker_version: u32,
    pub cluster_id: String,
    pub source_backup_sha256: String,
    pub source_schema_version: i64,
    pub replicated_schema_version: i64,
    pub imported_rows: u64,
    pub table_hashes: Vec<SqliteImportTableDigest>,
    /// The role this particular data directory was admitted with.
    ///
    /// Old one-voter markers omit this field and are therefore the only
    /// markers for which a missing `membership.json` can still be upgraded as
    /// a voter. Every joined node writes it before the activation marker
    /// becomes authoritative, so losing the membership record can never turn
    /// a learner into a campaigning process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_role: Option<ClusterRole>,
}

/// Crash-recovery record for the one-voter loopback-to-advertised transition.
///
/// Hiqlite 0.14 cannot replace a voter's address in-place. Before the first
/// peer is admitted, plurx therefore snapshots the state machine and rebuilds
/// only the single-node Raft metadata. This record makes the two directory
/// renames recoverable without ever falling back to the stale legacy source.
#[cfg(feature = "hiqlite-store")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ReaddressRecord {
    version: u32,
    membership: LocalMembership,
}

/// Record that this data directory already activated, kept outside the target.
///
/// The retained `plurx.db` is a rollback source, not a current one, and nothing
/// else can tell the two apart: a data directory that lost `hiqlite/` looks
/// exactly like one that never activated. Without this breadcrumb the next boot
/// would re-import the stale source and silently discard every write since
/// activation. It is written after the rename that made the target authoritative
/// and re-asserted on every later boot that opens one.
#[cfg(feature = "hiqlite-store")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivatedSourceRecord {
    pub cluster_id: String,
    pub source_backup_sha256: String,
}

/// Which durable backend the daemon selected for this boot.
#[cfg(feature = "hiqlite-store")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectedBackend {
    Replicated,
    /// One recovery boot after an interrupted activation. The attempt marker
    /// is consumed before this value is returned, so a later restart may retry.
    SqliteRecovery,
}

/// Store selection returned before daemon producers and listeners are built.
#[cfg(feature = "hiqlite-store")]
pub struct SelectedStore {
    pub store: Arc<dyn Store>,
    pub identity: super::ClusterIdentity,
    /// Node-local key used only by outbound credential consumers.
    pub credential_key: Arc<CredentialKey>,
    pub backend: SelectedBackend,
    membership: MembershipManager,
    replication: status::ReplicationMonitor,
    catalogue: CatalogueReader,
    local_client: Option<Client>,
    _daemon_lock: File,
}

#[cfg(feature = "hiqlite-store")]
impl SelectedStore {
    /// Read-only watch-state replication projection for the server API.
    #[must_use]
    pub fn replication_monitor(&self) -> status::ReplicationMonitor {
        self.replication.clone()
    }

    /// Named consistency boundary for eligible catalogue requests.
    #[must_use]
    pub fn catalogue_reader(&self) -> CatalogueReader {
        self.catalogue.clone()
    }

    /// Membership lifecycle and privacy-safe node health for the daemon API.
    #[must_use]
    pub fn membership_manager(&self) -> MembershipManager {
        self.membership.clone()
    }

    /// Finish daemon shutdown before the Tokio runtime tears down the voter.
    ///
    /// Hiqlite 0.14 drains Raft, the WAL, and the SQL writer before it reaches
    /// a known TLS-listener notification defect. `shutdown_voter` contains that
    /// final panic only after matching its exact message; it never substitutes
    /// process exit for the durable-writer drain.
    pub async fn shutdown(&self) -> Result<(), StoreError> {
        if let Some(client) = &self.local_client {
            shutdown_voter(client, true).await?;
        }
        Ok(())
    }
}

#[cfg(feature = "hiqlite-store")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivationFailpoint {
    Quiescence,
    Incoming,
    Marker,
    Rename,
}

#[cfg(feature = "hiqlite-store")]
impl ActivationFailpoint {
    fn configured() -> Result<Option<Self>, StoreError> {
        let Some(value) = std::env::var(ACTIVATION_FAILPOINT_ENV)
            .ok()
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let point = match value.as_str() {
            "after-quiescence" => Self::Quiescence,
            "after-incoming" => Self::Incoming,
            "after-marker" => Self::Marker,
            "after-rename" => Self::Rename,
            _ => {
                return Err(StoreError::Migration(format!(
                    "invalid {ACTIVATION_FAILPOINT_ENV} value {value:?}; expected one of \
                     after-quiescence, after-incoming, after-marker, after-rename"
                )));
            }
        };
        Ok(Some(point))
    }

    fn crash_if(configured: Option<Self>, point: Self) {
        if configured == Some(point) {
            eprintln!(
                "injected cluster activation crash at {point:?}; SQLite and its migration \
                 backup remain unchanged; rollback command: plurxd run"
            );
            std::process::exit(ACTIVATION_CRASH_EXIT);
        }
    }
}

/// Select the daemon's one-voter replicated store before any producer or HTTP
/// listener exists.
///
/// A prior interrupted attempt consumes exactly one SQLite recovery boot: its
/// incoming target is removed, the attempt marker is fsynced away, and the
/// unchanged legacy store is returned. A completed atomic target always wins.
#[cfg(feature = "hiqlite-store")]
pub async fn select_daemon_store(config: &Config) -> Result<SelectedStore, StoreError> {
    install_default_crypto_provider();
    std::fs::create_dir_all(&config.storage.data_dir)
        .map_err(|error| migration_io("creating", &config.storage.data_dir, error))?;
    let daemon_lock = acquire_daemon_lock(&config.storage.data_dir).await?;
    recover_interrupted_readdress(&config.storage.data_dir)?;

    let active = config.storage.data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    if path_exists(&active)? {
        let pending_join = !config.cluster.join_token_file.as_os_str().is_empty()
            && path_exists(&config.cluster.join_token_file)?
            && !path_exists(&active.join(ACTIVATION_MARKER_FILENAME))?;
        if pending_join {
            return join_fresh_store(config, daemon_lock).await;
        }
        readdress_single_voter_if_needed(config)?;
        let selected = open_active_store(config, daemon_lock).await?;
        finalize_pending_join_best_effort(config, &selected).await;
        // A crash immediately after rename may expose the target before its
        // parent-directory entry is durable. Observing it on recovery lets us
        // finish that durability boundary before clearing attempt artifacts.
        sync_directory(&config.storage.data_dir)?;
        remove_abandoned_incoming(&config.storage.data_dir)?;
        remove_activation_attempt(&config.storage.data_dir)?;
        finish_readdress(&config.storage.data_dir)?;
        return Ok(selected);
    }

    // No active target, but this directory has already handed authority over.
    // Re-importing the retained source here would look like a clean first boot
    // and silently discard everything written since activation, so refuse
    // before any cleanup, attempt marker, or import can start.
    if let Some(record) = read_activated_source_record(&config.storage.data_dir)? {
        return Err(StoreError::Migration(format!(
            "{} was already activated as cluster {} but {} is missing: refusing to \
             re-import {}, which is the pre-activation rollback source and would \
             discard every change since. Restore the replicated target from a copy, \
             or delete {} to deliberately accept that rollback and its data loss",
            config.storage.data_dir.display(),
            record.cluster_id,
            active.display(),
            config.storage.data_dir.join(SQLITE_FILENAME).display(),
            config
                .storage
                .data_dir
                .join(ACTIVATED_SOURCE_FILENAME)
                .display(),
        )));
    }

    if !config.cluster.join_token_file.as_os_str().is_empty() {
        return join_fresh_store(config, daemon_lock).await;
    }

    ensure_sqlite_source(&config.storage.data_dir)?;
    // Future-schema refusal must precede cleanup or attempt-marker writes.
    inspect_sqlite_source(&config.storage.data_dir.join(SQLITE_FILENAME))?;
    // The injection surface belongs only to a pending activation. Reading it
    // before the active-target return let a stale or misspelled environment
    // value take an already-activated server offline even though no failpoint
    // could fire on that path.
    let failpoint = ActivationFailpoint::configured()?;

    let incoming = config.storage.data_dir.join(HIQLITE_INCOMING_DIRNAME);
    let interrupted =
        path_exists(&activation_attempt_path(&config.storage.data_dir))? || path_exists(&incoming)?;
    if interrupted {
        remove_abandoned_incoming(&config.storage.data_dir)?;
        remove_activation_attempt(&config.storage.data_dir)?;
        let legacy = super::open_store(config).await?;
        let catalogue = CatalogueReader::authority(Arc::clone(&legacy.store));
        return Ok(SelectedStore {
            store: legacy.store,
            identity: legacy.identity,
            credential_key: legacy.credential_key,
            backend: SelectedBackend::SqliteRecovery,
            membership: MembershipManager::unavailable(),
            replication: status::ReplicationMonitor::sqlite(),
            catalogue,
            local_client: None,
            _daemon_lock: daemon_lock,
        });
    }

    // Reuse the ordinary SQLite startup upgrade before the immutable backup is
    // published. This is the one permitted source mutation: a pre-encryption
    // Trakt row must be sealed under the node-local key before the importer can
    // audit it, and no cleartext application row may ever be submitted to Raft.
    // Opening here is still quiescent and happens after the future-schema
    // refusal above. Drop the source connection before the online backup.
    let legacy = super::open_store(config).await?;
    let identity = legacy.identity;
    let credential_key = legacy.credential_key;
    drop(legacy.store);

    match activate_fresh_store(config, failpoint, daemon_lock, identity, credential_key).await {
        Ok(store) => Ok(store),
        Err(error) => Err(activation_failure(&config.storage.data_dir, error)),
    }
}

#[cfg(feature = "hiqlite-store")]
async fn join_fresh_store(config: &Config, daemon_lock: File) -> Result<SelectedStore, StoreError> {
    let source = config.storage.data_dir.join(SQLITE_FILENAME);
    if path_exists(&source)? {
        return Err(StoreError::Migration(format!(
            "refusing to join with existing local database {}; a cluster join consumes a fresh \
             data directory and never overwrites an installation's library",
            source.display()
        )));
    }
    let token = read_join_token_file(&config.cluster.join_token_file)?;
    let payload = decode_join_token(&token)
        .map_err(|error| StoreError::Migration(format!("{}: {}", error.code(), error)))?;
    // The coordinator owns the expiry verdict. Locally, an unused expired
    // token and an interrupted join already reserved to this node have the
    // same payload; only the replicated token record can distinguish them.
    // Rechecking the embedded timestamp here would strand an identity-bound
    // voter that failed after redemption and restarted after the token TTL.
    // The token names the protocols the cluster is using; this binary
    // implements a range. Refuse early when the two do not overlap at all —
    // but this is only the cheap local check. `preflight_voter` below is
    // authoritative, because the cluster's *active* range can have been
    // narrowed after this token was minted and only the live `cluster_meta`
    // row says so.
    //
    // Every refusal in this block runs before a single secret reaches the
    // disk. That ordering is the contract: a joiner that cannot participate
    // must leave nothing behind that a later boot could mistake for a staged
    // join, and must never hold this cluster's credentials.
    let (token_min, token_max) = payload.declared_protocol_range();
    if payload.schema_version() != AUTH_SCHEMA_VERSION
        || token_min < crate::store::AUTH_PROTOCOL_MIN
        || token_max > crate::store::AUTH_PROTOCOL_MAX
    {
        return Err(StoreError::Migration(format!(
            "join_incompatible: this join token declares schema {} and protocol {token_min}..\
             ={token_max}, but this binary implements schema {AUTH_SCHEMA_VERSION} and protocol \
             {}..={}; install a matching build on this node",
            payload.schema_version(),
            crate::store::AUTH_PROTOCOL_MIN,
            crate::store::AUTH_PROTOCOL_MAX,
        )));
    }
    let role = payload.role();
    let remote = Client::remote(
        payload
            .bootstrap()
            .iter()
            .map(|peer| peer.api_address.clone())
            .collect(),
        true,
        true,
        payload.secrets().api.clone(),
        false,
        None,
    )
    .await
    .map_err(|error| {
        StoreError::Database(format!("connecting to cluster for join preflight: {error}"))
    })?;
    HiqliteAuthStore::preflight_role(
        &remote,
        crate::store::ClusterCompatibility::CURRENT,
        role.is_learner(),
    )
    .await?;

    let existing_membership = read_local_membership(&config.storage.data_dir)?;
    let identity = match &existing_membership {
        Some(membership) => {
            if membership.cluster_id != payload.cluster_id()
                || membership.raft_id != payload.raft_id()
            {
                return Err(StoreError::Identity(
                    "join token does not match the interrupted local membership".to_owned(),
                ));
            }
            if membership.role != role {
                return Err(StoreError::Identity(format!(
                    "join token admits this node as a {} but the interrupted local join is a {}; \
                     a role is not something a retry may change",
                    role.as_str(),
                    membership.role.as_str()
                )));
            }
            super::ClusterIdentity {
                cluster_id: membership.cluster_id.clone(),
                node_id: membership.node_id.clone(),
                raft_id: membership.raft_id,
            }
        }
        None => super::initialize_join_identity(
            &config.storage.data_dir,
            payload.cluster_id(),
            payload.raft_id(),
        )?,
    };
    let local = configured_local_peer(config, payload.raft_id())?;
    let token_digest = join_token_digest(&token);
    let membership = LocalMembership {
        version: local_membership_version(role),
        cluster_id: payload.cluster_id().to_owned(),
        node_id: identity.node_id.clone(),
        raft_id: payload.raft_id(),
        local: local.clone(),
        bootstrap: payload.bootstrap().to_vec(),
        join_token_digest: Some(token_digest.clone()),
        role,
    };
    redeem_remote_join(
        &payload,
        RedeemJoinRequest {
            token_digest,
            raft_id: payload.raft_id(),
            node_id: identity.node_id.clone(),
            hostname: super::membership::system_short_hostname().unwrap_or_default(),
            raft_address: local.raft_address.clone(),
            api_address: local.api_address.clone(),
            http_base: configured_artwork_url(config)?,
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
            protocol_min: crate::store::AUTH_PROTOCOL_MIN,
            protocol_max: crate::store::AUTH_PROTOCOL_MAX,
            live_tv_v1: true,
        },
    )
    .await?;

    persist_join_secret(
        &config.storage.data_dir.join(RAFT_SECRET_FILENAME),
        &payload.secrets().raft,
    )?;
    persist_join_secret(
        &config.storage.data_dir.join(API_SECRET_FILENAME),
        &payload.secrets().api,
    )?;
    persist_join_secret(
        &config.cluster.credential_key_path(&config.storage.data_dir),
        &payload.secrets().credential_key,
    )?;
    let secrets = read_existing_secrets(&config.storage.data_dir)?;
    // A verified activation marker, not the directory name, is the commit bit.
    // Hiqlite 0.14 cannot stop and rebind fully-TLS listeners in one process,
    // so the joiner starts at its final path. A crash before the marker leaves
    // the token and node id in place; the next boot re-enters this path, redeems
    // idempotently for the same node, and resumes the partial voter.
    let active = config.storage.data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    std::fs::create_dir_all(&active).map_err(|error| migration_io("creating", &active, error))?;
    sync_directory(&config.storage.data_dir)?;
    let (client, _) = start_voter(
        config,
        &active,
        &secrets,
        &identity,
        Some(&membership),
        true,
        false,
    )
    .await?;
    let store = HiqliteAuthStore::open(client.clone(), &active.join("telemetry.db")).await?;
    verify_store_identity(&store, payload.cluster_id()).await?;
    let mut activation_marker = payload.activation_marker().clone();
    activation_marker.admitted_role = Some(role);
    publish_join_activation(
        &config.storage.data_dir,
        &active,
        &activation_marker,
        &membership,
        JoinActivationFailpoint::None,
    )?;
    ensure_activated_source_record(&config.storage.data_dir, &activation_marker)?;

    // Keep the caught-up voter alive. Fully-TLS Hiqlite listeners have no
    // graceful-shutdown handle, and no stop/rebind boundary is needed because
    // every durable file already lives at the final path.
    let credential_key = open_active_credential_key(config, &store).await?;
    let concrete_store = Arc::new(store);
    let store: Arc<dyn Store> = concrete_store.clone();
    let replication = status::ReplicationMonitor::replicated(client.clone());
    let catalogue = CatalogueReader::replicated(
        Arc::clone(&store),
        concrete_store,
        replication.metrics_handle(),
        config.cluster.bounded_replica_reads,
        config.cluster.bounded_replica_max_lag_entries,
    );
    let membership_manager = MembershipManager::replicated(
        client.clone(),
        replication.clone(),
        Arc::clone(&store),
        identity.clone(),
        local,
        configured_join_url(config)?,
        configured_artwork_url(config)?,
        JoinSecrets {
            raft: secrets.raft,
            api: secrets.api,
            credential_key: read_secret(
                &config.cluster.credential_key_path(&config.storage.data_dir),
            )?,
        },
        load_or_create_activity_signing_key(&config.storage.data_dir)?,
        activation_marker,
        role,
        config.storage.data_dir.clone(),
    )
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    let selected = SelectedStore {
        store,
        identity,
        credential_key,
        backend: SelectedBackend::Replicated,
        membership: membership_manager,
        replication,
        catalogue,
        local_client: Some(client),
        _daemon_lock: daemon_lock,
    };
    finalize_pending_join_best_effort(config, &selected).await;
    Ok(selected)
}

/// Publish a joined node's local role before making its active store
/// authoritative.
///
/// The ordering is the safety property. A retry after the failpoint sees the
/// role record and no activation marker, re-enters the idempotent join path,
/// and can never default a learner to voter. The private failpoint is used by
/// the crash-restart regression below; it is not configurable in production.
#[cfg(feature = "hiqlite-store")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JoinActivationFailpoint {
    None,
    #[cfg(test)]
    AfterMembership,
}

#[cfg(feature = "hiqlite-store")]
fn publish_join_activation(
    data_dir: &Path,
    active: &Path,
    marker: &ActivationMarker,
    membership: &LocalMembership,
    failpoint: JoinActivationFailpoint,
) -> Result<(), StoreError> {
    write_local_membership(data_dir, membership)?;
    sync_directory(data_dir)?;
    #[cfg(test)]
    if failpoint == JoinActivationFailpoint::AfterMembership {
        return Err(StoreError::Migration(
            "join activation failpoint after durable membership".to_owned(),
        ));
    }
    #[cfg(not(test))]
    let _ = failpoint;
    write_activation_marker(active, marker)?;
    sync_directory(active)?;
    sync_directory(data_dir)
}

#[cfg(feature = "hiqlite-store")]
async fn finalize_pending_join_best_effort(config: &Config, selected: &SelectedStore) {
    if let Err(error) = finalize_pending_join(config, selected).await {
        // The voter and activation marker are already durable at this point.
        // Finalization only consumes the coordinator's one-time record, so a
        // temporarily unavailable coordinator must not take this voter back
        // offline. The identity-bound token file remains for the next boot.
        tracing::warn!(
            error = %error,
            "joined voter is healthy but token finalization is pending; startup will retry"
        );
    }
}

#[cfg(feature = "hiqlite-store")]
async fn finalize_pending_join(
    config: &Config,
    selected: &SelectedStore,
) -> Result<(), StoreError> {
    let path = &config.cluster.join_token_file;
    if path.as_os_str().is_empty() || !path_exists(path)? {
        return Ok(());
    }
    let Some(membership) = read_local_membership(&config.storage.data_dir)? else {
        tracing::warn!("join-token file is present but this node has no staged join; ignoring it");
        return Ok(());
    };
    let Some(expected_digest) = membership.join_token_digest.as_deref() else {
        tracing::warn!(
            "join-token file is present on an initial voter that was never admitted; ignoring it"
        );
        return Ok(());
    };
    let token = read_join_token_file(path)?;
    let token_digest = join_token_digest(&token);
    if token_digest != expected_digest {
        tracing::warn!("join-token file does not match this node's staged join; ignoring it");
        return Ok(());
    }
    let payload = decode_join_token(&token)
        .map_err(|error| StoreError::Migration(format!("{}: {}", error.code(), error)))?;
    if payload.cluster_id() != membership.cluster_id
        || payload.raft_id() != membership.raft_id
        || payload.role() != membership.role
    {
        return Err(StoreError::Identity(
            "staged join token does not match local membership identity".to_owned(),
        ));
    }
    finalize_remote_join(
        &payload,
        FinalizeJoinRequest {
            token_digest,
            raft_id: membership.raft_id,
            node_id: selected.identity.node_id.clone(),
        },
    )
    .await?;
    std::fs::remove_file(path)
        .map_err(|error| migration_io("removing redeemed token", path, error))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
fn read_join_token_file(path: &Path) -> Result<String, StoreError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| migration_io("reading join token", path, error))?;
    let token = raw.trim();
    if token.is_empty() || token.lines().count() != 1 {
        return Err(StoreError::Migration(
            "join_token_invalid: join-token file must contain exactly one token".to_owned(),
        ));
    }
    Ok(token.to_owned())
}

#[cfg(feature = "hiqlite-store")]
fn persist_join_secret(path: &Path, secret: &str) -> Result<(), StoreError> {
    if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::Migration(
            "join_token_invalid: encrypted cluster secret is malformed".to_owned(),
        ));
    }
    if path_exists(path)? {
        if read_secret(path)? != secret {
            return Err(StoreError::Identity(
                "join token secrets do not match the interrupted local join".to_owned(),
            ));
        }
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        StoreError::Migration("join secret path has no parent directory".to_owned())
    })?;
    std::fs::create_dir_all(parent).map_err(|error| migration_io("creating", parent, error))?;
    write_private_file(path, format!("{secret}\n").as_bytes())?;
    sync_directory(parent)
}

#[cfg(feature = "hiqlite-store")]
async fn redeem_remote_join(
    payload: &JoinToken,
    request: RedeemJoinRequest,
) -> Result<(), StoreError> {
    let path = match payload.role() {
        ClusterRole::Voter => "/api/v1/cluster/join/redeem",
        ClusterRole::Learner => "/api/v1/cluster/learner/join/redeem",
    };
    post_join_request(payload, path, &request).await
}

#[cfg(feature = "hiqlite-store")]
async fn finalize_remote_join(
    payload: &JoinToken,
    request: FinalizeJoinRequest,
) -> Result<(), StoreError> {
    let path = match payload.role() {
        ClusterRole::Voter => "/api/v1/cluster/join/finalize",
        ClusterRole::Learner => "/api/v1/cluster/learner/join/finalize",
    };
    post_join_request(payload, path, &request).await
}

#[cfg(feature = "hiqlite-store")]
async fn post_join_request<T: Serialize>(
    payload: &JoinToken,
    path: &str,
    request: &T,
) -> Result<(), StoreError> {
    let response = reqwest::Client::new()
        .post(format!("{}{path}", payload.bootstrap_http()))
        .json(request)
        .send()
        .await
        .map_err(|error| StoreError::Database(format!("contacting join coordinator: {error}")))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let error = response
        .json::<JoinApiError>()
        .await
        .unwrap_or(JoinApiError {
            code: "membership_internal".to_owned(),
            message: "join coordinator refused the request".to_owned(),
        });
    Err(StoreError::Migration(format!(
        "{}: {} (HTTP {status})",
        error.code, error.message
    )))
}

#[cfg(feature = "hiqlite-store")]
#[derive(Deserialize)]
struct JoinApiError {
    code: String,
    message: String,
}

/// Connect a maintenance command to an already-running activated voter.
///
/// This path never imports and never starts a second local voter. It is safe
/// beside `plurxd run`; an unmigrated directory is refused before creating any
/// replicated state.
#[cfg(feature = "hiqlite-store")]
pub async fn connect_activated_store(config: &Config) -> Result<Arc<dyn Store>, StoreError> {
    install_default_crypto_provider();
    let active = config.storage.data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    if !path_exists(&active)? {
        return Err(StoreError::Migration(format!(
            "{} is not activated; only `plurxd run` may import SQLite into Hiqlite",
            config.storage.data_dir.display()
        )));
    }
    let marker = read_activation_marker(&active)?;
    let identity = super::initialize_identity(&config.storage.data_dir, &marker.cluster_id)?;
    let secret_api = read_secret(&config.storage.data_dir.join(API_SECRET_FILENAME))?;
    let address = local_voter_api_address(config, &identity)?;
    let client = Client::remote(vec![address], true, true, secret_api, false, None)
        .await
        .map_err(|error| {
            StoreError::Database(format!(
                "connecting to the running local Hiqlite voter: {error}"
            ))
        })?;
    let store = HiqliteAuthStore::open(client, &active.join("telemetry.db")).await?;
    verify_store_identity(&store, &marker.cluster_id).await?;
    Ok(Arc::new(store))
}

#[cfg(feature = "hiqlite-store")]
fn local_voter_api_address(
    config: &Config,
    identity: &super::ClusterIdentity,
) -> Result<String, StoreError> {
    match read_local_membership(&config.storage.data_dir)? {
        Some(membership)
            if membership.cluster_id == identity.cluster_id
                && membership.node_id == identity.node_id =>
        {
            Ok(membership.local.api_address)
        }
        Some(_) => Err(StoreError::Identity(
            "membership.json does not match activation.json and node.id".to_owned(),
        )),
        None => Ok(local_client_address(config.cluster.api_bind).to_string()),
    }
}

#[cfg(feature = "hiqlite-store")]
async fn activate_fresh_store(
    config: &Config,
    failpoint: Option<ActivationFailpoint>,
    daemon_lock: File,
    identity: super::ClusterIdentity,
    credential_key: Arc<CredentialKey>,
) -> Result<SelectedStore, StoreError> {
    write_activation_attempt(&config.storage.data_dir)?;
    ActivationFailpoint::crash_if(failpoint, ActivationFailpoint::Quiescence);

    let prepared = prepare_sqlite_import(&config.storage.data_dir)?;
    if identity.cluster_id != prepared.cluster_id {
        return Err(StoreError::Identity(format!(
            "SQLite instance.id changed while preparing activation: opened {}, backup contains {}",
            identity.cluster_id, prepared.cluster_id
        )));
    }
    verify_legacy_ownership(&prepared.backup_path, &identity)?;
    let secrets = load_or_create_secrets(&config.storage.data_dir)?;
    let incoming = config.storage.data_dir.join(HIQLITE_INCOMING_DIRNAME);
    std::fs::create_dir(&incoming).map_err(|error| migration_io("creating", &incoming, error))?;
    if let Err(error) = sync_directory(&config.storage.data_dir) {
        return Err(cleanup_incoming_failure(&incoming, error));
    }

    // The temporary import voter always stays on loopback. This preserves the
    // pre-M3 activation failpoints and lets cleartext staging shut down cleanly.
    // Once the atomic active target exists, the closed-store readdress path
    // below rebuilds sole-voter metadata with an explicitly advertised address.
    let client = match start_voter(config, &incoming, &secrets, &identity, None, false, true).await
    {
        Ok((client, _)) => client,
        Err(error) => {
            remove_abandoned_incoming(&config.storage.data_dir)?;
            return Err(error);
        }
    };
    let store = match HiqliteAuthStore::bootstrap(
        client.clone(),
        &prepared.cluster_id,
        &incoming.join("telemetry.db"),
    )
    .await
    {
        Ok(store) => store,
        Err(error) => return abort_incoming(client, &incoming, error, false).await,
    };
    ActivationFailpoint::crash_if(failpoint, ActivationFailpoint::Incoming);

    let report = match store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
    {
        Ok(report) => report,
        Err(error) => {
            drop(store);
            return abort_incoming(client, &incoming, error, false).await;
        }
    };
    let marker = ActivationMarker::from_report(&prepared.cluster_id, report);
    if let Err(error) = write_activation_marker(&incoming, &marker) {
        drop(store);
        return abort_incoming(client, &incoming, error, false).await;
    }
    ActivationFailpoint::crash_if(failpoint, ActivationFailpoint::Marker);

    drop(store);
    if let Err(error) = shutdown_voter(&client, false).await {
        drop(client);
        return Err(cleanup_incoming_failure(
            &incoming,
            StoreError::Database(format!("stopping incoming Hiqlite voter: {error}")),
        ));
    }
    drop(client);
    if let Err(error) = sync_directory(&incoming) {
        return Err(cleanup_incoming_failure(&incoming, error));
    }

    let active = config.storage.data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    if let Err(error) = std::fs::rename(&incoming, &active) {
        return Err(cleanup_incoming_failure(
            &incoming,
            migration_io("activating", &active, error),
        ));
    }
    ActivationFailpoint::crash_if(failpoint, ActivationFailpoint::Rename);
    sync_directory(&config.storage.data_dir)?;
    remove_activation_attempt(&config.storage.data_dir)?;

    readdress_single_voter_if_needed(config)?;
    let selected = open_active_store_with_key(config, daemon_lock, Some(credential_key)).await?;
    finish_readdress(&config.storage.data_dir)?;
    Ok(selected)
}

#[cfg(feature = "hiqlite-store")]
impl ActivationMarker {
    fn from_report(cluster_id: &str, report: SqliteImportReport) -> Self {
        Self {
            marker_version: ACTIVATION_MARKER_VERSION,
            cluster_id: cluster_id.to_owned(),
            source_backup_sha256: report.backup_sha256,
            source_schema_version: report.source_schema_version,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: report.imported_rows,
            table_hashes: report.tables,
            admitted_role: None,
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        if self.marker_version == 0 || self.marker_version > ACTIVATION_MARKER_VERSION {
            return Err(StoreError::Migration(format!(
                "unsupported Hiqlite activation marker version {}",
                self.marker_version
            )));
        }
        if self.cluster_id.trim().is_empty()
            || self.source_schema_version <= 0
            || !(AUTH_SCHEMA_MIGRATION_SOURCE..=AUTH_SCHEMA_VERSION)
                .contains(&self.replicated_schema_version)
            || !is_sha256(&self.source_backup_sha256)
            || self.table_hashes.is_empty()
        {
            return Err(StoreError::Migration(
                "Hiqlite activation marker is incomplete".to_owned(),
            ));
        }
        let mut names = BTreeSet::new();
        for table in &self.table_hashes {
            if table.table.is_empty()
                || !names.insert(table.table.as_str())
                || !is_sha256(&table.sha256)
            {
                return Err(StoreError::Migration(
                    "Hiqlite activation marker has invalid table hashes".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Install the process-level rustls provider every TLS path here depends on.
///
/// `rustls` panics rather than erroring when a process reaches TLS with no
/// default provider, and the crate is built with more than one provider feature
/// reachable, so it will not choose for us. `plurxd run` used to be covered only
/// by accident: `hiqlite::start_node` installs one on the server side, which the
/// maintenance commands never call, so `reset-password` and `refresh-metadata`
/// aborted on every activated node. Installing here rather than in one binary's
/// entry point keeps a future caller from reintroducing that gap.
///
/// Idempotent: a losing race or an already-installed provider returns `Err`,
/// which is the same end state as winning.
#[cfg(feature = "hiqlite-store")]
fn install_default_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Rebuild a sole voter's local Raft metadata around its unchanged state
/// machine, for the two cases that need it before Hiqlite opens the target.
///
/// **Address change.** Openraft supports an atomic `SetNodes` address update,
/// but Hiqlite 0.14 does not expose it. Rebuilding only the one-voter Raft
/// metadata from Hiqlite's metadata-reset backup is safe while there is exactly
/// one voter; doing the same after a peer exists would fork membership history
/// and is refused.
///
/// **Ungraceful shutdown.** Hiqlite's `auto-heal` deletes the whole
/// state-machine database whenever it finds its own lock file at startup, and
/// expects the Raft log to rebuild it. Activation imports the operator's SQLite
/// data straight into that database rather than through Raft, and nothing
/// snapshots before `logs_until_snapshot`, so for an activated node that
/// replay reconstructs nothing and the install is lost. Running first, while
/// the database is still intact, turns that into an ordinary crash recovery:
/// the committed state machine is preserved and only the one-voter Raft
/// metadata is rebuilt around it. A sole voter acks a write only after applying
/// it, so the discarded log tail holds nothing a client was told was durable.
/// A node with peers is left to Hiqlite, whose catch-up from the leader is the
/// correct recovery there and does not fork membership.
#[cfg(feature = "hiqlite-store")]
fn readdress_single_voter_if_needed(config: &Config) -> Result<(), StoreError> {
    let active = config.storage.data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    let ungraceful_shutdown = path_exists(&active.join("state_machine").join("lock"))?;
    let advertised = !config.cluster.advertise_host.trim().is_empty();
    if !advertised && !ungraceful_shutdown {
        return Ok(());
    }
    let existing_membership = read_local_membership(&config.storage.data_dir)?;
    // This rebuilds one-voter Raft metadata around a preserved state machine.
    // A learner is never a sole voter — it was admitted into an existing
    // cluster and its recovery is catching up from the leader, exactly as for
    // any node with peers. The peer count below reaches the same conclusion,
    // but only after opening the closed state machine; say it here instead.
    if existing_membership
        .as_ref()
        .is_some_and(|membership| membership.role.is_learner())
    {
        return Ok(());
    }
    // Settle on whether the committed address already matches configuration,
    // not on whether it looks like loopback. A loopback-literal advertise_host
    // is a legitimate configuration - two daemons on one host, and the whole
    // cluster harness - and testing loopback-ness there never reaches a fixed
    // point, so every boot would rebuild the state machine and every joined
    // follower would refuse to start.
    let address_changed = match &existing_membership {
        Some(membership) if advertised => {
            membership.local != configured_local_peer(config, membership.raft_id)?
        }
        Some(_) => false,
        None => advertised,
    };
    if !address_changed && !ungraceful_shutdown {
        return Ok(());
    }

    let marker = read_activation_marker(&active)?;
    let mut identity = super::initialize_identity(&config.storage.data_dir, &marker.cluster_id)?;
    if let Some(membership) = &existing_membership {
        if membership.cluster_id != marker.cluster_id || membership.node_id != identity.node_id {
            return Err(StoreError::Identity(
                "membership.json does not match activation.json and node.id".to_owned(),
            ));
        }
        identity.raft_id = membership.raft_id;
    }
    // Decide sole-voter-ness from replicated cluster_nodes rows in the closed
    // state machine, not from membership.json. redeem() writes an admitted peer
    // there before it answers the joining node, whereas membership.json records
    // the bootstrap list once and is never refreshed, so a coordinator that has
    // since admitted a peer still reads as a lone voter and would rebuild
    // one-node Raft metadata that silently ejects it.
    let admitted_peers = admitted_peer_count(&active, identity.raft_id)?;
    if admitted_peers > 0 {
        if address_changed {
            return Err(StoreError::Migration(format!(
                "cluster.advertise_host cannot change after another voter or learner exists \
                 ({admitted_peers} other node(s) are admitted); use a future online \
                 membership-reconfiguration path instead"
            )));
        }
        // Ungraceful shutdown with peers: rebuilding one-node Raft metadata
        // here would eject them. Hiqlite rebuilds this node from the leader.
        tracing::warn!(
            admitted_peers,
            "recovering from an ungraceful shutdown by catching up from the cluster; during the \
             first rollout of this recovery fix, update every voter before restarting a \
             crash-recovering majority because an old leader retains the old sender deadline"
        );
        return Ok(());
    }
    let desired = configured_local_peer(config, identity.raft_id)?;
    let backup = config
        .storage
        .data_dir
        .join(HIQLITE_READDRESS_BACKUP_DIRNAME);
    if path_exists(&backup)? {
        return Err(StoreError::Migration(format!(
            "refusing readdress because recovery directory {} already exists",
            backup.display()
        )));
    }

    // The daemon lock proves no voter is running from this directory. Snapshot
    // the closed SQLite state machine directly and clear only Hiqlite's private
    // applied-log metadata, matching its own backup implementation. The next
    // open initializes a new one-voter Raft log around unchanged application
    // rows and validates instance.id before the old target is removed.
    remove_abandoned_incoming(&config.storage.data_dir)?;
    let incoming = config.storage.data_dir.join(HIQLITE_INCOMING_DIRNAME);
    std::fs::create_dir_all(&incoming)
        .map_err(|error| migration_io("creating", &incoming, error))?;
    let state_target = incoming
        .join("state_machine")
        .join("db")
        .join(HIQLITE_DATABASE_FILENAME);
    if let Err(error) = snapshot_hiqlite_state_machine(
        &active
            .join("state_machine")
            .join("db")
            .join(HIQLITE_DATABASE_FILENAME),
        &state_target,
    ) {
        let _ = remove_abandoned_incoming(&config.storage.data_dir);
        return Err(error);
    }

    // telemetry.db is node-local, but it still contains useful continuity for
    // status and should survive a membership-address repair.
    let telemetry_target = incoming.join("telemetry.db");
    if let Err(error) = snapshot_sqlite_file(&active.join("telemetry.db"), &telemetry_target) {
        let _ = remove_abandoned_incoming(&config.storage.data_dir);
        return Err(error);
    }

    write_activation_marker(&incoming, &marker)?;
    verify_state_machine_snapshot_identity(&state_target, &marker.cluster_id)?;
    let membership = LocalMembership {
        version: local_membership_version(ClusterRole::Voter),
        cluster_id: identity.cluster_id.clone(),
        node_id: identity.node_id.clone(),
        raft_id: identity.raft_id,
        local: desired.clone(),
        bootstrap: vec![desired],
        join_token_digest: existing_membership.and_then(|membership| membership.join_token_digest),
        role: ClusterRole::Voter,
    };
    sync_directory(&incoming)?;

    write_readdress_record(&config.storage.data_dir, &membership)?;
    std::fs::rename(&active, &backup)
        .map_err(|error| migration_io("parking pre-readdress target", &backup, error))?;
    if let Err(error) = std::fs::rename(&incoming, &active) {
        let restore = std::fs::rename(&backup, &active);
        return Err(match restore {
            Ok(()) => migration_io("activating readdressed target", &active, error),
            Err(restore) => StoreError::Migration(format!(
                "activating readdressed target {} failed: {error}; restoring {} also failed: \
                 {restore}",
                active.display(),
                backup.display()
            )),
        });
    }
    sync_directory(&config.storage.data_dir)?;
    write_local_membership(&config.storage.data_dir, &membership)?;
    Ok(())
}

/// Count admitted cluster nodes other than this one, read directly from the
/// closed replicated state machine.
///
/// The daemon lock proves no voter is running, so this is a consistent read of
/// committed state: `redeem` inserts an admitted peer into `cluster_nodes`
/// before answering the joining node, and `remove_voter` tombstones it with
/// `removed_at`. A store activated before the membership schema existed has no
/// such table, which means no peer was ever admitted.
#[cfg(feature = "hiqlite-store")]
fn admitted_peer_count(active: &Path, raft_id: u64) -> Result<u64, StoreError> {
    let database = active
        .join("state_machine")
        .join("db")
        .join(HIQLITE_DATABASE_FILENAME);
    if !path_exists(&database)? {
        return Ok(0);
    }
    let connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        StoreError::Migration(format!(
            "opening state machine {} to check admitted nodes: {error}",
            database.display()
        ))
    })?;
    let table_exists = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'cluster_nodes'",
            (),
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| {
            StoreError::Migration(format!("inspecting replicated membership schema: {error}"))
        })?;
    if table_exists == 0 {
        return Ok(0);
    }
    let peers = connection
        .query_row(
            "SELECT COUNT(*) FROM cluster_nodes WHERE removed_at IS NULL AND raft_id != ?1",
            [raft_id as i64],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| {
            StoreError::Migration(format!("reading replicated cluster node records: {error}"))
        })?;
    Ok(peers.max(0) as u64)
}

#[cfg(feature = "hiqlite-store")]
fn snapshot_hiqlite_state_machine(source: &Path, target: &Path) -> Result<(), StoreError> {
    snapshot_sqlite_file(source, target)?;
    let connection = Connection::open(target).map_err(|error| {
        StoreError::Migration(format!(
            "opening readdressed state-machine snapshot {}: {error}",
            target.display()
        ))
    })?;
    connection
        .execute("DELETE FROM _metadata", ())
        .map_err(|error| {
            StoreError::Migration(format!(
                "resetting readdressed Raft state-machine metadata: {error}"
            ))
        })?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .map_err(|error| {
            StoreError::Migration(format!(
                "canonicalizing readdressed state-machine snapshot: {error}"
            ))
        })?;
    drop(connection);
    File::open(target)
        .and_then(|file| file.sync_all())
        .map_err(|error| migration_io("syncing", target, error))
}

#[cfg(feature = "hiqlite-store")]
fn verify_state_machine_snapshot_identity(
    snapshot: &Path,
    expected: &str,
) -> Result<(), StoreError> {
    let connection = Connection::open_with_flags(
        snapshot,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        StoreError::Migration(format!(
            "opening readdressed state-machine snapshot {}: {error}",
            snapshot.display()
        ))
    })?;
    let actual = connection
        .query_row(
            "SELECT value FROM settings WHERE key = 'instance.id'",
            (),
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| {
            StoreError::Migration(format!(
                "reading instance.id from readdressed state-machine snapshot: {error}"
            ))
        })?;
    if actual != expected {
        return Err(StoreError::Identity(format!(
            "readdressed state-machine instance.id is {actual}, expected {expected}"
        )));
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
fn snapshot_sqlite_file(source: &Path, target: &Path) -> Result<(), StoreError> {
    let parent = target.parent().expect("SQLite snapshot target has parent");
    std::fs::create_dir_all(parent).map_err(|error| migration_io("creating", parent, error))?;
    let source_connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        StoreError::Migration(format!(
            "opening node-local SQLite {}: {error}",
            source.display()
        ))
    })?;
    create_private_file(target)?;
    let mut target_connection = Connection::open(target).map_err(|error| {
        StoreError::Migration(format!(
            "opening node-local snapshot {}: {error}",
            target.display()
        ))
    })?;
    {
        let backup = Backup::new(&source_connection, &mut target_connection).map_err(|error| {
            StoreError::Migration(format!("starting node-local SQLite snapshot: {error}"))
        })?;
        backup
            .run_to_completion(256, Duration::from_millis(10), None)
            .map_err(|error| {
                StoreError::Migration(format!("copying node-local SQLite snapshot: {error}"))
            })?;
    }
    drop(target_connection);
    File::open(target)
        .and_then(|file| file.sync_all())
        .map_err(|error| migration_io("syncing", target, error))
}

#[cfg(feature = "hiqlite-store")]
async fn open_active_store(
    config: &Config,
    daemon_lock: File,
) -> Result<SelectedStore, StoreError> {
    open_active_store_with_key(config, daemon_lock, None).await
}

#[cfg(feature = "hiqlite-store")]
async fn open_active_store_with_key(
    config: &Config,
    daemon_lock: File,
    credential_key: Option<Arc<CredentialKey>>,
) -> Result<SelectedStore, StoreError> {
    let active = config.storage.data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    require_real_directory(&active)?;
    let mut marker = read_activation_marker(&active)?;
    let mut local_membership = read_local_membership(&config.storage.data_dir)?;
    // A runtime promotion fsyncs membership.json before activation.json. If
    // power fails between them, the voter record is the durable proof that a
    // live process already observed its committed vote; finish the second
    // half before enforcing the ordinary exact-match rule.
    if local_membership
        .as_ref()
        .is_some_and(|membership| membership.role == ClusterRole::Voter)
        && marker.admitted_role == Some(ClusterRole::Learner)
    {
        persist_promoted_voter_role(&config.storage.data_dir)?;
        marker = read_activation_marker(&active)?;
        local_membership = read_local_membership(&config.storage.data_dir)?;
    }
    let role = active_store_role(&marker, local_membership.as_ref(), &config.storage.data_dir)?;
    let mut identity = super::initialize_identity(&config.storage.data_dir, &marker.cluster_id)?;
    if let Some(membership) = &local_membership {
        if membership.cluster_id != marker.cluster_id || membership.node_id != identity.node_id {
            return Err(StoreError::Identity(
                "membership.json does not match activation.json and node.id".to_owned(),
            ));
        }
        identity.raft_id = membership.raft_id;
    }
    let secrets = read_existing_secrets(&config.storage.data_dir)?;
    let force_loopback = should_force_loopback(config, local_membership.as_ref());
    let (client, local) = start_voter(
        config,
        &active,
        &secrets,
        &identity,
        local_membership.as_ref(),
        true,
        force_loopback,
    )
    .await?;
    let cleanup_client = client.clone();
    let result = async move {
        let telemetry = active.join("telemetry.db");
        let store = open_store_for_role(role, client.clone(), &telemetry).await?;
        if marker.replicated_schema_version != AUTH_SCHEMA_VERSION {
            marker.replicated_schema_version = AUTH_SCHEMA_VERSION;
            write_activation_marker(&active, &marker)?;
            sync_directory(&active)?;
        }
        if let Err(error) = verify_store_identity(&store, &marker.cluster_id).await {
            drop(store);
            return Err(error);
        }
        // Written here rather than beside the rename so a crash in between still
        // converges: any boot that successfully opens an active target re-asserts
        // it, and this is the only place that can be reached without one.
        if let Err(error) = ensure_activated_source_record(&config.storage.data_dir, &marker) {
            drop(store);
            return Err(error);
        }
        let credential_key = match credential_key {
            Some(key) => key,
            None => match open_active_credential_key(config, &store).await {
                Ok(key) => key,
                Err(error) => {
                    drop(store);
                    return Err(error);
                }
            },
        };
        let concrete_store = Arc::new(store);
        let store: Arc<dyn Store> = concrete_store.clone();
        let replication = status::ReplicationMonitor::replicated(client.clone());
        let catalogue = CatalogueReader::replicated(
            Arc::clone(&store),
            concrete_store,
            replication.metrics_handle(),
            config.cluster.bounded_replica_reads,
            config.cluster.bounded_replica_max_lag_entries,
        );
        let membership_file = match local_membership.take() {
            Some(mut membership) => {
                if membership.local != local {
                    membership.local = local.clone();
                    for peer in &mut membership.bootstrap {
                        if peer.raft_id == local.raft_id {
                            *peer = local.clone();
                        }
                    }
                    write_local_membership(&config.storage.data_dir, &membership)?;
                }
                membership
            }
            None => {
                let metrics = client.metrics_db().await.map_err(|error| {
                    StoreError::Database(format!("reading initial cluster membership: {error}"))
                })?;
                // A node reaching this branch has no membership record at all, so
                // it is an initial voter: a learner only ever exists because a
                // join wrote one. Version 1 keeps this file readable by the
                // previous release, which is what makes installing this binary
                // reversible for every node that has not been admitted as a
                // learner.
                let membership = LocalMembership {
                    version: local_membership_version(ClusterRole::Voter),
                    cluster_id: identity.cluster_id.clone(),
                    node_id: identity.node_id.clone(),
                    raft_id: identity.raft_id,
                    local: local.clone(),
                    bootstrap: metrics
                        .membership_config
                        .nodes()
                        .map(|(_, node)| ClusterPeer::from(node))
                        .collect(),
                    join_token_digest: None,
                    role: ClusterRole::Voter,
                };
                write_local_membership(&config.storage.data_dir, &membership)?;
                membership
            }
        };
        let credential_key_secret =
            read_secret(&config.cluster.credential_key_path(&config.storage.data_dir))?;
        let membership = MembershipManager::replicated(
            client.clone(),
            replication.clone(),
            Arc::clone(&store),
            identity.clone(),
            membership_file.local,
            configured_join_url(config)?,
            configured_artwork_url(config)?,
            JoinSecrets {
                raft: secrets.raft,
                api: secrets.api,
                credential_key: credential_key_secret,
            },
            load_or_create_activity_signing_key(&config.storage.data_dir)?,
            marker,
            role,
            config.storage.data_dir.clone(),
        )
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
        Ok(SelectedStore {
            store,
            identity,
            credential_key,
            backend: SelectedBackend::Replicated,
            membership,
            replication,
            catalogue,
            local_client: Some(client),
            _daemon_lock: daemon_lock,
        })
    }
    .await;
    if result.is_err() {
        if let Err(shutdown_error) = shutdown_voter(&cleanup_client, true).await {
            tracing::error!(
                %shutdown_error,
                "failed to stop Hiqlite after active store initialization failed"
            );
        }
    }
    result
}

/// Resolve the key against the authoritative replicated rows on every reopen.
///
/// The retained SQLite source cannot provide this census after activation: a
/// household may link Trakt later, and a lost or replaced key must refuse at
/// startup rather than surface as an unrelated outbound-sync failure.
#[cfg(feature = "hiqlite-store")]
async fn open_active_credential_key(
    config: &Config,
    store: &HiqliteAuthStore,
) -> Result<Arc<CredentialKey>, StoreError> {
    let mut census = SealedRowCensus::default();
    for auth in store.list_trakt_auth().await? {
        census.observe_row(&auth.access_token, &auth.refresh_token);
    }
    let path = config.cluster.credential_key_path(&config.storage.data_dir);
    let key = secrets::open_credential_key(&path, &census)
        .map_err(|error| StoreError::Identity(error.to_string()))?;
    tracing::debug!(
        key_id = %key.id(),
        wrapped_rows = census.sealed_rows(),
        "opened the node-local credential key for the replicated store"
    );
    Ok(Arc::new(key))
}

/// Who the lock file says is holding the data directory.
///
/// This never decides whether to refuse — the advisory lock is the only
/// authority for that, and a recorded id can be stale, truncated, or written
/// by a process that has since died. It exists so a refusal can say which of
/// two very different conditions it actually saw, because they call for
/// opposite responses from whoever reads the message.
#[cfg(feature = "hiqlite-store")]
#[derive(Debug, PartialEq, Eq)]
enum DaemonLockHolder {
    /// The recorded owner is this very process.
    ///
    /// Not a second daemon under any reading: an earlier activation inside
    /// this process still has its lock handle open. In production that is a
    /// leak to fix here; in an in-process test it is the previous activation
    /// not having finished dropping.
    ThisProcess(u32),
    /// A different process. This is the genuine double-start.
    OtherProcess(u32),
    /// The lock file carried no usable owner record — it predates this
    /// bookkeeping, or the holder was interrupted between taking the lock and
    /// recording itself. Treated as another process, because the lock is held
    /// and nothing says it is ours.
    Unidentified,
}

/// How long startup re-attempts the data-directory lock before concluding that
/// another daemon owns it.
///
/// The lock lives exactly as long as the owner's open handle, and closing that
/// handle is the last thing a departing owner does. A container restarted by
/// `Restart=always`, a `systemctl restart`, and an in-process test that
/// re-activates the same directory all hand the incoming owner a predecessor
/// that is still finishing. Refusing on the first `WouldBlock` reported every
/// one of them as "another plurxd process already owns the data directory" —
/// a message that in production means a genuine double-start and is a correct,
/// serious refusal.
///
/// Waiting a bounded window costs nothing on a quiet host, where the first
/// attempt succeeds, and it cannot let a second live daemon through: a running
/// owner holds the lock for its entire lifetime, so it is still holding it when
/// the window expires. The window only converts "not free at this instant" into
/// "not free for five seconds", which is the claim the refusal actually makes.
#[cfg(feature = "hiqlite-store")]
const DAEMON_LOCK_ACQUIRE_WINDOW: Duration = Duration::from_secs(5);

/// Gap between attempts inside [`DAEMON_LOCK_ACQUIRE_WINDOW`].
///
/// Short enough that a predecessor closing its handle is picked up promptly,
/// long enough that the wait is not a spin. There is no OS notification for an
/// advisory lock being released without blocking on it, and blocking would give
/// up the bound this whole path exists to keep.
#[cfg(feature = "hiqlite-store")]
const DAEMON_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

#[cfg(feature = "hiqlite-store")]
async fn acquire_daemon_lock(data_dir: &Path) -> Result<File, StoreError> {
    acquire_daemon_lock_within(data_dir, DAEMON_LOCK_ACQUIRE_WINDOW).await
}

#[cfg(feature = "hiqlite-store")]
async fn acquire_daemon_lock_within(data_dir: &Path, window: Duration) -> Result<File, StoreError> {
    let path = data_dir.join(DAEMON_LOCK_FILENAME);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&path)
        .map_err(|error| migration_io("opening", &path, error))?;
    let mut waited = Duration::ZERO;
    loop {
        match file.try_lock() {
            Ok(()) => {
                record_daemon_lock_holder(&mut file, &path);
                return Ok(file);
            }
            Err(std::fs::TryLockError::WouldBlock) if waited < window => {
                // Async, not `thread::sleep`: this runs on the daemon runtime,
                // and the predecessor whose handle we are waiting on may need
                // that same runtime to finish dropping it.
                let step = DAEMON_LOCK_RETRY_INTERVAL.min(window - waited);
                tokio::time::sleep(step).await;
                waited += step;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(daemon_lock_conflict(
                    data_dir,
                    &read_daemon_lock_holder(&path),
                    waited,
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(migration_io("locking", &path, error))
            }
        }
    }
}

/// Record this process as the owner, so a later contender's refusal can name
/// it. Written under the lock we just took, so no two writers race here.
///
/// Best effort on purpose. The record only ever improves somebody else's error
/// message, and the lock — which is the thing that actually protects the data
/// directory — is already held by the time this runs. Refusing startup because
/// a seven-byte cosmetic write failed would trade a working server for a nicer
/// diagnostic; a contender falls back to
/// [`DaemonLockHolder::Unidentified`] instead.
#[cfg(feature = "hiqlite-store")]
fn record_daemon_lock_holder(file: &mut File, path: &Path) {
    if let Err(error) = file
        .set_len(0)
        .and_then(|()| file.rewind())
        .and_then(|_| writeln!(file, "{}", std::process::id()))
        .and_then(|()| file.flush())
    {
        tracing::debug!(
            path = %path.display(),
            %error,
            "could not record this process as the data-directory lock owner"
        );
    }
}

/// Read the recorded owner without taking the lock. Every failure is
/// [`DaemonLockHolder::Unidentified`]: this only ever improves a message, so a
/// missing or malformed record must not turn into a startup error of its own.
#[cfg(feature = "hiqlite-store")]
fn read_daemon_lock_holder(path: &Path) -> DaemonLockHolder {
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|recorded| recorded.trim().parse::<u32>().ok())
    {
        Some(pid) if pid == std::process::id() => DaemonLockHolder::ThisProcess(pid),
        Some(pid) => DaemonLockHolder::OtherProcess(pid),
        None => DaemonLockHolder::Unidentified,
    }
}

/// Describe a lock that stayed held for the whole acquire window.
///
/// The two conditions are not variants of one complaint. Another live process
/// means the operator started a second daemon on one data directory and must
/// stop one of them; this process means our own handle leaked and no operator
/// action would help. Printing the first text for the second condition sends
/// whoever reads it looking for a process that does not exist.
#[cfg(feature = "hiqlite-store")]
fn daemon_lock_conflict(
    data_dir: &Path,
    holder: &DaemonLockHolder,
    waited: Duration,
) -> StoreError {
    let data_dir = data_dir.display();
    let waited = waited.as_secs_f32();
    StoreError::Migration(match holder {
        DaemonLockHolder::ThisProcess(pid) => format!(
            "the data directory {data_dir} is still locked inside this plurxd process \
             (pid {pid}) after waiting {waited:.1}s for the previous owner's handle to \
             close. No second plurxd process is involved: an earlier activation in this \
             process never dropped its data-directory lock"
        ),
        DaemonLockHolder::OtherProcess(pid) => format!(
            "another plurxd process already owns the data directory {data_dir} \
             (pid {pid}) and still held it after waiting {waited:.1}s"
        ),
        DaemonLockHolder::Unidentified => format!(
            "another plurxd process already owns the data directory {data_dir} \
             (owner pid not recorded) and still held it after waiting {waited:.1}s"
        ),
    })
}

#[cfg(feature = "hiqlite-store")]
async fn verify_store_identity(store: &HiqliteAuthStore, expected: &str) -> Result<(), StoreError> {
    let actual = store.instance_id().await?;
    if actual != expected {
        return Err(StoreError::Identity(format!(
            "active Hiqlite instance.id is {actual}, activation marker expects {expected}"
        )));
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
struct ClusterSecrets {
    raft: String,
    api: String,
}

#[cfg(feature = "hiqlite-store")]
fn load_or_create_secrets(data_dir: &Path) -> Result<ClusterSecrets, StoreError> {
    Ok(ClusterSecrets {
        raft: load_or_create_secret(data_dir, RAFT_SECRET_FILENAME)?,
        api: load_or_create_secret(data_dir, API_SECRET_FILENAME)?,
    })
}

/// The role a boot resumes from this data directory's durable record.
///
/// It decides two things and no others: whether Hiqlite is told not to promote
/// this node, and what committed state startup waits for. Everything a running
/// daemon decides about leadership or singleton work is re-derived from live
/// committed membership instead, because a learner can be promoted while the
/// process runs. A directory with no record at all is an initial voter.
#[cfg(feature = "hiqlite-store")]
fn boot_cluster_role(local_membership: Option<&LocalMembership>) -> ClusterRole {
    local_membership
        .map(|membership| membership.role)
        .unwrap_or_default()
}

/// Resolve an active store's boot role without granting authority from a
/// missing joined-node record.
#[cfg(feature = "hiqlite-store")]
fn active_store_role(
    marker: &ActivationMarker,
    local_membership: Option<&LocalMembership>,
    data_dir: &Path,
) -> Result<ClusterRole, StoreError> {
    match local_membership {
        Some(membership)
            if marker
                .admitted_role
                .is_some_and(|admitted_role| admitted_role != membership.role) =>
        {
            Err(StoreError::Identity(
                "membership.json role does not match activation.json admitted role".to_owned(),
            ))
        }
        Some(membership) => Ok(membership.role),
        None if marker.admitted_role.is_some() => Err(StoreError::Identity(format!(
            "activation.json records this joined node's admitted role but {} is missing; restore \
             the durable membership record before startup rather than guessing that this node may vote",
            data_dir.join(LOCAL_MEMBERSHIP_FILENAME).display()
        ))),
        // Markers written before role-aware joins describe only the original
        // one-voter activation. Preserve that upgrade path.
        None => Ok(ClusterRole::Voter),
    }
}

/// Open the replicated store the way this node's role is allowed to.
///
/// Replicated schema migration is voter work. A learner opens the store and
/// refuses a version it does not implement, but never advances one: a
/// migration is a cluster-wide state change, and a node that carries no vote
/// has no business proposing one. It also cannot help — a learner is by
/// construction the node most likely to be behind, so the version it would
/// migrate *to* is the one it happens to ship rather than the one the cluster
/// agreed on.
///
/// A named function rather than a `match` inline in the boot path, because
/// this is the whole of "a learner never proposes a schema migration" and the
/// boot path around it needs a real cluster, a real join, and a real data
/// directory to reach.
#[cfg(feature = "hiqlite-store")]
async fn open_store_for_role(
    role: ClusterRole,
    client: Client,
    telemetry: &Path,
) -> Result<HiqliteAuthStore, StoreError> {
    match role {
        ClusterRole::Voter => HiqliteAuthStore::open_or_migrate(client, telemetry).await,
        ClusterRole::Learner => HiqliteAuthStore::open(client, telemetry).await,
    }
}

#[cfg(feature = "hiqlite-store")]
async fn start_voter(
    config: &Config,
    target: &Path,
    secrets: &ClusterSecrets,
    identity: &super::ClusterIdentity,
    local_membership: Option<&LocalMembership>,
    active_transport: bool,
    force_loopback: bool,
) -> Result<(Client, ClusterPeer), StoreError> {
    let raft_bind = if force_loopback {
        local_client_address(config.cluster.raft_bind)
    } else {
        config.cluster.raft_bind
    };
    let api_bind = if force_loopback {
        local_client_address(config.cluster.api_bind)
    } else {
        config.cluster.api_bind
    };
    validate_cluster_listener("raft", raft_bind, active_transport)?;
    validate_cluster_listener("cluster API", api_bind, active_transport)?;

    let role = boot_cluster_role(local_membership);
    let local = match local_membership {
        Some(membership) => {
            if !local_membership_version_matches_role(membership.version, membership.role)
                || membership.cluster_id != identity.cluster_id
                || membership.node_id != identity.node_id
                || membership.raft_id != identity.raft_id
                || membership.local.raft_id != identity.raft_id
            {
                return Err(StoreError::Identity(
                    "local cluster membership does not match node.id or activation marker"
                        .to_owned(),
                ));
            }
            configured_or_persisted_local_peer(config, membership, force_loopback)?
        }
        None if force_loopback => ClusterPeer {
            raft_id: identity.raft_id,
            raft_address: raft_bind.to_string(),
            api_address: api_bind.to_string(),
        },
        None => configured_local_peer(config, identity.raft_id)?,
    };
    let mut nodes = local_membership
        .map(|membership| membership.bootstrap.clone())
        .unwrap_or_default();
    nodes.retain(|peer| peer.raft_id != local.raft_id);
    nodes.push(local.clone());
    nodes.sort_by_key(|peer| peer.raft_id);
    nodes.dedup_by_key(|peer| peer.raft_id);
    let nodes = hiqlite_nodes_for_voter(&nodes, local.raft_id)?;

    // The daemon lock guards one data directory, but these ports are host-wide
    // and default to fixed values, so two data directories on one host collide.
    // Without this check the node does not report a bind conflict: it reaches a
    // foreign voter's listener, retries the handshake for the full start
    // timeout, and then blames the migration. Check first and say which port.
    for (role, address) in [("raft", raft_bind), ("cluster API", api_bind)] {
        if let Err(error) = std::net::TcpListener::bind(address) {
            return Err(StoreError::Migration(format!(
                "the {role} port {address} is not available: {error}. Another plurxd on \
                 this host is using it — cluster ports are host-wide, so a second data \
                 directory needs its own raft_bind and api_bind"
            )));
        }
    }
    let node_config = NodeConfig {
        node_id: identity.raft_id,
        nodes,
        listen_addr_api: Cow::Owned(api_bind.ip().to_string()),
        listen_addr_raft: Cow::Owned(raft_bind.ip().to_string()),
        data_dir: Cow::Owned(target.to_string_lossy().into_owned()),
        filename_db: Cow::Borrowed(HIQLITE_DATABASE_FILENAME),
        secret_raft: secrets.raft.clone(),
        secret_api: secrets.api.clone(),
        tls_raft: active_transport.then_some(ServerTlsConfig::TlsAutoCertificates),
        tls_api: active_transport.then_some(ServerTlsConfig::TlsAutoCertificates),
        // Defence in depth, not the guard. Hiqlite reads this exactly once, to
        // decide whether this node asks the leader to promote it during
        // startup reconciliation, and never consults it again — in particular
        // `become_member` is a route on every node that never reads its
        // target's hint. It stops a learner from promoting *itself*; it is not
        // what stops one from acting like a voter.
        learner_only: role.is_learner(),
        ..production_hiqlite_defaults(config)
    };
    let client = hiqlite::start_node(node_config)
        .await
        .map_err(|error| StoreError::Database(format!("starting Hiqlite voter: {error}")))?;
    if tokio::time::timeout(HIQLITE_HEALTH_TIMEOUT, client.wait_until_healthy_db())
        .await
        .is_err()
    {
        let _ = shutdown_voter(&client, active_transport).await;
        return Err(StoreError::Database(format!(
            "Hiqlite voter did not become healthy within {HIQLITE_HEALTH_TIMEOUT:?}"
        )));
    }
    // Committed membership is the gate on startup, and it is the *only* thing
    // stopping a node the cluster has not admitted from running — so what each
    // role waits for has to be exactly what admission means for it. A voter
    // waits for its vote. A learner waits to be a committed member, and must
    // never wait for a promotion it was admitted specifically not to receive.
    let admission_deadline = tokio::time::Instant::now() + MEMBERSHIP_ADMISSION_TIMEOUT;
    loop {
        let metrics = client
            .metrics_db()
            .await
            .map_err(|error| StoreError::Database(format!("reading voter membership: {error}")))?;
        let is_voter = metrics
            .membership_config
            .voter_ids()
            .any(|raft_id| raft_id == identity.raft_id);
        let is_member = metrics
            .membership_config
            .nodes()
            .any(|(raft_id, _)| *raft_id == identity.raft_id);
        let admitted = match role {
            ClusterRole::Voter => is_voter,
            ClusterRole::Learner => is_member,
        };
        if admitted {
            break;
        }
        if tokio::time::Instant::now() >= admission_deadline {
            let _ = shutdown_voter(&client, active_transport).await;
            let reason = match (role, is_member) {
                (ClusterRole::Voter, true) => {
                    "is still a learner and was not promoted before the start timeout"
                }
                (ClusterRole::Learner, _) => {
                    "was not admitted to committed membership as a learner before the start \
                     timeout"
                }
                (ClusterRole::Voter, false) => {
                    "is absent from committed membership; it may have been removed"
                }
            };
            return Err(StoreError::Identity(format!(
                "node {} {reason} in cluster {}",
                identity.node_id, identity.cluster_id
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Membership admission proves that Raft recognizes this node; it does not
    // prove that the local state machine has installed a snapshot needed to
    // reach the cluster's current database image. Capture one quorum-confirmed
    // watermark and keep Store construction/listener startup behind the
    // target-local applied watch. The deadline allows an abandoned in-flight
    // snapshot RPC to reach its configured soft cancellation point, reconnect,
    // and complete one clean transfer.
    let catchup_timeout = Duration::from_secs(config.cluster.install_snapshot_timeout_secs)
        .saturating_add(SNAPSHOT_CATCHUP_GRACE);
    let catchup_deadline = tokio::time::Instant::now() + catchup_timeout;
    let catchup_target = loop {
        match client.db_quorum_watermark().await {
            Ok(watermark) => break watermark.committed_index,
            Err(error) if tokio::time::Instant::now() < catchup_deadline => {
                tracing::debug!(%error, "waiting for startup quorum watermark");
            }
            Err(error) => {
                let _ = shutdown_voter(&client, active_transport).await;
                return Err(StoreError::Database(format!(
                    "node {} could not obtain a startup quorum watermark within \
                     {catchup_timeout:?}: {error}",
                    identity.node_id
                )));
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let local_metrics = match client.local_db_raft_metrics() {
        Ok(metrics) => metrics,
        Err(error) => {
            let _ = shutdown_voter(&client, active_transport).await;
            return Err(StoreError::Database(format!(
                "reading target-local startup progress: {error}"
            )));
        }
    };
    loop {
        let applied = local_metrics
            .snapshot()
            .last_applied_index
            .unwrap_or_default();
        if applied >= catchup_target {
            break;
        }
        if tokio::time::Instant::now() >= catchup_deadline {
            let _ = shutdown_voter(&client, active_transport).await;
            return Err(StoreError::Database(format!(
                "node {} applied only {applied} of startup quorum watermark {catchup_target} \
                 within {catchup_timeout:?}",
                identity.node_id
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if role.is_learner() {
        tracing::info!(
            node_id = %identity.node_id,
            raft_id = identity.raft_id,
            "started as a committed cluster learner: replicated, non-voting, and ineligible \
             for leader-singleton work"
        );
    }
    Ok((client, local))
}

/// The Raft, WAL, and read-pool settings every plurx voter runs with.
///
/// Public because the separate-process cluster harness launches voters of its
/// own: with its own copy of this block, a measurement run could report
/// numbers for a configuration production never runs — and a read-pool
/// comparison in particular would silently measure Hiqlite's default pool at
/// every size. One builder, one set of safety defaults.
#[cfg(feature = "hiqlite-store")]
pub fn production_hiqlite_defaults_with_read_pool(read_pool_size: usize) -> NodeConfig {
    let mut raft_config = NodeConfig::default_raft_config(10_000);
    raft_config.heartbeat_interval = HIQLITE_HEARTBEAT_INTERVAL_MS;
    raft_config.election_timeout_min = HIQLITE_ELECTION_TIMEOUT_MIN_MS;
    raft_config.election_timeout_max = HIQLITE_ELECTION_TIMEOUT_MAX_MS;
    raft_config.install_snapshot_timeout =
        install_snapshot_timeout_ms(DEFAULT_INSTALL_SNAPSHOT_TIMEOUT_SECS);
    NodeConfig {
        health_check_delay_secs: 0,
        wal_size: HIQLITE_WAL_SIZE_BYTES,
        read_pool_size,
        // Snapshot frequency, disaster-recovery retention, and WAL sync retain
        // Hiqlite's established values. The heartbeat/election window above
        // admits measured durable recovery responses; the snapshot transfer
        // deadline remains separately widened for production-sized databases.
        raft_config,
        ..Default::default()
    }
}

#[cfg(feature = "hiqlite-store")]
fn install_snapshot_timeout_ms(seconds: u64) -> u64 {
    seconds
        .checked_mul(1_000)
        .expect("validated snapshot timeout seconds must fit milliseconds")
}

#[cfg(feature = "hiqlite-store")]
fn production_hiqlite_defaults(config: &Config) -> NodeConfig {
    let mut defaults = production_hiqlite_defaults_with_read_pool(config.cluster.read_pool_size);
    defaults.raft_config.install_snapshot_timeout =
        install_snapshot_timeout_ms(config.cluster.install_snapshot_timeout_secs);
    defaults
}

/// Build Hiqlite's connection roster without treating durable Raft ids as
/// vector positions.
#[cfg(feature = "hiqlite-store")]
fn hiqlite_nodes_for_voter(
    peers: &[ClusterPeer],
    local_raft_id: u64,
) -> Result<Vec<Node>, StoreError> {
    if !peers.iter().any(|peer| peer.raft_id == local_raft_id) {
        return Err(StoreError::Identity(format!(
            "local Raft id {local_raft_id} is absent from the configured peer list"
        )));
    }

    let mut ids = BTreeSet::new();
    if let Some(duplicate) = peers.iter().find(|peer| !ids.insert(peer.raft_id)) {
        return Err(StoreError::Identity(format!(
            "configured peer list contains duplicate Raft id {}",
            duplicate.raft_id
        )));
    }

    Ok(peers.iter().map(Node::from).collect())
}

#[cfg(feature = "hiqlite-store")]
fn should_force_loopback(config: &Config, membership: Option<&LocalMembership>) -> bool {
    if !config.cluster.advertise_host.trim().is_empty() {
        return false;
    }
    membership.is_none_or(|membership| cluster_peer_is_loopback(&membership.local))
}

#[cfg(feature = "hiqlite-store")]
fn configured_or_persisted_local_peer(
    config: &Config,
    membership: &LocalMembership,
    force_loopback: bool,
) -> Result<ClusterPeer, StoreError> {
    if force_loopback {
        return if cluster_peer_is_loopback(&membership.local) {
            Ok(ClusterPeer {
                raft_id: membership.raft_id,
                raft_address: local_client_address(config.cluster.raft_bind).to_string(),
                api_address: local_client_address(config.cluster.api_bind).to_string(),
            })
        } else {
            // Fresh activation can bind its temporary staging listener on
            // loopback while committing the explicitly advertised address.
            Ok(membership.local.clone())
        };
    }

    if cluster_peer_is_loopback(&membership.local) {
        // Setting advertise_host is the explicit opt-in that opens an existing
        // one-voter install to peers. The readdress path replaces its persisted
        // loopback membership before the active voter reaches this function.
        return configured_local_peer(config, membership.raft_id);
    }

    let raft_port = stored_peer_port("Raft", &membership.local.raft_address)?;
    let api_port = stored_peer_port("cluster API", &membership.local.api_address)?;
    if raft_port != config.cluster.raft_bind.port() || api_port != config.cluster.api_bind.port() {
        return Err(StoreError::Migration(format!(
            "cluster listener port drift: membership.json records Raft/API ports \
             {raft_port}/{api_port}, but configuration requests {}/{}; changing a joined \
             voter's ports requires a membership reconfiguration and is refused at startup",
            config.cluster.raft_bind.port(),
            config.cluster.api_bind.port()
        )));
    }

    if config.cluster.advertise_host.trim().is_empty() {
        return Ok(membership.local.clone());
    }
    let configured = configured_local_peer(config, membership.raft_id)?;
    if configured != membership.local {
        return Err(StoreError::Migration(
            "cluster.advertise_host differs from membership.json; changing a joined voter's \
             advertised address requires a membership reconfiguration and is refused at startup"
                .to_owned(),
        ));
    }
    Ok(configured)
}

#[cfg(feature = "hiqlite-store")]
fn cluster_peer_is_loopback(peer: &ClusterPeer) -> bool {
    [&peer.raft_address, &peer.api_address]
        .into_iter()
        .all(|address| {
            address
                .parse::<SocketAddr>()
                .is_ok_and(|address| address.ip().is_loopback())
        })
}

#[cfg(feature = "hiqlite-store")]
fn stored_peer_port(role: &str, address: &str) -> Result<u16, StoreError> {
    let port = if let Some(rest) = address.strip_prefix('[') {
        rest.rsplit_once("]:").map(|(_, port)| port)
    } else {
        address.rsplit_once(':').map(|(_, port)| port)
    };
    port.and_then(|port| port.parse().ok()).ok_or_else(|| {
        StoreError::Migration(format!(
            "membership.json contains an invalid {role} address; refusing to guess a listener port"
        ))
    })
}

#[cfg(feature = "hiqlite-store")]
async fn shutdown_voter(client: &Client, active_transport: bool) -> Result<(), StoreError> {
    const TLS_SHUTDOWN_ASSERTION: &str =
        "The global Hiqlite shutdown handler to always listen: SendError { .. }";

    if !active_transport {
        return client
            .shutdown()
            .await
            .map_err(|error| StoreError::Database(format!("stopping Hiqlite voter: {error}")));
    }

    // The assertion is caught below, but Rust's default panic hook would still
    // print a frightening backtrace during an otherwise clean daemon stop.
    // Serialize the short process-global hook swap and suppress only the exact
    // upstream assertion; every unrelated panic still reaches the prior hook.
    static PANIC_HOOK_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> =
        std::sync::OnceLock::new();
    let _hook_guard = PANIC_HOOK_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let previous_hook = Arc::new(std::panic::take_hook());
    let delegated_hook = Arc::clone(&previous_hook);
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str));
        if message == Some(TLS_SHUTDOWN_ASSERTION) {
            return;
        }
        delegated_hook(info);
    }));

    let owned = client.clone();
    let outcome = tokio::spawn(async move { owned.shutdown().await }).await;
    drop(std::panic::take_hook());
    let previous_hook = match Arc::try_unwrap(previous_hook) {
        Ok(hook) => hook,
        Err(_) => unreachable!("serialized panic-hook owner"),
    };
    std::panic::set_hook(previous_hook);

    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(StoreError::Database(format!(
            "stopping TLS Hiqlite voter: {error}"
        ))),
        Err(join_error) if join_error.is_panic() => {
            let payload = join_error.into_panic();
            let message = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("non-string panic");
            if message == TLS_SHUTDOWN_ASSERTION {
                // In 0.14 this exact assertion is after Raft shutdown, WAL
                // drain, SQL-writer drain, and client-stream shutdown. TLS
                // listeners own no receiver, so only their final notification
                // is missing; runtime teardown closes those listener tasks.
                tracing::debug!("contained Hiqlite 0.14 TLS listener shutdown assertion");
                Ok(())
            } else {
                Err(StoreError::Database(format!(
                    "TLS Hiqlite voter panicked during shutdown: {message}"
                )))
            }
        }
        Err(join_error) => Err(StoreError::Database(format!(
            "TLS Hiqlite voter shutdown task failed: {join_error}"
        ))),
    }
}

#[cfg(feature = "hiqlite-store")]
fn validate_cluster_listener(role: &str, bind: SocketAddr, tls: bool) -> Result<(), StoreError> {
    if !tls && !bind.ip().is_loopback() {
        return Err(StoreError::Migration(format!(
            "refusing public cleartext {role} bind {bind}; use the automatic cluster TLS \
             transport or bind the listener to loopback"
        )));
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
fn configured_local_peer(config: &Config, raft_id: u64) -> Result<ClusterPeer, StoreError> {
    let host = configured_advertise_host(config)?;
    Ok(ClusterPeer {
        raft_id,
        raft_address: host_port(&host, config.cluster.raft_bind.port()),
        api_address: host_port(&host, config.cluster.api_bind.port()),
    })
}

#[cfg(feature = "hiqlite-store")]
fn configured_advertise_host(config: &Config) -> Result<String, StoreError> {
    let host = if !config.cluster.advertise_host.trim().is_empty() {
        config.cluster.advertise_host.trim().to_owned()
    } else if !config.cluster.api_bind.ip().is_unspecified() {
        config.cluster.api_bind.ip().to_string()
    } else if !config.server.bind.ip().is_unspecified() {
        config.server.bind.ip().to_string()
    } else {
        "127.0.0.1".to_owned()
    };
    if host.contains("//") || host.contains('/') || host.contains(char::is_whitespace) {
        return Err(StoreError::Migration(
            "cluster.advertise_host must be a host or IP address, not a URL".to_owned(),
        ));
    }
    Ok(host)
}

#[cfg(feature = "hiqlite-store")]
fn configured_join_url(config: &Config) -> Result<String, StoreError> {
    let configured = config.cluster.join_url.trim().trim_end_matches('/');
    if !configured.is_empty() {
        let url = reqwest::Url::parse(configured).map_err(|_| {
            StoreError::Migration("cluster.join_url must be a valid http(s) URL".to_owned())
        })?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(StoreError::Migration(
                "cluster.join_url must be an http(s) URL without credentials, query, or fragment"
                    .to_owned(),
            ));
        }
        return Ok(configured.to_owned());
    }
    Ok(format!(
        "http://{}",
        host_port(
            &configured_advertise_host(config)?,
            config.server.bind.port()
        )
    ))
}

#[cfg(feature = "hiqlite-store")]
fn configured_artwork_url(config: &Config) -> Result<String, StoreError> {
    let configured = config.cluster.artwork_url.trim();
    if !configured.is_empty() {
        return super::membership::normalize_internal_http_base(configured).ok_or_else(|| {
            StoreError::Migration(
                "cluster.artwork_url must be an http(s) origin without credentials or a path"
                    .to_owned(),
            )
        });
    }
    Ok(format!(
        "http://{}",
        host_port(
            &configured_advertise_host(config)?,
            config.server.bind.port()
        )
    ))
}

#[cfg(feature = "hiqlite-store")]
fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(feature = "hiqlite-store")]
async fn abort_incoming(
    client: Client,
    incoming: &Path,
    error: StoreError,
    active_transport: bool,
) -> Result<SelectedStore, StoreError> {
    let shutdown = shutdown_voter(&client, active_transport).await.err();
    drop(client);
    let mut message = error.to_string();
    if let Some(shutdown) = shutdown {
        message.push_str(&format!(
            "; additionally failed to stop incoming voter: {shutdown}"
        ));
    }
    Err(cleanup_incoming_failure(
        incoming,
        StoreError::Migration(message),
    ))
}

#[cfg(feature = "hiqlite-store")]
fn cleanup_incoming_failure(incoming: &Path, error: StoreError) -> StoreError {
    let mut message = error.to_string();
    if let Err(cleanup) = remove_abandoned_incoming(
        incoming
            .parent()
            .expect("the incoming directory always has a data-dir parent"),
    ) {
        message.push_str(&format!(
            "; additionally failed to remove incoming target: {cleanup}"
        ));
    }
    StoreError::Migration(message)
}

#[cfg(feature = "hiqlite-store")]
fn activation_failure(data_dir: &Path, error: StoreError) -> StoreError {
    StoreError::Migration(format!(
        "{error}; SQLite source {} remains available; rollback command: plurxd run",
        data_dir.join(SQLITE_FILENAME).display()
    ))
}

#[cfg(feature = "hiqlite-store")]
fn ensure_sqlite_source(data_dir: &Path) -> Result<(), StoreError> {
    let source = data_dir.join(SQLITE_FILENAME);
    if !source.exists() {
        drop(SqliteStore::open(&source)?);
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
fn verify_legacy_ownership(
    backup: &Path,
    identity: &super::ClusterIdentity,
) -> Result<(), StoreError> {
    if identity.node_id == identity.cluster_id {
        return Ok(());
    }
    let connection = open_source(backup)?;
    let count: i64 = connection
        .query_row(
            "SELECT \
                (SELECT COUNT(*) FROM transcode_cache_locations WHERE node_id = ?1) + \
                (SELECT COUNT(*) FROM offline_packages WHERE node_id = ?1)",
            [&identity.cluster_id],
            |row| row.get(0),
        )
        .map_err(|error| {
            StoreError::Migration(format!("checking legacy byte ownership: {error}"))
        })?;
    if count > 0 {
        return Err(StoreError::Identity(format!(
            "node.id {} differs from instance.id {}, but {count} cache or offline row(s) still \
             use instance.id; refusing to strand owned bytes",
            identity.node_id, identity.cluster_id
        )));
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
fn inspect_sqlite_source(path: &Path) -> Result<(i64, String), StoreError> {
    let source = open_source(path)?;
    let schema_version = read_schema_version(&source)?;
    if schema_version > SQLITE_SCHEMA_VERSION {
        return Err(StoreError::Migration(format!(
            "source database schema is v{schema_version}, but this binary only knows \
             v{SQLITE_SCHEMA_VERSION}; refusing clustering import without changing {}",
            path.parent().unwrap_or_else(|| Path::new(".")).display()
        )));
    }
    let cluster_id = read_cluster_id(&source)?;
    Ok((schema_version, cluster_id))
}

// `config.cluster.advertise_host` has no reader while both listeners are
// loopback-only: there is no peer to advertise an address to. M3 reintroduces
// the resolution helper alongside the membership that needs it, rather than
// keeping an unreachable one here.
#[cfg(feature = "hiqlite-store")]
fn local_client_address(bind: SocketAddr) -> SocketAddr {
    let ip = match bind.ip() {
        IpAddr::V4(_) => IpAddr::from([127, 0, 0, 1]),
        IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
    };
    SocketAddr::new(ip, bind.port())
}

#[cfg(feature = "hiqlite-store")]
fn load_or_create_secret(data_dir: &Path, filename: &str) -> Result<String, StoreError> {
    let path = data_dir.join(filename);
    if path_exists(&path)? {
        return read_secret(&path);
    }

    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| StoreError::Migration(format!("generating {filename}: {error}")))?;
    let secret = hex::encode(bytes);
    let temporary = data_dir.join(format!(".{filename}.{}.tmp", uuid::Uuid::new_v4()));
    write_private_file(&temporary, format!("{secret}\n").as_bytes())?;
    match std::fs::hard_link(&temporary, &path) {
        Ok(()) => sync_directory(data_dir)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) =>
        {
            publish_secret_with_rename(data_dir, &temporary, &path)?;
        }
        Err(error) => return Err(migration_io("publishing", &path, error)),
    }
    remove_file_if_present(&temporary)?;
    read_secret(&path)
}

#[cfg(feature = "hiqlite-store")]
fn load_or_create_activity_signing_key(data_dir: &Path) -> Result<ActivitySigningKey, StoreError> {
    let seed = load_or_create_secret(data_dir, ACTIVITY_SIGNING_KEY_FILENAME)?;
    ActivitySigningKey::from_seed_hex(&seed)
        .map_err(|error| StoreError::Identity(error.to_string()))
}

#[cfg(feature = "hiqlite-store")]
fn read_existing_secrets(data_dir: &Path) -> Result<ClusterSecrets, StoreError> {
    Ok(ClusterSecrets {
        raft: read_secret(&data_dir.join(RAFT_SECRET_FILENAME))?,
        api: read_secret(&data_dir.join(API_SECRET_FILENAME))?,
    })
}

#[cfg(feature = "hiqlite-store")]
fn publish_secret_with_rename(
    data_dir: &Path,
    temporary: &Path,
    destination: &Path,
) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(destination) {
        Ok(placeholder) => {
            drop(placeholder);
            std::fs::rename(temporary, destination)
                .map_err(|error| migration_io("publishing", destination, error))?;
            sync_directory(data_dir)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(migration_io("reserving", destination, error)),
    }
}

#[cfg(feature = "hiqlite-store")]
fn read_secret(path: &Path) -> Result<String, StoreError> {
    let raw =
        std::fs::read_to_string(path).map_err(|error| migration_io("reading", path, error))?;
    let secret = raw.trim();
    if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::Migration(format!(
            "cluster secret {} is malformed",
            path.display()
        )));
    }
    Ok(secret.to_owned())
}

#[cfg(feature = "hiqlite-store")]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| migration_io("creating", path, error))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| migration_io("writing", path, error))
}

#[cfg(feature = "hiqlite-store")]
fn write_activation_attempt(data_dir: &Path) -> Result<(), StoreError> {
    let migration_dir = data_dir.join(MIGRATION_DIRNAME);
    std::fs::create_dir_all(&migration_dir)
        .map_err(|error| migration_io("creating", &migration_dir, error))?;
    sync_directory(data_dir)?;
    write_atomic_private(
        &migration_dir,
        ACTIVATION_ATTEMPT_FILENAME,
        b"one-voter activation in progress\n",
    )
}

#[cfg(feature = "hiqlite-store")]
fn activation_attempt_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join(MIGRATION_DIRNAME)
        .join(ACTIVATION_ATTEMPT_FILENAME)
}

#[cfg(feature = "hiqlite-store")]
fn remove_activation_attempt(data_dir: &Path) -> Result<(), StoreError> {
    let path = activation_attempt_path(data_dir);
    let existed = path_exists(&path)?;
    remove_file_if_present(&path)?;
    if existed {
        sync_directory(&data_dir.join(MIGRATION_DIRNAME))?;
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
fn write_activation_marker(target: &Path, marker: &ActivationMarker) -> Result<(), StoreError> {
    marker.validate()?;
    let mut bytes = serde_json::to_vec_pretty(marker).map_err(|error| {
        StoreError::Migration(format!("serializing Hiqlite activation marker: {error}"))
    })?;
    bytes.push(b'\n');
    write_atomic_private(target, ACTIVATION_MARKER_FILENAME, &bytes)
}

#[cfg(feature = "hiqlite-store")]
fn read_local_membership(data_dir: &Path) -> Result<Option<LocalMembership>, StoreError> {
    let path = data_dir.join(LOCAL_MEMBERSHIP_FILENAME);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(migration_io("reading", &path, error)),
    };
    let membership: LocalMembership = serde_json::from_slice(&bytes).map_err(|error| {
        StoreError::Identity(format!(
            "decoding local cluster membership {}: {error}",
            path.display()
        ))
    })?;
    // Version 1 is a voter, version 2 names its role. Reading a version 1
    // record is the upgrade path: `role` defaults to voter, which is what
    // every record written before this binary existed described. Anything
    // else — an unknown version, or a version that disagrees with the role
    // beside it — is refused rather than guessed at, because guessing wrong
    // starts a campaigning process on a node the cluster admitted as a
    // learner.
    if !local_membership_version_matches_role(membership.version, membership.role)
        || membership.cluster_id.trim().is_empty()
        || membership.node_id.trim().is_empty()
        || membership.raft_id == 0
    {
        return Err(StoreError::Identity(format!(
            "local cluster membership {} is incomplete",
            path.display()
        )));
    }
    Ok(Some(membership))
}

/// The role this data directory was admitted with.
///
/// For a process that has no Raft membership handle of its own — the
/// maintenance CLI attaches to a running voter as a remote client — this is
/// the only durable statement of what the node is. It is a boot record, not
/// live committed membership, so it may only be used to *refuse*: it can say
/// "this host was admitted as a learner", never "this host is a voter now".
///
/// A missing record has to be read carefully, because the answer this hands
/// back grants cluster-wide job authority. No record at all on a directory
/// that was never clustered is the honest single-node case and answers voter.
/// A missing record *beside an activation marker* is not: the directory is
/// clustered and the one file that says what it was admitted as is gone, so
/// answering "voter" would let a learner whose record was deleted grant itself
/// the cluster's artwork lease. That is refused instead.
#[cfg(feature = "hiqlite-store")]
pub fn local_cluster_role(data_dir: &Path) -> Result<ClusterRole, StoreError> {
    if let Some(membership) = read_local_membership(data_dir)? {
        return Ok(membership.role);
    }
    let marker = data_dir
        .join(HIQLITE_ACTIVE_DIRNAME)
        .join(ACTIVATION_MARKER_FILENAME);
    if path_exists(&marker)? {
        return Err(StoreError::Identity(format!(
            "{} records an activated cluster node but {} is missing, so this host's admitted \
             role cannot be established; restore it before running cluster-wide work here",
            marker.display(),
            data_dir.join(LOCAL_MEMBERSHIP_FILENAME).display()
        )));
    }
    Ok(ClusterRole::default())
}

#[cfg(feature = "hiqlite-store")]
fn write_local_membership(data_dir: &Path, membership: &LocalMembership) -> Result<(), StoreError> {
    let mut bytes = serde_json::to_vec_pretty(membership).map_err(|error| {
        StoreError::Identity(format!("serializing local cluster membership: {error}"))
    })?;
    bytes.push(b'\n');
    write_atomic_private(data_dir, LOCAL_MEMBERSHIP_FILENAME, &bytes)
}

/// Persist the target-local half of a committed learner promotion.
///
/// The caller may invoke this only after local Raft metrics contain this
/// node's vote. membership.json is written and fsynced first; activation.json
/// follows. A crash before the first rename leaves an ordinary learner, while
/// a crash between the two is completed by `open_active_store_with_key`
/// before the normal role-match check. Once both are voter/version 1, the
/// previous release's parser can open the directory again.
#[cfg(feature = "hiqlite-store")]
pub(crate) fn persist_promoted_voter_role(data_dir: &Path) -> Result<(), StoreError> {
    let active = data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    #[cfg(feature = "cluster-validation")]
    if !active.join(ACTIVATION_MARKER_FILENAME).exists() {
        // The separate-process Raft harness constructs MembershipManager
        // directly and deliberately has no daemon activation directory. Its
        // production-file contract is pinned by the migration tests beside
        // this helper; let the harness continue proving live membership.
        return Ok(());
    }
    let mut marker = read_activation_marker(&active)?;
    let mut membership = read_local_membership(data_dir)?.ok_or_else(|| {
        StoreError::Identity(format!(
            "cannot persist promoted voter role because {} is missing",
            data_dir.join(LOCAL_MEMBERSHIP_FILENAME).display()
        ))
    })?;
    if marker.cluster_id != membership.cluster_id {
        return Err(StoreError::Identity(
            "membership.json does not match activation.json during voter promotion".to_owned(),
        ));
    }
    match (membership.role, marker.admitted_role) {
        (ClusterRole::Voter, Some(ClusterRole::Voter)) => return Ok(()),
        (ClusterRole::Learner, Some(ClusterRole::Learner))
        | (ClusterRole::Voter, Some(ClusterRole::Learner)) => {}
        _ => {
            return Err(StoreError::Identity(
                "local cluster role records are not a resumable learner promotion".to_owned(),
            ));
        }
    }

    if membership.role == ClusterRole::Learner {
        membership.role = ClusterRole::Voter;
        membership.version = local_membership_version(ClusterRole::Voter);
        write_local_membership(data_dir, &membership)?;
        sync_directory(data_dir)?;
    }
    marker.admitted_role = Some(ClusterRole::Voter);
    write_activation_marker(&active, &marker)?;
    sync_directory(&active)?;
    sync_directory(data_dir)
}

#[cfg(feature = "hiqlite-store")]
fn write_readdress_record(data_dir: &Path, membership: &LocalMembership) -> Result<(), StoreError> {
    let record = ReaddressRecord {
        version: 1,
        membership: membership.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&record).map_err(|error| {
        StoreError::Migration(format!("serializing voter-readdress record: {error}"))
    })?;
    bytes.push(b'\n');
    write_atomic_private(data_dir, HIQLITE_READDRESS_MARKER_FILENAME, &bytes)
}

#[cfg(feature = "hiqlite-store")]
fn read_readdress_record(data_dir: &Path) -> Result<Option<ReaddressRecord>, StoreError> {
    let path = data_dir.join(HIQLITE_READDRESS_MARKER_FILENAME);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(migration_io("reading", &path, error)),
    };
    let record: ReaddressRecord = serde_json::from_slice(&bytes).map_err(|error| {
        StoreError::Migration(format!(
            "decoding voter-readdress record {}: {error}",
            path.display()
        ))
    })?;
    if record.version != 1
        || !local_membership_version_matches_role(record.membership.version, record.membership.role)
        || record.membership.cluster_id.trim().is_empty()
        || record.membership.node_id.trim().is_empty()
        || record.membership.raft_id == 0
    {
        return Err(StoreError::Migration(format!(
            "voter-readdress record {} is incomplete",
            path.display()
        )));
    }
    Ok(Some(record))
}

/// Recover the only non-atomic boundary in the one-voter directory swap.
#[cfg(feature = "hiqlite-store")]
fn recover_interrupted_readdress(data_dir: &Path) -> Result<(), StoreError> {
    let Some(record) = read_readdress_record(data_dir)? else {
        return Ok(());
    };
    let active = data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    let incoming = data_dir.join(HIQLITE_INCOMING_DIRNAME);
    let backup = data_dir.join(HIQLITE_READDRESS_BACKUP_DIRNAME);
    match (path_exists(&active)?, path_exists(&backup)?) {
        // Both renames committed. Publish the matching local record before the
        // replacement target is opened; cleanup waits for successful open.
        (true, true) => write_local_membership(data_dir, &record.membership),
        // The prepared target never replaced the authoritative directory.
        (true, false) => {
            remove_directory_if_present(&incoming)?;
            remove_file_if_present(&data_dir.join(HIQLITE_READDRESS_MARKER_FILENAME))?;
            sync_directory(data_dir)
        }
        // Crash between the two renames: restore the old authoritative target.
        (false, true) => {
            std::fs::rename(&backup, &active)
                .map_err(|error| migration_io("restoring pre-readdress target", &active, error))?;
            remove_directory_if_present(&incoming)?;
            remove_file_if_present(&data_dir.join(HIQLITE_READDRESS_MARKER_FILENAME))?;
            sync_directory(data_dir)
        }
        (false, false) => Err(StoreError::Migration(format!(
            "{} exists but neither the active nor recovery Hiqlite target exists",
            data_dir.join(HIQLITE_READDRESS_MARKER_FILENAME).display()
        ))),
    }
}

#[cfg(feature = "hiqlite-store")]
fn finish_readdress(data_dir: &Path) -> Result<(), StoreError> {
    if read_readdress_record(data_dir)?.is_none() {
        return Ok(());
    }
    remove_directory_if_present(&data_dir.join(HIQLITE_READDRESS_BACKUP_DIRNAME))?;
    remove_file_if_present(&data_dir.join(HIQLITE_READDRESS_MARKER_FILENAME))?;
    sync_directory(data_dir)
}

#[cfg(feature = "hiqlite-store")]
fn remove_directory_if_present(path: &Path) -> Result<(), StoreError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(migration_io("inspecting", path, error)),
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(StoreError::Migration(format!(
            "refusing to recursively remove non-directory recovery path {}",
            path.display()
        )));
    }
    std::fs::remove_dir_all(path).map_err(|error| migration_io("removing", path, error))
}

/// Write the already-activated breadcrumb unless it is already correct.
///
/// Fsynced through the same atomic path as the marker: a torn breadcrumb would
/// fail closed on the next boot and demand an operator decision for a directory
/// that is actually fine.
#[cfg(feature = "hiqlite-store")]
fn ensure_activated_source_record(
    data_dir: &Path,
    marker: &ActivationMarker,
) -> Result<(), StoreError> {
    let record = ActivatedSourceRecord {
        cluster_id: marker.cluster_id.clone(),
        source_backup_sha256: marker.source_backup_sha256.clone(),
    };
    if read_activated_source_record(data_dir)?.as_ref() == Some(&record) {
        return Ok(());
    }
    let mut bytes = serde_json::to_vec_pretty(&record).map_err(|error| {
        StoreError::Migration(format!("serializing the activated-source record: {error}"))
    })?;
    bytes.push(b'\n');
    write_atomic_private(data_dir, ACTIVATED_SOURCE_FILENAME, &bytes)
}

/// Read the already-activated breadcrumb; `None` means this directory never has.
///
/// A present but undecodable record is an error rather than a `None`: treating
/// it as absent would re-import the stale source, which is the exact loss the
/// record exists to prevent.
#[cfg(feature = "hiqlite-store")]
fn read_activated_source_record(
    data_dir: &Path,
) -> Result<Option<ActivatedSourceRecord>, StoreError> {
    let path = data_dir.join(ACTIVATED_SOURCE_FILENAME);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(migration_io("reading", &path, error)),
    };
    let record: ActivatedSourceRecord = serde_json::from_slice(&bytes).map_err(|error| {
        StoreError::Migration(format!(
            "decoding the activated-source record {}: {error}. It records that this \
             directory already handed authority to Hiqlite; repair or remove it \
             deliberately rather than letting a re-import discard replicated state",
            path.display()
        ))
    })?;
    Ok(Some(record))
}

#[cfg(feature = "hiqlite-store")]
fn read_activation_marker(target: &Path) -> Result<ActivationMarker, StoreError> {
    let path = target.join(ACTIVATION_MARKER_FILENAME);
    let bytes = std::fs::read(&path).map_err(|error| migration_io("reading", &path, error))?;
    let marker: ActivationMarker = serde_json::from_slice(&bytes).map_err(|error| {
        StoreError::Migration(format!(
            "decoding Hiqlite activation marker {}: {error}",
            path.display()
        ))
    })?;
    marker.validate()?;
    Ok(marker)
}

#[cfg(feature = "hiqlite-store")]
fn write_atomic_private(directory: &Path, filename: &str, bytes: &[u8]) -> Result<(), StoreError> {
    let destination = directory.join(filename);
    let temporary = directory.join(format!(".{filename}.{}.incoming", uuid::Uuid::new_v4()));
    write_private_file(&temporary, bytes)?;
    if let Err(error) = std::fs::rename(&temporary, &destination) {
        let _ = remove_file_if_present(&temporary);
        return Err(migration_io("publishing", &destination, error));
    }
    sync_directory(directory)
}

#[cfg(feature = "hiqlite-store")]
fn path_exists(path: &Path) -> Result<bool, StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(migration_io("inspecting", path, error)),
    }
}

#[cfg(feature = "hiqlite-store")]
fn require_real_directory(path: &Path) -> Result<(), StoreError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| migration_io("inspecting", path, error))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(StoreError::Migration(format!(
            "active Hiqlite target {} is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(feature = "hiqlite-store")]
fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Validate and durably snapshot the legacy SQLite source.
///
/// The daemon will call this only from `plurxd run`, before any listener or
/// background task starts. A future-schema database is refused before any
/// cleanup or backup write. SQLite's online-backup API is used instead of a
/// filesystem copy so committed pages still resident in `plurx.db-wal` are
/// included in the snapshot.
pub fn prepare_sqlite_import(data_dir: &Path) -> Result<PreparedSqliteImport, StoreError> {
    let source_path = data_dir.join(SQLITE_FILENAME);
    let source = open_source(&source_path)?;
    let schema_version = read_schema_version(&source)?;
    if schema_version > SQLITE_SCHEMA_VERSION {
        return Err(StoreError::Migration(format!(
            "source database schema is v{schema_version}, but this binary only knows \
             v{SQLITE_SCHEMA_VERSION}; refusing clustering import without changing {}",
            data_dir.display()
        )));
    }
    let cluster_id = read_cluster_id(&source)?;

    remove_abandoned_incoming(data_dir)?;

    let migration_dir = data_dir.join(MIGRATION_DIRNAME);
    std::fs::create_dir_all(&migration_dir)
        .map_err(|error| migration_io("creating", &migration_dir, error))?;
    sync_directory(data_dir)?;
    remove_abandoned_backup_temps(&migration_dir)?;

    let temporary_path = migration_dir.join(format!(
        ".{SQLITE_FILENAME}.{}.incoming",
        uuid::Uuid::new_v4()
    ));
    let prepared = create_backup(
        &source,
        &source_path,
        schema_version,
        &cluster_id,
        &migration_dir,
        &temporary_path,
    );
    if let Err(error) = prepared {
        return match remove_file_if_present(&temporary_path) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(StoreError::Migration(format!(
                "{error}; additionally failed to clean temporary backup: {cleanup}"
            ))),
        };
    }
    let prepared = prepared?;
    prune_migration_backups(&migration_dir, &prepared.backup_path)?;
    Ok(prepared)
}

/// Keep the newest few source backups and delete the rest.
///
/// Backups are content-addressed, so a repeated attempt against an unchanged
/// source reuses one file. But a failed activation leaves a recovery boot that
/// serves traffic and mutates `plurx.db`, so the next retry addresses different
/// content and writes another full-size copy. Unbounded, a persistently failing
/// activation fills the media server's data volume with complete database
/// copies. Retention is by modification time and never removes the backup this
/// attempt is about to import.
fn prune_migration_backups(migration_dir: &Path, keep: &Path) -> Result<(), StoreError> {
    let mut backups = Vec::new();
    for entry in std::fs::read_dir(migration_dir)
        .map_err(|error| migration_io("reading", migration_dir, error))?
    {
        let entry = entry.map_err(|error| migration_io("reading", migration_dir, error))?;
        let path = entry.path();
        if path == keep || path.extension().is_none_or(|ext| ext != "db") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map_err(|error| migration_io("reading", &path, error))?;
        backups.push((modified, path));
    }
    if backups.len() < MIGRATION_BACKUP_RETENTION {
        return Ok(());
    }
    // Newest first, so the tail past the retained window is what goes. `keep`
    // is excluded above rather than counted, so retention is "this attempt's
    // backup plus the previous MIGRATION_BACKUP_RETENTION - 1".
    backups.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in backups.drain(MIGRATION_BACKUP_RETENTION - 1..) {
        remove_file_if_present(&path)?;
    }
    sync_directory(migration_dir)
}

fn open_source(path: &Path) -> Result<Connection, StoreError> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        StoreError::Migration(format!(
            "opening SQLite import source {}: {error}",
            path.display()
        ))
    })
}

fn read_schema_version(connection: &Connection) -> Result<i64, StoreError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| StoreError::Migration(format!("reading source schema version: {error}")))
}

fn read_cluster_id(connection: &Connection) -> Result<String, StoreError> {
    connection
        .query_row(
            "SELECT value FROM settings WHERE key = 'instance.id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| StoreError::Migration(format!("reading source instance.id: {error}")))
        .and_then(|cluster_id| {
            if cluster_id.trim().is_empty() {
                Err(StoreError::Migration(
                    "source instance.id is empty".to_owned(),
                ))
            } else {
                Ok(cluster_id)
            }
        })
}

fn remove_abandoned_incoming(data_dir: &Path) -> Result<(), StoreError> {
    let incoming = data_dir.join(HIQLITE_INCOMING_DIRNAME);
    let metadata = match std::fs::symlink_metadata(&incoming) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(migration_io("inspecting", &incoming, error)),
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(&incoming)
            .map_err(|error| migration_io("removing abandoned", &incoming, error))?;
    } else {
        std::fs::remove_file(&incoming)
            .map_err(|error| migration_io("removing abandoned", &incoming, error))?;
    }
    sync_directory(data_dir)
}

fn remove_abandoned_backup_temps(migration_dir: &Path) -> Result<(), StoreError> {
    let mut removed = false;
    let entries = std::fs::read_dir(migration_dir)
        .map_err(|error| migration_io("reading", migration_dir, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| migration_io("reading", migration_dir, error))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_backup_temp_artifact(&name) {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| migration_io("inspecting", &path, error))?;
            if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
                return Err(StoreError::Migration(format!(
                    "refusing to recursively remove unexpected backup staging directory {}",
                    path.display()
                )));
            }
            std::fs::remove_file(&path)
                .map_err(|error| migration_io("removing abandoned", &path, error))?;
            removed = true;
        }
    }
    if removed {
        sync_directory(migration_dir)?;
    }
    Ok(())
}

fn is_backup_temp_artifact(name: &str) -> bool {
    let Some(remainder) = name.strip_prefix(&format!(".{SQLITE_FILENAME}.")) else {
        return false;
    };
    let Some((identifier, suffix)) = remainder.split_once(".incoming") else {
        return false;
    };
    !identifier.is_empty() && matches!(suffix, "" | "-wal" | "-shm")
}

fn create_backup(
    source: &Connection,
    source_path: &Path,
    schema_version: i64,
    cluster_id: &str,
    migration_dir: &Path,
    temporary_path: &Path,
) -> Result<PreparedSqliteImport, StoreError> {
    create_private_file(temporary_path)?;
    let mut destination = Connection::open(temporary_path).map_err(|error| {
        StoreError::Migration(format!(
            "opening temporary SQLite backup {}: {error}",
            temporary_path.display()
        ))
    })?;
    {
        let backup = Backup::new(source, &mut destination).map_err(|error| {
            StoreError::Migration(format!("starting SQLite online backup: {error}"))
        })?;
        backup
            .run_to_completion(256, Duration::from_millis(10), None)
            .map_err(|error| {
                StoreError::Migration(format!("copying SQLite import source: {error}"))
            })?;
    }
    destination
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(|error| {
            StoreError::Migration(format!(
                "canonicalizing SQLite backup journal mode: {error}"
            ))
        })?;
    let copied_version = read_schema_version(&destination)?;
    if copied_version != schema_version {
        return Err(StoreError::Migration(format!(
            "SQLite backup schema changed during import preparation: \
             source v{schema_version}, backup v{copied_version}"
        )));
    }
    let integrity: String = destination
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| StoreError::Migration(format!("checking SQLite backup: {error}")))?;
    if integrity != "ok" {
        return Err(StoreError::Migration(format!(
            "SQLite backup failed quick_check: {integrity}"
        )));
    }
    drop(destination);

    File::open(temporary_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| migration_io("syncing", temporary_path, error))?;
    let backup_sha256 = sha256_file(temporary_path)?;
    let backup_path = migration_dir.join(format!("plurx-v{schema_version}-{backup_sha256}.db"));

    if backup_path.exists() {
        let existing_sha256 = sha256_file(&backup_path)?;
        if existing_sha256 != backup_sha256 {
            return Err(StoreError::Migration(format!(
                "existing migration backup {} does not match its content-addressed name",
                backup_path.display()
            )));
        }
        remove_file_if_present(temporary_path)?;
    } else {
        match std::fs::rename(temporary_path, &backup_path) {
            Ok(()) => {}
            Err(error) if backup_path.exists() => {
                let existing_sha256 = sha256_file(&backup_path)?;
                if existing_sha256 != backup_sha256 {
                    return Err(migration_io("publishing", &backup_path, error));
                }
                remove_file_if_present(temporary_path)?;
            }
            Err(error) => return Err(migration_io("publishing", &backup_path, error)),
        }
        sync_directory(migration_dir)?;
    }

    Ok(PreparedSqliteImport {
        source_path: source_path.to_owned(),
        backup_path,
        backup_sha256,
        schema_version,
        cluster_id: cluster_id.to_owned(),
    })
}

fn create_private_file(path: &Path) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| migration_io("creating", path, error))?;
    file.write_all(&[])
        .map_err(|error| migration_io("initializing", path, error))
}

fn sha256_file(path: &Path) -> Result<String, StoreError> {
    let mut file = File::open(path).map_err(|error| migration_io("opening", path, error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| migration_io("hashing", path, error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn remove_file_if_present(path: &Path) -> Result<(), StoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(migration_io("removing temporary", path, error)),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| migration_io("syncing directory", path, error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

fn migration_io(action: &str, path: &Path, error: std::io::Error) -> StoreError {
    StoreError::Migration(format!("{action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn join_url_preserves_reverse_proxy_prefix_but_peer_url_requires_an_origin() {
        let mut config = Config::default();
        config.cluster.join_url = "https://cluster.example/plurx/".to_owned();
        assert_eq!(
            configured_join_url(&config).expect("path-prefixed join coordinator"),
            "https://cluster.example/plurx"
        );

        config.cluster.artwork_url = config.cluster.join_url.clone();
        assert!(
            configured_artwork_url(&config).is_err(),
            "peer routes must not inherit a reverse-proxy path prefix"
        );

        for invalid in [
            "https://user:secret@cluster.example/plurx",
            "https://cluster.example/plurx?redirect=elsewhere",
            "file:///tmp/plurx.sock",
        ] {
            config.cluster.join_url = invalid.to_owned();
            assert!(configured_join_url(&config).is_err(), "accepted {invalid}");
        }
    }
    use crate::store::{SettingsStore, SqliteStore};

    #[cfg(feature = "hiqlite-store")]
    use axum::extract::State;
    #[cfg(feature = "hiqlite-store")]
    use axum::http::StatusCode;
    #[cfg(feature = "hiqlite-store")]
    use axum::response::{IntoResponse, Response};
    #[cfg(feature = "hiqlite-store")]
    use axum::routing::post;
    #[cfg(feature = "hiqlite-store")]
    use axum::{Json, Router};
    #[cfg(feature = "hiqlite-store")]
    use serde_json::json;

    /// The claim a data-directory refusal is entitled to make. Nothing that did
    /// not observe a *different* live process may print it.
    #[cfg(feature = "hiqlite-store")]
    const SECOND_DAEMON_VERDICT: &str = "another plurxd process already owns the data directory";

    /// Raft ids are durable identities, not positions in the bootstrap vector.
    /// A live token can reserve id 2 while id 3 joins first, so Hiqlite must
    /// accept the unique sparse roster rather than requiring a duplicate pad.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn hiqlite_accepts_a_unique_sparse_node_roster() {
        let peers = [
            ClusterPeer {
                raft_id: 1,
                raft_address: "127.0.0.1:32401".to_owned(),
                api_address: "127.0.0.1:32402".to_owned(),
            },
            ClusterPeer {
                raft_id: 3,
                raft_address: "127.0.0.1:32501".to_owned(),
                api_address: "127.0.0.1:32502".to_owned(),
            },
        ];
        let nodes = hiqlite_nodes_for_voter(&peers, 3).expect("build sparse connection roster");
        let config = NodeConfig {
            node_id: 3,
            nodes,
            secret_raft: "0123456789abcdef".to_owned(),
            secret_api: "fedcba9876543210".to_owned(),
            ..NodeConfig::default()
        };

        config
            .is_valid()
            .expect("sparse Raft ids must be selected by id");
        assert_eq!(
            config.nodes.iter().map(|node| node.id).collect::<Vec<_>>(),
            vec![1, 3],
            "the compatibility path must not duplicate a connection target"
        );
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn production_timing_admits_recovery_after_upgrade_and_clean_rolling_restarts() {
        let mut config = Config::default();
        config.cluster.read_pool_size = 16;
        let defaults = production_hiqlite_defaults(&config);
        assert_eq!(defaults.read_pool_size, 16);
        assert_eq!(defaults.wal_size, HIQLITE_WAL_SIZE_BYTES);
        assert_eq!(defaults.raft_config.heartbeat_interval, 800);
        assert_eq!(defaults.raft_config.election_timeout_min, 2_400);
        assert_eq!(defaults.raft_config.election_timeout_max, 4_000);
        assert!(
            defaults.raft_config.election_timeout_min
                >= defaults.raft_config.heartbeat_interval * 3
        );
        assert!(
            defaults.raft_config.heartbeat_interval * 3 / 2 < 1_500,
            "a new leader must beat the previous release's election floor"
        );
        let vote_soft_ttl = defaults.raft_config.election_timeout_min * 3 / 4;
        assert!(
            u128::from(defaults.raft_config.election_timeout_max + vote_soft_ttl)
                < hiqlite::LEADER_DISCOVERY_TIMEOUT.as_millis(),
            "detached leader discovery must outlive the longest election and vote round"
        );
        assert_eq!(
            hiqlite::LEADER_DISCOVERY_TIMEOUT + hiqlite::LEADER_STREAM_HANDOFF_TIMEOUT,
            hiqlite::LEADER_RETRY_RECOVERY_TIMEOUT,
            "the recovery bound must include discovery and an acknowledged replacement stream"
        );
        assert!(
            hiqlite::LEADER_STREAM_CONNECT_TIMEOUT < hiqlite::LEADER_STREAM_HANDOFF_TIMEOUT,
            "the end-to-end handoff must leave time around one connection attempt"
        );
        assert!(
            hiqlite::LEADER_RETRY_RECOVERY_TIMEOUT
                + Duration::from_millis(defaults.raft_config.heartbeat_interval)
                < REPLICATED_LEADER_RECOVERY_BUDGET,
            "leader discovery, stream reconnect, and one AppendEntries round must fit the recovery budget"
        );
        assert_eq!(defaults.raft_config.max_in_snapshot_log_to_keep, 1);
        assert_eq!(defaults.raft_config.purge_batch_size, 1);
        assert_eq!(defaults.raft_config.install_snapshot_timeout, 120_000);
        assert!(
            format!("{:?}", defaults.raft_config.snapshot_policy).contains("10000"),
            "snapshot policy must stay at 10,000 entries"
        );
        assert_eq!(
            format!("{:?}", defaults.wal_sync),
            format!("{:?}", NodeConfig::default().wal_sync),
            "read-pool tuning must not change immediate-async WAL sync"
        );

        config.cluster.install_snapshot_timeout_secs = 300;
        let tuned = production_hiqlite_defaults(&config);
        assert_eq!(tuned.raft_config.install_snapshot_timeout, 300_000);
    }

    /// Once the active voter exists, every later initialization error must
    /// drain it before returning. Otherwise the process exits with Hiqlite's
    /// state-machine crash sentinel present and the next boot destroys the
    /// local state machine to rebuild it from peers.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_post_start_initialization_failure_leaves_no_crash_sentinel() {
        install_default_crypto_provider();

        let dir = tempfile::tempdir().expect("post-start failure data dir");
        let config = membership_test_config(dir.path());
        drop(SqliteStore::open(&dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
        write_private_file(
            &dir.path().join(ACTIVITY_SIGNING_KEY_FILENAME),
            b"not-hexadecimal\n",
        )
        .expect("write invalid activity key");

        let error = select_daemon_store(&config)
            .await
            .err()
            .expect("the invalid post-start key must refuse activation")
            .to_string();
        assert!(
            error.contains(ACTIVITY_SIGNING_KEY_FILENAME) && error.contains("is malformed"),
            "the refusal must occur after the voter starts: {error}"
        );
        assert!(
            !dir.path()
                .join(HIQLITE_ACTIVE_DIRNAME)
                .join("state_machine")
                .join("lock")
                .exists(),
            "a handled initialization error must not look like a process crash"
        );
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn activation_marker_accepts_only_the_supported_upgrade_window() {
        let marker = |replicated_schema_version| ActivationMarker {
            marker_version: ACTIVATION_MARKER_VERSION,
            cluster_id: "cluster-a".to_owned(),
            source_backup_sha256: "0".repeat(64),
            source_schema_version: 20,
            replicated_schema_version,
            imported_rows: 0,
            table_hashes: vec![SqliteImportTableDigest {
                table: "settings".to_owned(),
                row_count: 0,
                sha256: "0".repeat(64),
            }],
            admitted_role: None,
        };
        marker(AUTH_SCHEMA_MIGRATION_SOURCE)
            .validate()
            .expect("v5 marker must reach the complete daemon migration chain");
        marker(AUTH_SCHEMA_MIGRATION_SOURCE + 1)
            .validate()
            .expect("v6 marker must reach the book-facts migration");
        marker(AUTH_SCHEMA_VERSION)
            .validate()
            .expect("current marker");
        for unsupported in [AUTH_SCHEMA_MIGRATION_SOURCE - 1, AUTH_SCHEMA_VERSION + 1] {
            let error = marker(unsupported)
                .validate()
                .expect_err("unsupported marker must fail before voter startup");
            assert!(error.to_string().contains("incomplete"), "{error}");
        }

        // Version 0 has never been a valid writer-produced marker format.
        {
            let zero = ActivationMarker {
                marker_version: 0,
                cluster_id: "cluster-a".to_owned(),
                source_backup_sha256: "0".repeat(64),
                source_schema_version: 20,
                replicated_schema_version: AUTH_SCHEMA_VERSION,
                imported_rows: 0,
                table_hashes: vec![SqliteImportTableDigest {
                    table: "settings".to_owned(),
                    row_count: 0,
                    sha256: "0".repeat(64),
                }],
                admitted_role: None,
            };
            let error = zero
                .validate()
                .expect_err("marker version 0 must fail closed");
            assert!(error.to_string().contains("unsupported"), "{error}");
        }
    }

    /// A predecessor still closing its handle is not a second daemon.
    ///
    /// Regression for #374. `acquire_daemon_lock` took the advisory lock
    /// non-blockingly, so
    /// `daemon_join_refuses_occupied_and_expired_targets_then_resumes_finalization`
    /// re-activating the same data directory printed [`SECOND_DAEMON_VERDICT`]
    /// whenever the previous activation's handle had not finished dropping.
    /// Unloaded the drop won that race; under gate-parallel CI load it did not,
    /// and `rust-gate` failed on diffs that cannot reach this code — a load
    /// artifact reported as a production-grade double-start.
    ///
    /// Exercises the real production entry point and its real window, because
    /// the window is the correction. Reverting to a single non-blocking attempt
    /// fails here immediately.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test]
    async fn a_still_closing_predecessor_lock_is_not_reported_as_a_second_daemon() {
        let data = tempfile::tempdir().expect("daemon lock dir");
        let holder = acquire_daemon_lock(data.path())
            .await
            .expect("the first owner takes the lock");

        // Well inside DAEMON_LOCK_ACQUIRE_WINDOW, and the whole point of it:
        // the incoming owner cannot know this handle is about to close.
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            drop(holder);
        });

        let acquired = acquire_daemon_lock(data.path()).await;
        release.await.expect("predecessor release task");
        let error = match acquired {
            Ok(_lock) => return,
            Err(error) => error,
        };
        panic!(
            "a predecessor that was still closing its handle was refused: {error}. The \
             data-directory lock must wait out a departing owner rather than read a \
             not-yet-closed handle as a second running daemon"
        );
    }

    /// The refusal survives, and it does not conflate the two conditions.
    ///
    /// The owner here is this test process, so the lock is genuinely held for
    /// the whole window and must still be refused — but calling that "another
    /// plurxd process" would send a reader hunting a process that does not
    /// exist. A short explicit window keeps this from costing five seconds;
    /// the production window is exercised by the regression above.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test]
    async fn a_lock_held_for_the_whole_window_is_refused_without_blaming_another_process() {
        let data = tempfile::tempdir().expect("daemon lock dir");
        let _holder = acquire_daemon_lock(data.path())
            .await
            .expect("the owner takes the lock");
        assert_eq!(
            read_daemon_lock_holder(&data.path().join(DAEMON_LOCK_FILENAME)),
            DaemonLockHolder::ThisProcess(std::process::id()),
            "an owner must record itself so a contender's refusal can name it"
        );

        let message = acquire_daemon_lock_within(data.path(), Duration::from_millis(200))
            .await
            .expect_err("a lock held for the whole window must still be refused")
            .to_string();
        assert!(
            !message.contains(SECOND_DAEMON_VERDICT),
            "our own still-open handle was reported as a second daemon: {message}"
        );
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "a refusal must name the recorded holder: {message}"
        );
    }

    /// A genuinely different owner keeps the operator-facing verdict.
    ///
    /// `crates/plurxd/tests/cluster_activation.rs` proves this end to end with
    /// two real processes; this pins the text and the classification so the
    /// #374 correction cannot quietly stop calling a real double-start what it
    /// is. An unrecorded owner is treated the same way: the lock is held and
    /// nothing says it is ours.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn a_different_owner_is_still_reported_as_a_second_daemon() {
        let data = tempfile::tempdir().expect("daemon lock dir");
        let path = data.path().join(DAEMON_LOCK_FILENAME);

        let foreign = std::process::id().wrapping_add(1);
        std::fs::write(&path, format!("{foreign}\n")).expect("record a foreign owner");
        assert_eq!(
            read_daemon_lock_holder(&path),
            DaemonLockHolder::OtherProcess(foreign)
        );
        std::fs::write(&path, "not-a-pid").expect("record an unusable owner");
        assert_eq!(
            read_daemon_lock_holder(&path),
            DaemonLockHolder::Unidentified
        );
        assert_eq!(
            read_daemon_lock_holder(&data.path().join("absent.lock")),
            DaemonLockHolder::Unidentified,
            "a missing record must degrade to a message, never to a startup error"
        );

        for holder in [
            DaemonLockHolder::OtherProcess(foreign),
            DaemonLockHolder::Unidentified,
        ] {
            let message =
                daemon_lock_conflict(data.path(), &holder, DAEMON_LOCK_ACQUIRE_WINDOW).to_string();
            assert!(
                message.contains(SECOND_DAEMON_VERDICT),
                "{holder:?} must keep the double-start verdict: {message}"
            );
        }
    }

    /// Staging import never needs a remotely reachable address. Active M3
    /// voters and maintenance clients use persisted membership addresses.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn staging_fallback_addresses_are_always_loopback() {
        let configured_v4: SocketAddr = "192.0.2.40:32401".parse().expect("IPv4 bind");
        assert_eq!(
            local_client_address(configured_v4),
            "127.0.0.1:32401".parse().expect("IPv4 loopback")
        );

        let configured_v6: SocketAddr = "[2001:db8::40]:32402".parse().expect("IPv6 bind");
        assert_eq!(
            local_client_address(configured_v6),
            "[::1]:32402".parse().expect("IPv6 loopback")
        );
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn maintenance_uses_the_persisted_local_api_address() {
        let data = tempfile::tempdir().expect("maintenance address dir");
        let mut config = Config::default();
        config.storage.data_dir = data.path().to_owned();
        let identity = super::super::ClusterIdentity {
            cluster_id: "cluster-a".to_owned(),
            node_id: "node-a".to_owned(),
            raft_id: 1,
        };
        write_local_membership(
            data.path(),
            &LocalMembership {
                version: 1,
                cluster_id: identity.cluster_id.clone(),
                node_id: identity.node_id.clone(),
                raft_id: identity.raft_id,
                local: ClusterPeer {
                    raft_id: 1,
                    raft_address: "192.0.2.40:32401".to_owned(),
                    api_address: "192.0.2.40:32402".to_owned(),
                },
                bootstrap: Vec::new(),
                join_token_digest: None,
                role: ClusterRole::Voter,
            },
        )
        .expect("write local membership");
        assert_eq!(
            local_voter_api_address(&config, &identity).expect("maintenance address"),
            "192.0.2.40:32402"
        );
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn public_cleartext_cluster_listeners_are_refused() {
        let public: SocketAddr = "0.0.0.0:32401".parse().expect("public bind");
        let error = validate_cluster_listener("raft", public, false)
            .expect_err("public cleartext must be refused");
        assert!(error.to_string().contains("refusing public cleartext raft"));
        validate_cluster_listener("raft", public, true).expect("public TLS is allowed");
        validate_cluster_listener("raft", "127.0.0.1:32401".parse().expect("loopback"), false)
            .expect("loopback cleartext is allowed for staging");
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn default_voters_stay_loopback_and_joined_port_drift_is_refused() {
        let config = Config::default();
        assert!(should_force_loopback(&config, None));

        let mut enabled = config.clone();
        enabled.cluster.advertise_host = "plurx-a.lan".to_owned();
        assert!(!should_force_loopback(&enabled, None));

        let membership = LocalMembership {
            version: 1,
            cluster_id: "cluster-a".to_owned(),
            node_id: "node-a".to_owned(),
            raft_id: 1,
            local: ClusterPeer {
                raft_id: 1,
                raft_address: "plurx-a.lan:32401".to_owned(),
                api_address: "plurx-a.lan:32402".to_owned(),
            },
            bootstrap: Vec::new(),
            join_token_digest: None,
            role: ClusterRole::Voter,
        };
        let mut drifted = enabled;
        drifted.cluster.api_bind.set_port(32502);
        let error = configured_or_persisted_local_peer(&drifted, &membership, false)
            .expect_err("joined listener port drift must fail closed");
        assert!(error.to_string().contains("listener port drift"), "{error}");
    }

    /// Build an activated data directory whose persisted membership is
    /// `membership`, so readdress decisions can be exercised without a store.
    #[cfg(feature = "hiqlite-store")]
    fn activated_dir_for_readdress(membership: &LocalMembership) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("activated data dir");
        let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
        std::fs::create_dir_all(&active).expect("active target");
        write_activation_marker(
            &active,
            &ActivationMarker {
                marker_version: ACTIVATION_MARKER_VERSION,
                cluster_id: membership.cluster_id.clone(),
                source_backup_sha256: "0".repeat(64),
                source_schema_version: AUTH_SCHEMA_VERSION,
                replicated_schema_version: AUTH_SCHEMA_VERSION,
                imported_rows: 0,
                table_hashes: vec![SqliteImportTableDigest {
                    table: "settings".to_owned(),
                    row_count: 0,
                    sha256: "0".repeat(64),
                }],
                admitted_role: Some(membership.role),
            },
        )
        .expect("activation marker");
        std::fs::write(
            dir.path().join("node.id"),
            format!("{}\n", membership.node_id),
        )
        .expect("node identity");
        write_local_membership(dir.path(), membership).expect("persisted membership");
        dir
    }

    /// A loopback-literal `advertise_host` is a legitimate configuration - two
    /// daemons on one host - and must reach a fixed point. Deciding on
    /// loopback-ness instead of on "differs from configured" never settles, so
    /// a sole voter rebuilt its state machine on every boot and a joined
    /// follower refused to start at all.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn readdress_is_a_no_op_once_the_committed_address_matches_configuration() {
        for (raft_id, bootstrap_raft_id) in [(1_u64, 1_u64), (2, 1)] {
            let dir = tempfile::tempdir().expect("port probe dir");
            let config_probe = membership_test_config(dir.path());
            let raft_port = config_probe.cluster.raft_bind.port();
            let api_port = config_probe.cluster.api_bind.port();
            let local = ClusterPeer {
                raft_id,
                raft_address: format!("127.0.0.1:{raft_port}"),
                api_address: format!("127.0.0.1:{api_port}"),
            };
            let membership = LocalMembership {
                version: 1,
                cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
                node_id: "22222222-2222-4222-8222-222222222222".to_owned(),
                raft_id,
                local: local.clone(),
                // A joined follower carries the coordinator's bootstrap list,
                // which is exactly what the old peer-existence guard rejected.
                bootstrap: vec![ClusterPeer {
                    raft_id: bootstrap_raft_id,
                    raft_address: format!("127.0.0.1:{raft_port}"),
                    api_address: format!("127.0.0.1:{api_port}"),
                }],
                join_token_digest: None,
                role: ClusterRole::Voter,
            };
            let activated = activated_dir_for_readdress(&membership);
            let mut config = membership_test_config(activated.path());
            config.cluster.advertise_host = "127.0.0.1".to_owned();
            config.cluster.raft_bind = format!("127.0.0.1:{raft_port}").parse().expect("Raft bind");
            config.cluster.api_bind = format!("127.0.0.1:{api_port}")
                .parse()
                .expect("cluster API bind");

            readdress_single_voter_if_needed(&config)
                .unwrap_or_else(|error| panic!("raft_id {raft_id} must boot unchanged: {error}"));

            assert_eq!(
                read_local_membership(activated.path())
                    .expect("read membership")
                    .map(|membership| membership.local),
                Some(local),
                "raft_id {raft_id} committed address was rewritten"
            );
            assert!(
                !activated
                    .path()
                    .join(HIQLITE_READDRESS_BACKUP_DIRNAME)
                    .exists()
                    && !activated.path().join(HIQLITE_INCOMING_DIRNAME).exists(),
                "raft_id {raft_id} rebuilt its state machine with nothing to repair"
            );
        }
    }

    /// `membership.json` records the bootstrap list once and is never
    /// refreshed, so a coordinator that has admitted a peer still reads as a
    /// lone voter there. Deciding sole-voter-ness from that file let the
    /// readdress rebuild one-node Raft metadata that silently ejected the peer.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn readdress_refuses_once_a_peer_is_admitted_even_if_local_membership_is_stale() {
        let membership = LocalMembership {
            version: 1,
            cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            node_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            raft_id: 1,
            local: ClusterPeer {
                raft_id: 1,
                raft_address: "127.0.0.1:32401".to_owned(),
                api_address: "127.0.0.1:32402".to_owned(),
            },
            // Written before the peer joined, and never updated since.
            bootstrap: vec![ClusterPeer {
                raft_id: 1,
                raft_address: "127.0.0.1:32401".to_owned(),
                api_address: "127.0.0.1:32402".to_owned(),
            }],
            join_token_digest: None,
            role: ClusterRole::Voter,
        };
        let activated = activated_dir_for_readdress(&membership);
        let database = activated
            .path()
            .join(HIQLITE_ACTIVE_DIRNAME)
            .join("state_machine")
            .join("db")
            .join(HIQLITE_DATABASE_FILENAME);
        std::fs::create_dir_all(database.parent().expect("state machine dir"))
            .expect("state machine dir");
        let connection = Connection::open(&database).expect("state machine");
        connection
            .execute_batch(
                "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
                 raft_address TEXT NOT NULL, api_address TEXT NOT NULL, \
                 last_seen_at INTEGER NOT NULL, removed_at INTEGER);\
                 INSERT INTO cluster_nodes VALUES \
                   ('node-a', 1, '127.0.0.1:32401', '127.0.0.1:32402', 1, NULL),\
                   ('node-b', 2, '127.0.0.1:32411', '127.0.0.1:32412', 1, NULL);",
            )
            .expect("admitted peer record");
        drop(connection);

        let mut config = membership_test_config(activated.path());
        config.cluster.advertise_host = "plurx-a.lan".to_owned();
        config.cluster.raft_bind = "127.0.0.1:32401".parse().expect("Raft bind");
        config.cluster.api_bind = "127.0.0.1:32402".parse().expect("cluster API bind");

        let error = readdress_single_voter_if_needed(&config)
            .expect_err("readdress must refuse once a peer is admitted");
        assert!(
            error
                .to_string()
                .contains("cannot change after another voter or learner exists"),
            "{error}"
        );
        assert!(
            !activated
                .path()
                .join(HIQLITE_READDRESS_BACKUP_DIRNAME)
                .exists()
                && !activated.path().join(HIQLITE_INCOMING_DIRNAME).exists(),
            "refused readdress still touched the active target"
        );
    }

    /// A removed node is tombstoned rather than deleted, so it must stop
    /// blocking a sole survivor's readdress while still reserving its Raft id.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn admitted_peer_count_ignores_tombstones_and_missing_schema() {
        let dir = tempfile::tempdir().expect("state machine dir");
        let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
        let database = active
            .join("state_machine")
            .join("db")
            .join(HIQLITE_DATABASE_FILENAME);
        std::fs::create_dir_all(database.parent().expect("db dir")).expect("db dir");

        // A store activated before the membership schema existed never admitted
        // a peer, and an absent state machine cannot contradict that either.
        assert_eq!(admitted_peer_count(&active, 1).expect("absent database"), 0);
        let connection = Connection::open(&database).expect("state machine");
        assert_eq!(admitted_peer_count(&active, 1).expect("absent table"), 0);
        connection
            .execute_batch(
                "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
                 raft_address TEXT NOT NULL, api_address TEXT NOT NULL, \
                 last_seen_at INTEGER NOT NULL, removed_at INTEGER);\
                 INSERT INTO cluster_nodes VALUES \
                   ('node-a', 1, 'a:1', 'a:2', 1, NULL),\
                   ('node-b', 2, 'b:1', 'b:2', 1, 99);",
            )
            .expect("tombstoned peer");
        drop(connection);

        assert_eq!(admitted_peer_count(&active, 1).expect("tombstoned peer"), 0);
        assert_eq!(admitted_peer_count(&active, 2).expect("live peer"), 1);
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn readdress_directory_swap_recovers_both_crash_boundaries() {
        let membership = LocalMembership {
            version: 1,
            cluster_id: "cluster-a".to_owned(),
            node_id: "node-a".to_owned(),
            raft_id: 1,
            local: ClusterPeer {
                raft_id: 1,
                raft_address: "plurx-a.lan:32401".to_owned(),
                api_address: "plurx-a.lan:32402".to_owned(),
            },
            bootstrap: Vec::new(),
            join_token_digest: None,
            role: ClusterRole::Voter,
        };

        let between = tempfile::tempdir().expect("between-renames recovery dir");
        std::fs::create_dir(between.path().join(HIQLITE_READDRESS_BACKUP_DIRNAME))
            .expect("old target");
        std::fs::create_dir(between.path().join(HIQLITE_INCOMING_DIRNAME))
            .expect("prepared target");
        write_readdress_record(between.path(), &membership).expect("readdress marker");
        recover_interrupted_readdress(between.path()).expect("restore old target");
        assert!(between.path().join(HIQLITE_ACTIVE_DIRNAME).exists());
        assert!(!between
            .path()
            .join(HIQLITE_READDRESS_BACKUP_DIRNAME)
            .exists());
        assert!(!between.path().join(HIQLITE_INCOMING_DIRNAME).exists());
        assert!(!between
            .path()
            .join(HIQLITE_READDRESS_MARKER_FILENAME)
            .exists());

        let after = tempfile::tempdir().expect("after-renames recovery dir");
        std::fs::create_dir(after.path().join(HIQLITE_ACTIVE_DIRNAME)).expect("new target");
        std::fs::create_dir(after.path().join(HIQLITE_READDRESS_BACKUP_DIRNAME))
            .expect("old recovery target");
        write_readdress_record(after.path(), &membership).expect("readdress marker");
        recover_interrupted_readdress(after.path()).expect("publish new membership");
        assert_eq!(
            read_local_membership(after.path()).expect("read membership"),
            Some(membership)
        );
        assert!(after.path().join(HIQLITE_READDRESS_BACKUP_DIRNAME).exists());
        finish_readdress(after.path()).expect("finish readdress cleanup");
        assert!(!after.path().join(HIQLITE_READDRESS_BACKUP_DIRNAME).exists());
        assert!(!after
            .path()
            .join(HIQLITE_READDRESS_MARKER_FILENAME)
            .exists());
    }

    #[cfg(feature = "hiqlite-store")]
    fn free_test_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind test port")
            .local_addr()
            .expect("test port address")
            .port()
    }

    #[cfg(feature = "hiqlite-store")]
    fn membership_test_config(data_dir: &Path) -> Config {
        let mut config = Config::default();
        config.storage.data_dir = data_dir.to_owned();
        config.cluster.raft_bind = format!("127.0.0.1:{}", free_test_port())
            .parse()
            .expect("Raft bind");
        config.cluster.api_bind = format!("127.0.0.1:{}", free_test_port())
            .parse()
            .expect("cluster API bind");
        // `localhost` is reachable on this machine but is not parsed as a
        // loopback SocketAddr. Tests can therefore distinguish committed
        // advertised membership from the old hard-coded 127.0.0.1 record.
        config.cluster.advertise_host = "localhost".to_owned();
        config
    }

    #[cfg(feature = "hiqlite-store")]
    fn membership_http_error(error: super::super::membership::MembershipError) -> Response {
        (
            StatusCode::CONFLICT,
            Json(json!({ "code": error.code(), "message": error.to_string() })),
        )
            .into_response()
    }

    #[cfg(feature = "hiqlite-store")]
    async fn redeem_join_for_test(
        State(manager): State<MembershipManager>,
        Json(request): Json<RedeemJoinRequest>,
    ) -> Response {
        match manager.redeem(&request).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => membership_http_error(error),
        }
    }

    #[cfg(feature = "hiqlite-store")]
    async fn redeem_learner_join_for_test(
        State(manager): State<MembershipManager>,
        Json(request): Json<RedeemJoinRequest>,
    ) -> Response {
        match manager.redeem_learner(&request).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => membership_http_error(error),
        }
    }

    #[cfg(feature = "hiqlite-store")]
    async fn finalize_join_for_test(
        State(manager): State<MembershipManager>,
        Json(request): Json<FinalizeJoinRequest>,
    ) -> Response {
        match manager.finalize(&request).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => membership_http_error(error),
        }
    }

    #[cfg(feature = "hiqlite-store")]
    async fn finalize_learner_join_for_test(
        State(manager): State<MembershipManager>,
        Json(request): Json<FinalizeJoinRequest>,
    ) -> Response {
        match manager.finalize_learner(&request).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => membership_http_error(error),
        }
    }

    /// Exercise the operator's actual daemon join entry point, including the
    /// token file, public redeem/finalize wire, local membership state, and
    /// fully-TLS voter startup. The in-process membership harness alone cannot
    /// cover any of these pre-store decisions.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn daemon_join_refuses_occupied_and_expired_targets_then_resumes_finalization() {
        install_default_crypto_provider();

        let source_dir = tempfile::tempdir().expect("source data dir");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("join coordinator listener");
        let coordinator_addr = listener.local_addr().expect("coordinator address");
        let mut source_config = membership_test_config(source_dir.path());
        source_config.cluster.advertise_host.clear();
        source_config.server.bind = coordinator_addr;
        source_config.cluster.join_url = format!("http://{coordinator_addr}");
        drop(SqliteStore::open(&source_dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
        let source = select_daemon_store(&source_config)
            .await
            .expect("activate source voter");
        let cluster_id = source.identity.cluster_id.clone();
        source
            .store
            .put_setting("membership.readdress", "preserved")
            .await
            .expect("write state before readdress");
        let original_metrics = source
            .local_client
            .as_ref()
            .expect("source client")
            .metrics_db()
            .await
            .expect("source membership");
        let original = original_metrics
            .membership_config
            .nodes()
            .find(|(raft_id, _)| **raft_id == 1)
            .map(|(_, node)| node.clone())
            .expect("source voter record");
        assert!(original.addr_api.starts_with("127.0.0.1:"));
        source.shutdown().await.expect("stop loopback source");
        drop(source);

        // This is the actual upgrade path: an already-running one-voter install
        // opts into membership later. Its committed Raft address, not only
        // membership.json, must become remotely usable without losing data.
        // A real daemon restart drops the old runtime and frees these ports;
        // this in-process test uses fresh ports because Hiqlite 0.14 leaves its
        // TLS listener task alive until the test runtime itself exits.
        source_config.cluster.raft_bind = format!("127.0.0.1:{}", free_test_port())
            .parse()
            .expect("readdressed Raft bind");
        source_config.cluster.api_bind = format!("127.0.0.1:{}", free_test_port())
            .parse()
            .expect("readdressed cluster API bind");
        source_config.cluster.advertise_host = "localhost".to_owned();
        let source = select_daemon_store(&source_config)
            .await
            .expect("readdress existing source voter");
        assert_eq!(source.identity.cluster_id, cluster_id);
        assert_eq!(
            source
                .store
                .get_setting("membership.readdress")
                .await
                .expect("read state after readdress")
                .as_deref(),
            Some("preserved")
        );
        let readdressed_metrics = source
            .local_client
            .as_ref()
            .expect("readdressed source client")
            .metrics_db()
            .await
            .expect("readdressed membership");
        let readdressed = readdressed_metrics
            .membership_config
            .nodes()
            .find(|(raft_id, _)| **raft_id == 1)
            .map(|(_, node)| node.clone())
            .expect("readdressed voter record");
        assert!(
            readdressed.addr_api.starts_with("localhost:"),
            "committed membership remained loopback: {readdressed:?}"
        );
        assert!(!source_dir
            .path()
            .join(HIQLITE_READDRESS_BACKUP_DIRNAME)
            .exists());
        assert!(!source_dir
            .path()
            .join(HIQLITE_READDRESS_MARKER_FILENAME)
            .exists());
        let coordinator = source.membership_manager();
        let source_client = source.local_client.clone().expect("source client");
        let app = Router::new()
            .route("/api/v1/cluster/join/redeem", post(redeem_join_for_test))
            .route(
                "/api/v1/cluster/learner/join/redeem",
                post(redeem_learner_join_for_test),
            )
            .route(
                "/api/v1/cluster/join/finalize",
                post(finalize_join_for_test),
            )
            .route(
                "/api/v1/cluster/learner/join/finalize",
                post(finalize_learner_join_for_test),
            )
            .with_state(coordinator.clone());
        let http_task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve join coordinator");
        });

        let occupied = tempfile::tempdir().expect("occupied join dir");
        let occupied_token = occupied.path().join("join.token");
        std::fs::write(&occupied_token, "unused\n").expect("occupied token file");
        File::create(occupied.path().join(SQLITE_FILENAME)).expect("occupied SQLite path");
        let mut occupied_config = membership_test_config(occupied.path());
        occupied_config.cluster.join_token_file = occupied_token;
        let occupied_error = select_daemon_store(&occupied_config)
            .await
            .err()
            .expect("existing SQLite must refuse daemon join")
            .to_string();
        assert!(occupied_error.contains("refusing to join with existing local database"));

        let expired = coordinator
            .issue_token(Duration::from_secs(1))
            .await
            .expect("issue expiring token");
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        let expired_dir = tempfile::tempdir().expect("expired join dir");
        let expired_path = expired_dir.path().join("join.token");
        std::fs::write(&expired_path, format!("{}\n", expired.token)).expect("expired token file");
        let mut expired_config = membership_test_config(expired_dir.path());
        expired_config.cluster.join_token_file = expired_path;
        let expired_error = select_daemon_store(&expired_config)
            .await
            .err()
            .expect("expired token must refuse daemon join")
            .to_string();
        assert!(
            expired_error.contains("join_token_expired"),
            "{expired_error}"
        );
        assert!(!expired_dir.path().join(HIQLITE_ACTIVE_DIRNAME).exists());

        // A still-live token may reserve the next Raft id while another
        // machine starts first. OpenRaft permits that sparse membership, and
        // the production daemon must not confuse the assigned id with the
        // length of its bootstrap vector (Hiqlite 0.14 does exactly that
        // without the compatibility adapter in `start_voter`).
        let held_gap = coordinator
            .issue_token(Duration::from_secs(120))
            .await
            .expect("reserve the preceding Raft id");
        let issued = coordinator
            .issue_token(Duration::from_secs(120))
            .await
            .expect("issue daemon join token");
        assert_eq!(
            issued.raft_id,
            held_gap.raft_id + 1,
            "daemon join fixture must exercise a sparse assigned id"
        );
        let joining_dir = tempfile::tempdir().expect("joining data dir");
        let token_path = joining_dir.path().join("join.token");
        std::fs::write(&token_path, format!("{}\n", issued.token)).expect("joining token file");
        let mut joining_config = membership_test_config(joining_dir.path());
        joining_config.cluster.join_token_file = token_path.clone();
        let staged_identity = crate::cluster::initialize_join_identity(
            joining_dir.path(),
            &cluster_id,
            issued.raft_id,
        )
        .expect("stage the joining node identity");
        let staged_local = configured_local_peer(&joining_config, issued.raft_id)
            .expect("configure the staged peer");
        let issued_digest = join_token_digest(&issued.token);
        coordinator
            .redeem(&RedeemJoinRequest {
                token_digest: issued_digest.clone(),
                raft_id: issued.raft_id,
                node_id: staged_identity.node_id,
                hostname: "joining-test-node".to_owned(),
                raft_address: staged_local.raft_address,
                api_address: staged_local.api_address,
                http_base: configured_artwork_url(&joining_config)
                    .expect("derive the staged node artwork origin"),
                schema_version: AUTH_SCHEMA_VERSION,
                protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
                protocol_min: crate::store::AUTH_PROTOCOL_MIN,
                protocol_max: crate::store::AUTH_PROTOCOL_MAX,
                live_tv_v1: true,
            })
            .await
            .expect("reserve the token to the staged node before its failed start");
        source_client
            .execute(
                "UPDATE cluster_join_tokens SET expires_at = 0 \
                 WHERE token_hash = $1 AND state = 'redeeming'",
                hiqlite::params!(issued_digest.as_str()),
            )
            .await
            .expect("expire the identity-bound reservation deterministically");
        let joined = select_daemon_store(&joining_config)
            .await
            .expect("resume an expired identity-bound join through daemon store selection");
        assert_eq!(joined.identity.cluster_id, cluster_id);
        assert_eq!(joined.identity.raft_id, issued.raft_id);
        assert!(
            !token_path.exists(),
            "successful finalization removes the token"
        );
        let local = read_local_membership(joining_dir.path())
            .expect("read joined local membership")
            .expect("joined node persists local membership");
        assert_eq!(
            local.join_token_digest.as_deref(),
            Some(issued_digest.as_str())
        );

        // A crash after finalization but before unlink leaves exactly this
        // shape: active target + membership.json + the original token. The
        // retry is idempotent and removes it on the next pass.
        std::fs::write(&token_path, format!("{}\n", issued.token))
            .expect("restore interrupted-finalization token");
        finalize_pending_join(&joining_config, &joined)
            .await
            .expect("resume finalized join");
        assert!(!token_path.exists());

        // An unrelated copied token is not this node's pending state. It is
        // ignored rather than taking a healthy voter offline.
        let foreign = coordinator
            .issue_token(Duration::from_secs(120))
            .await
            .expect("issue foreign token");
        std::fs::write(&token_path, format!("{}\n", foreign.token)).expect("foreign token file");
        finalize_pending_join(&joining_config, &joined)
            .await
            .expect("foreign token is a warning, not a startup failure");
        assert!(
            token_path.exists(),
            "foreign token remains for operator inspection"
        );

        // An operator staging two machines, or replacing a token they lost,
        // must not be made to wait out the first token's TTL. Allocation walks
        // past ids that live tokens are holding instead of recomputing the one
        // id an outstanding token already reserved.
        let concurrent = coordinator
            .issue_token(Duration::from_secs(120))
            .await
            .expect("second token while the first is still outstanding");
        assert_ne!(
            concurrent.raft_id, foreign.raft_id,
            "concurrent tokens reserved the same Raft id"
        );

        // Once voter state is durable, a coordinator outage may delay token
        // consumption but must not take the healthy joined daemon offline.
        std::fs::write(&token_path, format!("{}\n", issued.token))
            .expect("restore own token before coordinator outage");
        http_task.abort();
        finalize_pending_join_best_effort(&joining_config, &joined).await;
        assert!(
            token_path.exists(),
            "pending finalization keeps the identity-bound token for retry"
        );

        let local_client = joined.local_client.clone().expect("joined local client");
        joined.shutdown().await.expect("drain joined voter");
        let post_shutdown = tokio::time::timeout(
            Duration::from_secs(2),
            local_client.execute(
                "CREATE TABLE IF NOT EXISTS shutdown_must_have_closed_writers (id INTEGER)",
                hiqlite::macros::params!(),
            ),
        )
        .await;
        assert!(
            !matches!(post_shutdown, Ok(Ok(_))),
            "shutdown returned while the replicated writer still accepted work"
        );

        source.shutdown().await.expect("drain source voter");
    }

    #[tokio::test]
    async fn backup_includes_committed_wal_state_and_removes_abandoned_incoming() {
        let data = tempfile::tempdir().expect("data dir");
        let source_path = data.path().join(SQLITE_FILENAME);
        let store = SqliteStore::open(&source_path).expect("source store");
        store
            .put_setting("migration.wal", "committed")
            .await
            .expect("write source");

        let incoming = data.path().join(HIQLITE_INCOMING_DIRNAME);
        std::fs::create_dir_all(incoming.join("partial")).expect("incoming tree");
        std::fs::write(incoming.join("partial/state"), b"not complete").expect("incoming state");

        let prepared = prepare_sqlite_import(data.path()).expect("prepare import");
        assert!(!incoming.exists(), "abandoned target was removed");
        assert_eq!(prepared.schema_version, SQLITE_SCHEMA_VERSION);
        assert_eq!(
            prepared.backup_sha256,
            sha256_file(&prepared.backup_path).expect("hash published backup")
        );

        let backup = Connection::open_with_flags(
            &prepared.backup_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open backup");
        let value: String = backup
            .query_row(
                "SELECT value FROM settings WHERE key = 'migration.wal'",
                [],
                |row| row.get(0),
            )
            .expect("backed-up setting");
        assert_eq!(value, "committed");
        let journal_mode: String = backup
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read backup journal mode");
        assert_eq!(journal_mode, "delete");
    }

    #[test]
    fn future_schema_refusal_changes_nothing() {
        let data = tempfile::tempdir().expect("data dir");
        let source_path = data.path().join(SQLITE_FILENAME);
        let source = Connection::open(&source_path).expect("source");
        source
            .pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION + 1)
            .expect("future version");
        drop(source);
        let incoming = data.path().join(HIQLITE_INCOMING_DIRNAME);
        std::fs::create_dir(&incoming).expect("incoming");
        std::fs::write(incoming.join("state"), b"preserve").expect("state");

        let error = prepare_sqlite_import(data.path()).expect_err("future schema refused");
        assert!(error.to_string().contains("refusing clustering import"));
        assert!(incoming.join("state").exists());
        assert!(!data.path().join(MIGRATION_DIRNAME).exists());
    }

    #[tokio::test]
    async fn content_addressed_backups_are_reused_and_old_backups_are_kept() {
        let data = tempfile::tempdir().expect("data dir");
        let source_path = data.path().join(SQLITE_FILENAME);
        let store = SqliteStore::open(&source_path).expect("source store");
        let migration_dir = data.path().join(MIGRATION_DIRNAME);
        std::fs::create_dir(&migration_dir).expect("migration dir");
        let abandoned = migration_dir.join(format!(
            ".{SQLITE_FILENAME}.{}.incoming",
            uuid::Uuid::new_v4()
        ));
        let abandoned_wal = PathBuf::from(format!("{}-wal", abandoned.display()));
        let abandoned_shm = PathBuf::from(format!("{}-shm", abandoned.display()));
        std::fs::write(&abandoned, b"partial backup").expect("abandoned backup");
        std::fs::write(&abandoned_wal, b"partial wal").expect("abandoned backup wal");
        std::fs::write(&abandoned_shm, b"partial shm").expect("abandoned backup shm");
        let first = prepare_sqlite_import(data.path()).expect("first backup");
        assert!(!abandoned.exists(), "abandoned backup temp was removed");
        assert!(!abandoned_wal.exists(), "abandoned backup WAL was removed");
        assert!(!abandoned_shm.exists(), "abandoned backup SHM was removed");
        let repeated = prepare_sqlite_import(data.path()).expect("repeated backup");
        assert_eq!(first, repeated);

        store
            .put_setting("migration.changed", "yes")
            .await
            .expect("change source");
        let changed = prepare_sqlite_import(data.path()).expect("changed backup");
        assert_ne!(changed.backup_path, first.backup_path);
        assert!(first.backup_path.exists(), "original backup is retained");
        assert!(changed.backup_path.exists(), "new backup is retained");
        assert!(
            std::fs::read_dir(data.path().join(MIGRATION_DIRNAME))
                .expect("migration dir")
                .all(|entry| {
                    let entry = entry.expect("migration entry");
                    !is_backup_temp_artifact(&entry.file_name().to_string_lossy())
                }),
            "no temporary backup remains"
        );
    }

    /// A failing activation must not fill the data volume with database copies.
    ///
    /// Content addressing alone does not bound this: each failed attempt leaves
    /// a recovery boot that serves traffic and mutates the source, so the next
    /// retry addresses different content and writes another full-size copy.
    #[tokio::test]
    async fn repeated_activation_attempts_bound_retained_source_backups() {
        let data = tempfile::tempdir().expect("data dir");
        let source_path = data.path().join(SQLITE_FILENAME);
        let store = SqliteStore::open(&source_path).expect("source store");

        let mut published = Vec::new();
        // Comfortably past the retention window, each with different content,
        // standing in for the mutation a recovery boot makes between retries.
        for attempt in 0..MIGRATION_BACKUP_RETENTION + 3 {
            store
                .put_setting("migration.attempt", &attempt.to_string())
                .await
                .expect("change source between attempts");
            published.push(
                prepare_sqlite_import(data.path())
                    .expect("attempt backup")
                    .backup_path,
            );
        }

        let retained: Vec<_> = std::fs::read_dir(data.path().join(MIGRATION_DIRNAME))
            .expect("migration dir")
            .map(|entry| entry.expect("migration entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
            .collect();
        assert_eq!(
            retained.len(),
            MIGRATION_BACKUP_RETENTION,
            "retained backups: {retained:?}"
        );
        // The window keeps the newest, and above all the one this attempt is
        // about to import — deleting that would break the activation it serves.
        for recent in published.iter().rev().take(MIGRATION_BACKUP_RETENTION) {
            assert!(retained.contains(recent), "{recent:?} must be retained");
        }
        for old in published
            .iter()
            .take(published.len() - MIGRATION_BACKUP_RETENTION)
        {
            assert!(!old.exists(), "{old:?} must have been pruned");
        }
    }

    #[test]
    fn corrupt_content_addressed_backup_is_refused() {
        let data = tempfile::tempdir().expect("data dir");
        let source_path = data.path().join(SQLITE_FILENAME);
        SqliteStore::open(&source_path).expect("source store");

        let prepared = prepare_sqlite_import(data.path()).expect("first backup");
        std::fs::write(&prepared.backup_path, b"not a SQLite backup")
            .expect("poison published backup");

        let error = prepare_sqlite_import(data.path()).expect_err("corrupt backup refused");
        assert!(
            error
                .to_string()
                .contains("does not match its content-addressed name"),
            "unexpected error: {error}"
        );
    }

    /// The upgrade contract, on a real replicated voter.
    ///
    /// Everything here happens with no operator action beyond installing this
    /// binary: the cluster must stay on the pre-P6 protocol, must keep
    /// admitting the previous release, and must move only when activation is
    /// asked for. The refusal that comes after activation is the point of the
    /// whole guard, so it is asserted on the message an operator would read.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_deployed_binary_widens_no_range_until_activation_is_asked_for() {
        install_default_crypto_provider();

        let dir = tempfile::tempdir().expect("protocol activation data dir");
        let config = membership_test_config(dir.path());
        drop(SqliteStore::open(&dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
        let selected = select_daemon_store(&config)
            .await
            .expect("activate a one-voter cluster on this binary");
        let membership = selected.membership_manager();
        let client = selected
            .local_client
            .as_ref()
            .expect("voter client")
            .clone();

        // The binary that shipped before P6: it implements exactly protocol 4.
        let previous_release = crate::store::ClusterCompatibility {
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_min: crate::store::AUTH_PROTOCOL_VERSION,
            protocol_max: crate::store::AUTH_PROTOCOL_VERSION,
        };

        // 1. Installing this binary changed nothing.
        assert_eq!(
            membership
                .active_protocol_range()
                .await
                .expect("read the active range"),
            (
                crate::store::AUTH_PROTOCOL_MIN,
                crate::store::AUTH_PROTOCOL_MIN
            ),
            "a bootstrapped cluster must not activate protocol 5 by itself"
        );
        HiqliteAuthStore::preflight_voter(&client, previous_release)
            .await
            .expect("the previous release must still be able to join this cluster");
        HiqliteAuthStore::preflight_voter(&client, crate::store::ClusterCompatibility::CURRENT)
            .await
            .expect("and so must this one");

        // A learner's preflight against the same unactivated cluster is
        // refused, and the refusal is not dressed as a database problem. It
        // reached operators as `schema migration failed: learner_protocol_
        // inactive: …`, which sent them to look at the schema when the answer
        // was "run the activation command".
        let learner_refusal = HiqliteAuthStore::preflight_role(
            &client,
            crate::store::ClusterCompatibility::CURRENT,
            true,
        )
        .await
        .expect_err("an unactivated cluster must refuse a learner's preflight")
        .to_string();
        assert!(
            learner_refusal.contains("learner_protocol_inactive"),
            "{learner_refusal}"
        );
        assert!(
            !learner_refusal.contains("migration"),
            "a protocol refusal must not arrive as a migration failure: {learner_refusal}"
        );

        // The node's own heartbeat is the proof; nothing else writes it.
        let status = membership
            .protocol_status()
            .await
            .expect("read the protocol status");
        assert!(
            status.learner_protocol_pending.is_empty(),
            "the running binary proved the learner protocol by heartbeating: {status:?}"
        );
        assert!(!status.learner_protocol_active);

        // An unactivated cluster still admits a joiner that predates the range
        // fields entirely. This is the other half of "zero operator action":
        // the coordinator was upgraded, and a not-yet-upgraded node can still
        // join it. Any refusal here is addressing, never the protocol gate.
        let unactivated_token = membership
            .issue_token(Duration::from_secs(120))
            .await
            .expect("issue a join token before activation");
        let pre_range_joiner = RedeemJoinRequest {
            token_digest: join_token_digest(&unactivated_token.token),
            raft_id: unactivated_token.raft_id,
            node_id: "pre-range-joiner".to_owned(),
            hostname: "pre-range-joiner".to_owned(),
            raft_address: "127.0.0.1:3".to_owned(),
            api_address: "127.0.0.1:4".to_owned(),
            http_base: String::new(),
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
            protocol_min: 0,
            protocol_max: 0,
            live_tv_v1: false,
        };
        assert_ne!(
            membership
                .redeem(&pre_range_joiner)
                .await
                .err()
                .map(|error| error.code().to_owned())
                .unwrap_or_default(),
            "join_incompatible",
            "an unactivated cluster must not refuse a joiner that predates the range"
        );

        // And that admitted joiner now holds activation, which is exactly the
        // point: it redeemed on a binary that predates protocol 5, it is
        // mid-join, and narrowing the range past it would leave a node that
        // counts for quorum and can never open its store again.
        match membership.activate_learner_protocol().await {
            Err(super::super::membership::MembershipError::JoinInFlight(nodes)) => {
                assert_eq!(nodes, vec!["pre-range-joiner".to_owned()]);
            }
            other => panic!("a join in flight must hold activation: {other:?}"),
        }
        assert_eq!(
            membership
                .active_protocol_range()
                .await
                .expect("read the range"),
            (
                crate::store::AUTH_PROTOCOL_MIN,
                crate::store::AUTH_PROTOCOL_MIN
            ),
            "a refused activation must not move the range"
        );
        // The operator's exit: this joiner has no process behind it and is
        // never going to finish, so remove it.
        client
            .execute(
                "UPDATE cluster_nodes SET removed_at = $1 WHERE node_id = $2",
                hiqlite::params!(1_i64, "pre-range-joiner"),
            )
            .await
            .expect("tombstone the abandoned joiner");

        // 2. Activation is explicit, and only then does the range move.
        let activated = membership
            .activate_learner_protocol()
            .await
            .expect("activate the learner protocol");
        assert!(
            activated.changed,
            "the first activation commits: {activated:?}"
        );
        assert!(activated.protocol.learner_protocol_active);
        assert_eq!(
            membership
                .active_protocol_range()
                .await
                .expect("read the activated range"),
            (
                crate::store::AUTH_PROTOCOL_MAX,
                crate::store::AUTH_PROTOCOL_MAX
            )
        );

        // 3. Idempotent: a retried request after a timeout is not an error and
        //    is not a second write.
        let again = membership
            .activate_learner_protocol()
            .await
            .expect("activating an already-activated cluster succeeds");
        assert!(!again.changed, "activation must not write twice: {again:?}");

        // 4. The previous release can no longer boot here, and is told why.
        let refusal = HiqliteAuthStore::preflight_voter(&client, previous_release)
            .await
            .expect_err("an activated cluster must refuse the previous release")
            .to_string();
        assert!(refusal.contains("protocol 5"), "{refusal}");
        assert!(refusal.contains("too old"), "{refusal}");
        HiqliteAuthStore::preflight_voter(&client, crate::store::ClusterCompatibility::CURRENT)
            .await
            .expect("this binary keeps working across activation");

        // The admission gate agrees with the boot guard rather than comparing
        // against a constant of its own.
        let issued = membership
            .issue_token(Duration::from_secs(120))
            .await
            .expect("issue a join token for the activated cluster");
        let old_joiner = RedeemJoinRequest {
            token_digest: join_token_digest(&issued.token),
            raft_id: issued.raft_id,
            node_id: "old-binary-joiner".to_owned(),
            hostname: "old-binary-joiner".to_owned(),
            raft_address: "127.0.0.1:1".to_owned(),
            api_address: "127.0.0.1:2".to_owned(),
            http_base: String::new(),
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: crate::store::AUTH_PROTOCOL_VERSION,
            // A joiner that predates the range sends no range fields.
            protocol_min: 0,
            protocol_max: 0,
            live_tv_v1: false,
        };
        assert_eq!(
            membership
                .redeem(&old_joiner)
                .await
                .expect_err("an activated cluster must refuse a protocol-4-only joiner")
                .code(),
            "join_incompatible"
        );
        let new_joiner = RedeemJoinRequest {
            protocol_min: crate::store::AUTH_PROTOCOL_MIN,
            protocol_max: crate::store::AUTH_PROTOCOL_MAX,
            live_tv_v1: true,
            ..old_joiner
        };
        // This one clears the protocol gate; it fails later, on addressing,
        // which is exactly what proves the protocol check let it through.
        assert_ne!(
            membership
                .redeem(&new_joiner)
                .await
                .err()
                .map(|error| error.code().to_owned())
                .unwrap_or_default(),
            "join_incompatible",
            "a range-declaring joiner must clear the activated protocol gate"
        );

        // 5. The reversible rollback, and its idempotence.
        let rolled_back = membership
            .deactivate_learner_protocol()
            .await
            .expect("deactivate with no learner present");
        assert!(rolled_back.changed);
        assert!(!rolled_back.protocol.learner_protocol_active);
        let noop = membership
            .deactivate_learner_protocol()
            .await
            .expect("deactivating an unactivated cluster succeeds");
        assert!(!noop.changed, "deactivation must not write twice: {noop:?}");
        HiqliteAuthStore::preflight_voter(&client, previous_release)
            .await
            .expect("rollback restores the previous release's ability to boot");

        selected.shutdown().await.expect("stop the voter");
    }

    /// The replicated schema marker, read through the leader.
    #[cfg(feature = "hiqlite-store")]
    async fn replicated_schema_version(client: &Client) -> i64 {
        client
            .query_consistent_map::<SchemaVersionRow, _>(
                "SELECT schema_version FROM cluster_meta WHERE singleton = 1",
                hiqlite::params!(),
            )
            .await
            .expect("read the replicated schema marker")
            .first()
            .expect("cluster_meta has exactly one row")
            .schema_version
    }

    #[cfg(feature = "hiqlite-store")]
    struct SchemaVersionRow {
        schema_version: i64,
    }

    #[cfg(feature = "hiqlite-store")]
    impl From<&mut hiqlite::Row<'_>> for SchemaVersionRow {
        fn from(row: &mut hiqlite::Row<'_>) -> Self {
            Self {
                schema_version: row.get("schema_version"),
            }
        }
    }

    /// One replicated membership row's durable admission role.
    #[cfg(feature = "hiqlite-store")]
    #[derive(Debug)]
    struct NodeRoleRow {
        node_id: String,
        role: Option<String>,
    }

    #[cfg(feature = "hiqlite-store")]
    impl From<&mut hiqlite::Row<'_>> for NodeRoleRow {
        fn from(row: &mut hiqlite::Row<'_>) -> Self {
            Self {
                node_id: row.get("node_id"),
                role: row.get("role"),
            }
        }
    }

    /// A token framing this build does not implement must leave the data
    /// directory exactly as it found it.
    ///
    /// The assertion is on the filesystem rather than on the return value on
    /// purpose: the refusal is only worth anything if this node is not holding
    /// the cluster's raft, API, and credential secrets afterwards, and has not
    /// staged an identity a later boot could resume. `plxjoin:v3` stands in for
    /// any future protocol — including, from an older build's side, `plxjoin:v2`
    /// itself.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test]
    async fn an_unsupported_join_token_is_refused_before_any_secret_is_written() {
        let joining = tempfile::tempdir().expect("joining data dir");
        let token_path = joining.path().join("join.token");
        // Well-formed framing, unknown version: 32-byte key, 24-byte nonce and
        // a body, so nothing but the version can be what refuses it.
        let token = format!("plxjoin:v3:{}:{}", "ab".repeat(32), "cd".repeat(64));
        std::fs::write(&token_path, format!("{token}\n")).expect("write token file");
        let mut config = membership_test_config(joining.path());
        config.cluster.join_token_file = token_path.clone();

        let error = select_daemon_store(&config)
            .await
            .err()
            .expect("an unsupported token framing must refuse the join")
            .to_string();
        assert!(error.contains("join_token_invalid"), "{error}");

        for leftover in [
            joining.path().join(RAFT_SECRET_FILENAME),
            joining.path().join(API_SECRET_FILENAME),
            config.cluster.credential_key_path(joining.path()),
            joining.path().join(crate::cluster::NODE_ID_FILENAME),
            joining.path().join(LOCAL_MEMBERSHIP_FILENAME),
            joining.path().join(HIQLITE_ACTIVE_DIRNAME),
        ] {
            assert!(
                !leftover.exists(),
                "a refused join left {} behind",
                leftover.display()
            );
        }
        assert!(
            token_path.exists(),
            "the operator's token stays put for inspection"
        );
    }

    /// The previous release's `membership.json` version check, transcribed.
    ///
    /// A build that predates the learner role has no idea it must not campaign,
    /// so it has to refuse a learner's record rather than read it as a voter's.
    /// That is the desired behaviour, which means it is worth a test rather
    /// than a comment.
    #[cfg(feature = "hiqlite-store")]
    fn the_previous_release_accepts(record: &LocalMembership) -> bool {
        record.version == 1
            && !record.cluster_id.trim().is_empty()
            && !record.node_id.trim().is_empty()
            && record.raft_id != 0
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn a_learner_membership_record_fails_closed_on_an_older_binary() {
        let peer = ClusterPeer {
            raft_id: 2,
            raft_address: "localhost:32401".to_owned(),
            api_address: "localhost:32402".to_owned(),
        };
        let voter = LocalMembership {
            version: local_membership_version(ClusterRole::Voter),
            cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            node_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            raft_id: 2,
            local: peer.clone(),
            bootstrap: vec![peer.clone()],
            join_token_digest: None,
            role: ClusterRole::Voter,
        };
        let learner = LocalMembership {
            version: local_membership_version(ClusterRole::Learner),
            role: ClusterRole::Learner,
            ..voter.clone()
        };

        // Installing this binary does not rewrite a voter's record, so rolling
        // that node back to the previous release still works.
        assert!(the_previous_release_accepts(&voter));
        assert!(!the_previous_release_accepts(&learner));

        // And this build round-trips both, including the v1 record's implied
        // role — which is the v1-to-v2 read path.
        let dir = tempfile::tempdir().expect("membership dir");
        write_local_membership(dir.path(), &voter).expect("write a voter record");
        assert_eq!(
            read_local_membership(dir.path()).expect("read it back"),
            Some(voter.clone())
        );
        let raw = std::fs::read_to_string(dir.path().join(LOCAL_MEMBERSHIP_FILENAME))
            .expect("read the raw record");
        assert!(raw.contains("\"version\": 1"), "{raw}");

        write_local_membership(dir.path(), &learner).expect("write a learner record");
        assert_eq!(
            read_local_membership(dir.path()).expect("read it back"),
            Some(learner)
        );

        // A record whose version and role disagree is an edited file, not a
        // version this build has to interpret.
        write_local_membership(
            dir.path(),
            &LocalMembership {
                version: 1,
                role: ClusterRole::Learner,
                ..voter
            },
        )
        .expect("write an inconsistent record");
        assert!(
            read_local_membership(dir.path()).is_err(),
            "a v1 record must not be allowed to claim the learner role"
        );
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn a_promoted_learner_persists_a_crash_safe_downgrade_readable_voter_role() {
        let dir = tempfile::tempdir().expect("promoted data dir");
        let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
        std::fs::create_dir_all(&active).expect("active target");
        let peer = ClusterPeer {
            raft_id: 4,
            raft_address: "localhost:33401".to_owned(),
            api_address: "localhost:33402".to_owned(),
        };
        let mut membership = LocalMembership {
            version: local_membership_version(ClusterRole::Learner),
            cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            node_id: "44444444-4444-4444-8444-444444444444".to_owned(),
            raft_id: 4,
            local: peer.clone(),
            bootstrap: vec![peer],
            join_token_digest: Some("a".repeat(64)),
            role: ClusterRole::Learner,
        };
        let mut marker = ActivationMarker {
            marker_version: ACTIVATION_MARKER_VERSION,
            cluster_id: membership.cluster_id.clone(),
            source_backup_sha256: "0".repeat(64),
            source_schema_version: AUTH_SCHEMA_VERSION,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: 0,
            table_hashes: vec![SqliteImportTableDigest {
                table: "settings".to_owned(),
                row_count: 0,
                sha256: "0".repeat(64),
            }],
            admitted_role: Some(ClusterRole::Learner),
        };
        write_local_membership(dir.path(), &membership).expect("learner membership");
        write_activation_marker(&active, &marker).expect("learner activation marker");

        persist_promoted_voter_role(dir.path()).expect("persist promoted role");
        let promoted = read_local_membership(dir.path())
            .expect("read promoted membership")
            .expect("membership exists");
        assert_eq!(promoted.role, ClusterRole::Voter);
        assert_eq!(promoted.version, 1);
        assert!(the_previous_release_accepts(&promoted));
        assert_eq!(
            read_activation_marker(&active)
                .expect("read promoted marker")
                .admitted_role,
            Some(ClusterRole::Voter)
        );

        // Recreate the only crash interval: membership was fsynced as voter,
        // activation.json still says learner. The next boot's reconciliation
        // completes the marker and remains idempotent.
        membership.role = ClusterRole::Voter;
        membership.version = local_membership_version(ClusterRole::Voter);
        marker.admitted_role = Some(ClusterRole::Learner);
        write_local_membership(dir.path(), &membership).expect("interrupted voter record");
        write_activation_marker(&active, &marker).expect("stale learner marker");
        persist_promoted_voter_role(dir.path()).expect("resume interrupted promotion");
        persist_promoted_voter_role(dir.path()).expect("idempotent retry");
        assert_eq!(
            read_activation_marker(&active)
                .expect("read resumed marker")
                .admitted_role,
            Some(ClusterRole::Voter)
        );
    }

    /// The admitted-role record grants cluster-wide job authority to a process
    /// that cannot read committed membership, so a *missing* record must not
    /// be read as "voter" on a clustered directory.
    ///
    /// Two different absences: a directory that was never clustered has no
    /// record because there is nothing to record, and that is the ordinary
    /// single-node server which owns every job. A directory with an activation
    /// marker and no record is a cluster node whose one durable statement of
    /// what it was admitted as has been lost — and if that node was a learner,
    /// answering "voter" hands it `provider:artwork`.
    ///
    /// Gated like everything it calls. Without this, `cargo test -p plurx-core
    /// --lib` does not compile — the same default-feature blind spot that hid
    /// `local_cluster_role`'s own missing gate, one layer up.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn a_clustered_directory_with_no_admitted_role_record_refuses_instead_of_defaulting() {
        let dir = tempfile::tempdir().expect("data dir");

        // Never clustered: no marker, no record, and the single-node answer.
        assert_eq!(
            local_cluster_role(dir.path()).expect("an unclustered directory answers"),
            ClusterRole::Voter
        );

        // Clustered, record present: the record answers.
        let peer = ClusterPeer {
            raft_id: 2,
            raft_address: "localhost:32401".to_owned(),
            api_address: "localhost:32402".to_owned(),
        };
        let learner = LocalMembership {
            version: local_membership_version(ClusterRole::Learner),
            cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            node_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            raft_id: 2,
            local: peer.clone(),
            bootstrap: vec![peer],
            join_token_digest: None,
            role: ClusterRole::Learner,
        };
        write_local_membership(dir.path(), &learner).expect("write a learner record");
        let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
        std::fs::create_dir_all(&active).expect("create the active directory");
        std::fs::write(active.join(ACTIVATION_MARKER_FILENAME), b"{}")
            .expect("write an activation marker");
        assert_eq!(
            local_cluster_role(dir.path()).expect("a clustered directory answers from its record"),
            ClusterRole::Learner
        );

        // Clustered, record gone: refuse rather than grant.
        std::fs::remove_file(dir.path().join(LOCAL_MEMBERSHIP_FILENAME))
            .expect("delete the learner's record");
        let refused = local_cluster_role(dir.path())
            .expect_err("a clustered directory with no admitted-role record must refuse")
            .to_string();
        assert!(
            refused.contains(LOCAL_MEMBERSHIP_FILENAME),
            "the refusal must name the missing file: {refused}"
        );
    }

    /// A joined role is durable before `activation.json` becomes the commit
    /// bit, and a retry after a crash at that boundary resumes the same role.
    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn join_activation_crash_after_membership_never_restarts_a_learner_as_a_voter() {
        let dir = tempfile::tempdir().expect("joined data dir");
        let active = dir.path().join(HIQLITE_ACTIVE_DIRNAME);
        std::fs::create_dir_all(&active).expect("active target");
        let peer = ClusterPeer {
            raft_id: 4,
            raft_address: "localhost:33401".to_owned(),
            api_address: "localhost:33402".to_owned(),
        };
        let membership = LocalMembership {
            version: local_membership_version(ClusterRole::Learner),
            cluster_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            node_id: "44444444-4444-4444-8444-444444444444".to_owned(),
            raft_id: 4,
            local: peer.clone(),
            bootstrap: vec![peer],
            join_token_digest: Some("a".repeat(64)),
            role: ClusterRole::Learner,
        };
        let marker = ActivationMarker {
            marker_version: ACTIVATION_MARKER_VERSION,
            cluster_id: membership.cluster_id.clone(),
            source_backup_sha256: "0".repeat(64),
            source_schema_version: AUTH_SCHEMA_VERSION,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: 0,
            table_hashes: vec![SqliteImportTableDigest {
                table: "settings".to_owned(),
                row_count: 0,
                sha256: "0".repeat(64),
            }],
            admitted_role: Some(ClusterRole::Learner),
        };

        let interrupted = publish_join_activation(
            dir.path(),
            &active,
            &marker,
            &membership,
            JoinActivationFailpoint::AfterMembership,
        )
        .expect_err("failpoint interrupts before activation marker");
        assert!(interrupted.to_string().contains("failpoint"));
        assert_eq!(
            read_local_membership(dir.path())
                .expect("read durable membership")
                .expect("membership was published")
                .role,
            ClusterRole::Learner
        );
        assert!(!active.join(ACTIVATION_MARKER_FILENAME).exists());

        publish_join_activation(
            dir.path(),
            &active,
            &marker,
            &membership,
            JoinActivationFailpoint::None,
        )
        .expect("retry publishes the marker");
        let resumed_marker = read_activation_marker(&active).expect("read activation marker");
        assert_eq!(
            active_store_role(&resumed_marker, Some(&membership), dir.path())
                .expect("resume the admitted role"),
            ClusterRole::Learner
        );

        std::fs::remove_file(dir.path().join(LOCAL_MEMBERSHIP_FILENAME))
            .expect("simulate later membership loss");
        assert!(
            active_store_role(&resumed_marker, None, dir.path()).is_err(),
            "an activated learner with a missing role record must fail closed"
        );
    }

    /// The whole learner admission path against a real replicated cluster.
    ///
    /// A learner is only useful if it is provably harmless, so this asserts the
    /// harm it must not do rather than only that it started: it carries no
    /// vote, its own membership handle refuses it leader-singleton work, its
    /// durable role survives a restart, and while it exists the cluster refuses
    /// to deactivate the protocol that admitted it.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_learner_joins_only_after_activation_and_never_gains_a_vote() {
        install_default_crypto_provider();

        let source_dir = tempfile::tempdir().expect("source data dir");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("join coordinator listener");
        let coordinator_addr = listener.local_addr().expect("coordinator address");
        let mut source_config = membership_test_config(source_dir.path());
        source_config.server.bind = coordinator_addr;
        source_config.cluster.join_url = format!("http://{coordinator_addr}");
        drop(SqliteStore::open(&source_dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
        let source = select_daemon_store(&source_config)
            .await
            .expect("activate the source voter");
        let coordinator = source.membership_manager();
        let source_client = source.local_client.clone().expect("source client");
        let app = Router::new()
            .route("/api/v1/cluster/join/redeem", post(redeem_join_for_test))
            .route(
                "/api/v1/cluster/learner/join/redeem",
                post(redeem_learner_join_for_test),
            )
            .route(
                "/api/v1/cluster/join/finalize",
                post(finalize_join_for_test),
            )
            .route(
                "/api/v1/cluster/learner/join/finalize",
                post(finalize_learner_join_for_test),
            )
            .with_state(coordinator.clone());
        let http_task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve join coordinator");
        });

        // 1. Before activation there is no learner protocol to admit anyone
        //    with, and the refusal happens at issuance — an operator finds out
        //    before they have copied a token to another machine.
        assert_eq!(
            coordinator
                .issue_learner_token(Duration::from_secs(120))
                .await
                .expect_err("an unactivated cluster cannot mint a learner token")
                .code(),
            "learner_protocol_inactive"
        );
        // A voter token is still perfectly ordinary, and still v1.
        let voter_token = coordinator
            .issue_token(Duration::from_secs(120))
            .await
            .expect("issue a voter token on an unactivated cluster");
        assert!(voter_token.token.starts_with("plxjoin:v1:"));

        // 2. Activation is the explicit step that changes this.
        assert!(
            coordinator
                .activate_learner_protocol()
                .await
                .expect("activate the learner protocol")
                .changed
        );
        let learner_token = coordinator
            .issue_learner_token(Duration::from_secs(120))
            .await
            .expect("issue a learner token on an activated cluster");
        assert!(learner_token.token.starts_with("plxjoin:v2:"));

        // A pre-P6 coordinator knows only the voter route and its old SQL
        // transition. Replicated state must reject that transition even if
        // the old HTTP decoder would otherwise ignore a caller's learner
        // field and proceed with the stored digest.
        let learner_digest = join_token_digest(&learner_token.token);
        let old_coordinator = source_client
            .execute(
                "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = $1 \
                 WHERE token_hash = $2 AND state = 'issued'",
                hiqlite::params!("old-coordinator", learner_digest.as_str()),
            )
            .await
            .expect_err("the voter-path transition must reject a learner token");
        assert!(
            old_coordinator
                .to_string()
                .contains("learner token requires v2 admission"),
            "unexpected old-coordinator refusal: {old_coordinator}"
        );

        // 3. The join itself, through the operator's real entry point.
        let learner_dir = tempfile::tempdir().expect("learner data dir");
        let token_path = learner_dir.path().join("join.token");
        std::fs::write(&token_path, format!("{}\n", learner_token.token))
            .expect("learner token file");
        let mut learner_config = membership_test_config(learner_dir.path());
        learner_config.cluster.join_token_file = token_path.clone();
        let learner = select_daemon_store(&learner_config)
            .await
            .expect("admit the learner");
        assert_eq!(learner.identity.raft_id, learner_token.raft_id);
        assert!(
            !token_path.exists(),
            "a finalized learner join consumes its token"
        );

        // 4. It is a committed member and it carries no vote. This is the
        //    property Raft itself enforces: a non-voter cannot campaign.
        let metrics = source_client
            .metrics_db()
            .await
            .expect("read committed membership");
        assert!(
            metrics
                .membership_config
                .nodes()
                .any(|(raft_id, _)| *raft_id == learner_token.raft_id),
            "the learner must be a committed member"
        );
        assert!(
            !metrics
                .membership_config
                .voter_ids()
                .any(|raft_id| raft_id == learner_token.raft_id),
            "the learner must not have acquired a vote"
        );
        assert!(
            metrics.membership_config.voter_ids().count() == 1,
            "admitting a learner must not change quorum size"
        );

        // 5. Its own membership handle refuses it every leader-singleton job,
        //    from live committed membership rather than a boot flag.
        let learner_membership = learner.membership_manager();
        assert!(!learner_membership
            .local_node_is_committed_voter()
            .await
            .expect("read the learner's committed role"));
        assert!(!learner_membership.may_run_cluster_jobs().await);
        assert!(
            coordinator.may_run_cluster_jobs().await,
            "the voter is unaffected"
        );

        // 6. The durable role is in replicated membership, where deactivation
        //    can see it.
        let roles = source_client
            .query_map::<NodeRoleRow, _>(
                "SELECT node_id, role FROM cluster_nodes ORDER BY raft_id",
                hiqlite::params!(),
            )
            .await
            .expect("read the durable roles");
        let learner_rows = roles
            .iter()
            .filter(|row| row.role.as_deref() == Some("learner"))
            .map(|row| row.node_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            learner_rows,
            vec![learner.identity.node_id.clone()],
            "exactly the joined node is durably a learner: {roles:?}"
        );
        match coordinator.deactivate_learner_protocol().await {
            Err(super::super::membership::MembershipError::LearnerProtocolInUse {
                admitted,
                non_voting,
            }) => {
                assert!(
                    admitted.contains(&learner.identity.node_id),
                    "the refusal must name the learner it protects: {admitted:?}"
                );
                assert_eq!(
                    non_voting,
                    vec![learner.identity.raft_id.to_string()],
                    "and must label the Raft-membership roster as its own claim"
                );
            }
            other => panic!("deactivation must be refused while a learner exists: {other:?}"),
        }
        assert_eq!(
            coordinator
                .active_protocol_range()
                .await
                .expect("read the range"),
            (
                crate::store::AUTH_LEARNER_PROTOCOL,
                crate::store::AUTH_LEARNER_PROTOCOL
            ),
            "a refused deactivation must not move the range"
        );

        // 6b. And the removal endpoint an operator reaches for next says the
        //     same thing. It used to answer "not found" for this node — a 404
        //     for a row the roster is printing, which reads as "you typed the
        //     wrong id" rather than "this release cannot remove it". The
        //     refusal has to name the node and be its own code.
        let refused_removal = coordinator
            .remove_voter(&learner.identity.node_id)
            .await
            .expect_err("this release has no removal path for a member with no vote");
        assert_eq!(
            refused_removal.code(),
            "cluster_non_voter_removal_unsupported",
            "a learner the roster lists must not be reported as missing: {refused_removal}"
        );
        assert!(
            refused_removal
                .to_string()
                .contains(&learner.identity.node_id),
            "the refusal must name the node the operator asked about: {refused_removal}"
        );
        assert_eq!(
            coordinator
                .remove_voter("a-node-id-that-was-never-in-this-cluster")
                .await
                .expect_err("an unknown node is still not found")
                .code(),
            "cluster_node_not_found",
            "and the genuine 404 must keep answering 404"
        );

        // 7. A learner never advances replicated schema. Rewind the marker
        //    one step and make the learner's own boot call — `open`, not
        //    `open_or_migrate` — against its own client: it must refuse and
        //    leave the marker exactly where it found it. What a voter's boot
        //    call does with the same marker is covered by the schema
        //    migration tests in `store::hiqlite`; what matters here is that
        //    the learner is not the node that does it.
        let learner_client = learner.local_client.clone().expect("learner client");
        let previous_schema = crate::store::AUTH_SCHEMA_VERSION - 1;
        source_client
            .execute(
                "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
                hiqlite::params!(previous_schema),
            )
            .await
            .expect("rewind the replicated schema marker");
        let refused = HiqliteAuthStore::open(
            learner_client.clone(),
            &learner_dir.path().join("learner-migration-probe.db"),
        )
        .await
        .err()
        .expect("a learner must refuse a schema it does not implement")
        .to_string();
        assert!(refused.contains("schema"), "{refused}");
        assert_eq!(
            replicated_schema_version(&source_client).await,
            previous_schema,
            "a learner must not advance replicated schema"
        );
        source_client
            .execute(
                "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
                hiqlite::params!(crate::store::AUTH_SCHEMA_VERSION),
            )
            .await
            .expect("restore the replicated schema marker");

        // 7. The role is durable across a restart: membership.json carries it,
        //    and the restarted process must not wait for a promotion it was
        //    admitted specifically not to receive.
        let persisted = read_local_membership(learner_dir.path())
            .expect("read the learner record")
            .expect("a joined node persists one");
        assert_eq!(persisted.role, ClusterRole::Learner);
        assert_eq!(persisted.version, 2);
        learner.shutdown().await.expect("drain the learner");
        drop(learner);

        // The record a restart reads, read back through the same function both
        // boot paths use to resume a role.
        let resumed = read_local_membership(learner_dir.path())
            .expect("read the record a restart would resume from")
            .expect("still present after shutdown");
        assert_eq!(boot_cluster_role(Some(&resumed)), ClusterRole::Learner);

        // And the restart itself gets as far as binding its listeners, which
        // is downstream of reading and accepting that version 2 record — the
        // previous release refuses it before this point. It stops there only
        // because Hiqlite 0.14 keeps its TLS listener task alive until the
        // test runtime exits, so this process cannot free its own ports; a
        // real daemon restart has them back.
        let restart = select_daemon_store(&learner_config)
            .await
            .err()
            .expect("an in-process restart cannot rebind Hiqlite's leaked listener")
            .to_string();
        assert!(
            restart.contains("is not available"),
            "the restart must fail on the port and not on the learner record: {restart}"
        );

        http_task.abort();
        source.shutdown().await.expect("drain the source voter");
    }

    /// A learner never proposes a replicated schema migration.
    ///
    /// The boot path picks how to open the store from the role this node was
    /// admitted with, and swapping the learner arm to `open_or_migrate`
    /// survived everywhere — including the separate-process harness, which
    /// never sets up a learner joining a cluster whose replicated schema is
    /// behind. Nothing looked at the one thing that distinguishes the arms.
    ///
    /// So construct exactly that: rewind the cluster's replicated schema
    /// marker, then open the store both ways against the same live cluster.
    /// The learner has to refuse and leave the marker where it found it; the
    /// voter has to advance it. Either arm collapsing onto the other fails
    /// one of the two halves.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_learner_refuses_a_behind_schema_that_a_voter_migrates() {
        install_default_crypto_provider();

        let dir = tempfile::tempdir().expect("schema role data dir");
        let config = membership_test_config(dir.path());
        drop(SqliteStore::open(&dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
        let selected = select_daemon_store(&config)
            .await
            .expect("activate a one-voter cluster");
        let client = selected
            .local_client
            .as_ref()
            .expect("voter client")
            .clone();

        let current = replicated_schema_version(&client).await;
        assert_eq!(current, crate::store::AUTH_SCHEMA_VERSION);
        let behind = current - 1;
        client
            .execute(
                "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
                hiqlite::params!(behind),
            )
            .await
            .expect("rewind the replicated schema marker");

        // The learner arm: refuse, and change nothing. The refusal is asserted
        // in full, because that is what distinguishes the arms — swapping this
        // to `open_or_migrate` also fails here, but on a migration step rather
        // than on the compatibility check, and an operator reading "duplicate
        // column name" learns nothing about what their learner is waiting for.
        let refused = open_store_for_role(
            ClusterRole::Learner,
            client.clone(),
            &dir.path().join("learner-probe.db"),
        )
        .await
        .err()
        .expect("a learner must refuse a replicated schema it does not implement")
        .to_string();
        assert!(
            refused.contains(&format!(
                "cluster schema {behind} is incompatible with voter schema {}",
                crate::store::AUTH_SCHEMA_VERSION
            )),
            "the learner must refuse on the compatibility check, not inside a migration: \
             {refused}"
        );
        assert_eq!(
            replicated_schema_version(&client).await,
            behind,
            "a learner must not advance replicated schema"
        );

        // And the refusal was about the schema, not about learners: put the
        // marker back and the same arm opens the same cluster.
        client
            .execute(
                "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
                hiqlite::params!(crate::store::AUTH_SCHEMA_VERSION),
            )
            .await
            .expect("restore the replicated schema marker");
        drop(
            open_store_for_role(
                ClusterRole::Learner,
                client.clone(),
                &dir.path().join("learner-probe.db"),
            )
            .await
            .expect("a learner opens a schema it does implement"),
        );

        // The voter arm is the one that may migrate, and it opens the same
        // cluster through `open_or_migrate`.
        drop(
            open_store_for_role(
                ClusterRole::Voter,
                client.clone(),
                &dir.path().join("voter-probe.db"),
            )
            .await
            .expect("a voter opens the cluster it is responsible for migrating"),
        );
        assert_eq!(
            replicated_schema_version(&client).await,
            crate::store::AUTH_SCHEMA_VERSION
        );

        selected.shutdown().await.expect("stop the voter");
    }

    /// Activation is decided on the binary each voter is running *now*.
    ///
    /// Both refusals below are constructed against a live replicated cluster:
    /// one voter whose capability row was never written, and one whose row
    /// survives from a build that was rolled back. The second is the case the
    /// heartbeat coupling exists for — the row is present and looks like a
    /// proof until it is compared with that node's current heartbeat.
    #[cfg(feature = "hiqlite-store")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn activation_refuses_a_voter_whose_running_binary_is_unproven() {
        install_default_crypto_provider();

        let dir = tempfile::tempdir().expect("unproven voter data dir");
        let config = membership_test_config(dir.path());
        drop(SqliteStore::open(&dir.path().join(SQLITE_FILENAME)).expect("source SQLite"));
        let selected = select_daemon_store(&config)
            .await
            .expect("activate a one-voter cluster");
        let membership = selected.membership_manager();
        let client = selected
            .local_client
            .as_ref()
            .expect("voter client")
            .clone();

        // A second node that has heartbeated but never proved the capability,
        // standing in for a voter still running the previous release. Its
        // heartbeats are *current* on purpose: activation also refuses for a
        // node that has gone silent, and this test is about the capability
        // rule, not that one.
        let beat = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64;
        client
            .execute(
                "INSERT INTO cluster_nodes \
                 (node_id, raft_id, raft_address, api_address, last_seen_at, removed_at) \
                 VALUES ($1, $2, $3, $4, $5, NULL)",
                hiqlite::params!("unproven-voter", 77_i64, "127.0.0.1:1", "127.0.0.1:2", beat),
            )
            .await
            .expect("seed a voter running an older binary");
        client
            .execute(
                "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 VALUES ($1, $2, $3)",
                hiqlite::params!("unproven-voter", "membership_removal_attempt_refs_v1", beat),
            )
            .await
            .expect("that binary does write the capability it knows about");

        match membership.activate_learner_protocol().await {
            Err(super::super::membership::MembershipError::LearnerProtocolUpgradeRequired(
                nodes,
            )) => {
                assert_eq!(nodes, vec!["unproven-voter".to_owned()]);
            }
            other => panic!("an unproven voter must block activation: {other:?}"),
        }
        assert_eq!(
            membership
                .active_protocol_range()
                .await
                .expect("read the range"),
            (
                crate::store::AUTH_PROTOCOL_MIN,
                crate::store::AUTH_PROTOCOL_MIN
            ),
            "a refused activation must not move the range"
        );

        // Now the deliberate stale case: the node proves the capability, then
        // heartbeats again on a binary that does not know about it. The row is
        // still there; only its timestamp says it is no longer current.
        client
            .execute(
                "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 VALUES ($1, $2, $3)",
                hiqlite::params!("unproven-voter", "learner_protocol_v5", beat),
            )
            .await
            .expect("the node proves the learner protocol once");
        assert!(
            membership
                .protocol_status()
                .await
                .expect("status after the proof")
                .learner_protocol_pending
                .is_empty(),
            "the cluster is briefly ready"
        );
        client
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = $1 WHERE node_id = $2",
                hiqlite::params!(beat + 1, "unproven-voter"),
            )
            .await
            .expect("the rolled-back binary heartbeats");

        match membership.activate_learner_protocol().await {
            Err(super::super::membership::MembershipError::LearnerProtocolUpgradeRequired(
                nodes,
            )) => {
                assert_eq!(
                    nodes,
                    vec!["unproven-voter".to_owned()],
                    "a capability older than its node's heartbeat is not a current proof"
                );
            }
            other => panic!("a stale capability must block activation: {other:?}"),
        }
        assert_eq!(
            membership
                .active_protocol_range()
                .await
                .expect("read the range"),
            (
                crate::store::AUTH_PROTOCOL_MIN,
                crate::store::AUTH_PROTOCOL_MIN
            )
        );

        selected.shutdown().await.expect("stop the voter");
    }
}

#[cfg(feature = "hiqlite-store")]
pub mod status {
    //! User-facing replication status derived from the selected durable backend.
    //!
    //! This is deliberately a read-only projection. Membership changes belong to
    //! M3; this module only turns the backend and Raft metrics the daemon already
    //! has into an honest answer about watch-state convergence.

    use std::collections::BTreeSet;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Serialize};

    pub use hiqlite::{
        BoundedWalError, DbSnapshotHistogram, DbSnapshotLastOutcome, DbSnapshotMetricsSnapshot,
        WalRecoveryObservation, WalRuntimeState, WalStatusSnapshot,
        DB_SNAPSHOT_HISTOGRAM_BOUNDS_NANOS,
    };
    use hiqlite::{
        Client, DbQuorumWatermark, LocalDbRaftMetrics, LocalDbRaftSnapshot, LocalDbSnapshotMetrics,
    };
    use std::sync::{Arc, Mutex};

    const PASSIVE_METRICS_REFRESH: Duration = Duration::from_secs(5);
    const PASSIVE_METRICS_FRESHNESS_SECS: u64 = 15;
    const QUORUM_WATERMARK_REFRESH: Duration = Duration::from_millis(500);
    const QUORUM_WATERMARK_TIMEOUT: Duration = Duration::from_millis(750);
    const QUORUM_WATERMARK_LEASE: Duration = Duration::from_secs(1);

    /// The durable backend serving this daemon process.
    #[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ReplicationBackend {
        Sqlite,
        Replicated,
    }

    /// The conclusion an operator should act on, not a raw Raft server state.
    #[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ReplicationHealth {
        /// SQLite has no replication peer and must not be labelled "synced".
        SingleNode,
        InSync,
        Degraded,
    }

    /// Safe, user-visible watch-state replication status.
    ///
    /// It carries no node address, media path, account, token, or library data.
    #[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
    pub struct ReplicationStatus {
        pub backend: ReplicationBackend,
        pub health: ReplicationHealth,
        /// More than one voter is present. The membership roster belongs to M3.
        pub clustered: bool,
        /// Raft term/index of the last entry applied to this node.
        pub last_applied_term: Option<u64>,
        pub last_applied_index: Option<u64>,
        /// Largest observed gap, or a conservative upper bound for a missing peer.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub behind_by: Option<u64>,
        /// Unix seconds when this process last positively observed convergence.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub last_converged_at: Option<i64>,
        /// Process-local applied-index baseline retained across degraded samples.
        ///
        /// This is classifier state, not part of the public JSON contract.
        #[serde(skip)]
        last_converged_index: Option<u64>,
        /// Unix seconds when this projection was sampled.
        pub checked_at: i64,
        /// Plain-language interpretation for clients that do not speak Raft.
        pub explanation: String,
    }

    /// Backend-neutral facts needed to classify one replicated-store sample.
    ///
    /// The cluster harness constructs this from real three-voter metrics too, so
    /// the degraded contract exercises this production classifier rather than a
    /// test-only imitation.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ReplicationObservation {
        pub running: bool,
        pub leader_known: bool,
        pub voter_count: usize,
        pub last_log_index: Option<u64>,
        pub last_applied_term: Option<u64>,
        pub last_applied_index: Option<u64>,
        /// Leader-known match index for every other voter. `None` means the leader
        /// has not observed that voter at any index yet. Followers have no map.
        pub peer_matched_indexes: Option<Vec<Option<u64>>>,
    }

    /// One coherent local Raft sample. `last_applied_index` is intentionally
    /// not described as a commit index: OpenRaft's local watch does not prove a
    /// quorum watermark on a follower.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PassiveRaftSample {
        pub node_id: u64,
        pub current_term: u64,
        pub leader_id: Option<u64>,
        pub last_applied_index: Option<u64>,
        pub leader_known: bool,
        pub is_leader: bool,
    }

    /// Privacy-safe projection of the latest quorum-confirmed commit proof.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct QuorumWatermarkSample {
        pub committed_index: u64,
        pub apply_lag_entries: Option<u64>,
    }

    /// Store-free view consumed by the Prometheus handler.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PassiveRaftMetricsView {
        pub local_source: bool,
        pub sample: Option<PassiveRaftSample>,
        pub age_seconds: Option<u64>,
        pub valid: bool,
        pub errors: u64,
        pub leader_changes: u64,
        pub watermark_source: bool,
        /// `true` only when this proof is bound to this process's local Raft
        /// term, leader observation, epoch, and applied index. A remote
        /// authority-only proof must never be used for a bounded replica read.
        pub watermark_requires_local_binding: bool,
        pub watermark: Option<QuorumWatermarkSample>,
        pub watermark_age_millis: Option<u64>,
        pub watermark_valid: bool,
        /// The current watermark source implements this binary's bounded
        /// local-read protocol. False keeps rolling upgrades on Authority.
        pub watermark_local_reads_supported: bool,
        pub watermark_errors: u64,
        pub snapshot_metrics: Option<DbSnapshotMetricsSnapshot>,
    }

    /// Private, single-operation state retained across one local query.
    /// Callers never receive or replay this capability directly.
    struct BoundedReplicaPermit {
        metrics: PassiveRaftMetrics,
        current_term: u64,
        leader_id: u64,
        committed_index: u64,
        applied_index: u64,
        local_observation_epoch: u64,
        watermark_started_nanos: u64,
        max_apply_lag_entries: u64,
    }

    impl BoundedReplicaPermit {
        fn remains_valid(&self) -> bool {
            let elapsed = self.metrics.started_at.elapsed();
            self.remains_valid_at(elapsed.as_secs(), duration_nanos(elapsed))
        }

        fn remains_valid_at(&self, elapsed_seconds: u64, elapsed_nanos: u64) -> bool {
            let original_deadline_valid = elapsed_nanos
                .saturating_sub(self.watermark_started_nanos)
                < duration_nanos(QUORUM_WATERMARK_LEASE);
            original_deadline_valid
                && self
                    .metrics
                    .bounded_replica_state_at(
                        elapsed_seconds,
                        elapsed_nanos,
                        self.max_apply_lag_entries,
                    )
                    .is_some_and(|current| {
                        current.current_term == self.current_term
                            && current.leader_id == self.leader_id
                            && current.committed_index >= self.committed_index
                            && current.local_observation_epoch == self.local_observation_epoch
                            && current.applied_index >= self.applied_index
                    })
        }
    }

    struct BoundedReplicaState {
        current_term: u64,
        leader_id: u64,
        committed_index: u64,
        applied_index: u64,
        local_observation_epoch: u64,
        watermark_started_nanos: u64,
    }

    #[derive(Default)]
    struct PassiveRaftMetricsAtomics {
        sequence: AtomicU64,
        published: AtomicBool,
        current_term: AtomicU64,
        node_id: AtomicU64,
        last_applied_present: AtomicBool,
        last_applied_index: AtomicU64,
        leader_known: AtomicBool,
        is_leader: AtomicBool,
        sampled_elapsed: AtomicU64,
        errors: AtomicU64,
        leader_changes: AtomicU64,
        last_known_leader_present: AtomicBool,
        last_known_leader: AtomicU64,
        current_leader_present: AtomicBool,
        current_leader: AtomicU64,
        local_observation_epoch: AtomicU64,
        watermark_published: AtomicBool,
        watermark_term: AtomicU64,
        watermark_leader: AtomicU64,
        watermark_committed_index: AtomicU64,
        watermark_local_read_protocol_version: AtomicU64,
        watermark_started_nanos: AtomicU64,
        watermark_local_epoch: AtomicU64,
        watermark_errors: AtomicU64,
        watermark_invalidated: AtomicBool,
        sampler_started: AtomicBool,
    }

    /// Narrow atomics-only handle for passive local Raft observability.
    #[derive(Clone)]
    pub struct PassiveRaftMetrics {
        inner: Arc<PassiveRaftMetricsAtomics>,
        started_at: Instant,
        local_source: bool,
        watermark_source: bool,
        watermark_requires_local_binding: bool,
        snapshot_metrics: Option<LocalDbSnapshotMetrics>,
    }

    impl PassiveRaftMetrics {
        fn new(local_source: bool) -> Self {
            Self {
                inner: Arc::new(PassiveRaftMetricsAtomics::default()),
                started_at: Instant::now(),
                local_source,
                watermark_source: local_source,
                watermark_requires_local_binding: local_source,
                snapshot_metrics: None,
            }
        }

        /// A distinct serving process has no local replica to bind or lag to.
        /// Its readiness may use only the short quorum authority lease; this
        /// mode deliberately exposes no local source and no apply-lag value.
        fn remote_authority() -> Self {
            Self {
                inner: Arc::new(PassiveRaftMetricsAtomics::default()),
                started_at: Instant::now(),
                local_source: false,
                watermark_source: true,
                watermark_requires_local_binding: false,
                snapshot_metrics: None,
            }
        }

        fn with_snapshot_metrics(mut self, metrics: Option<LocalDbSnapshotMetrics>) -> Self {
            self.snapshot_metrics = metrics;
            self
        }

        /// Construct one current local/quorum proof for deterministic Store
        /// boundary contracts. Production obtains both observations from the
        /// live passive sampler; this helper is absent without the validation
        /// feature.
        #[cfg(feature = "cluster-read-cost-validation")]
        #[doc(hidden)]
        #[must_use]
        pub fn validation_bounded_ready() -> Self {
            let metrics = Self::new(true);
            assert!(metrics.publish_at(
                &LocalDbRaftSnapshot {
                    running: true,
                    node_id: 1,
                    current_term: 7,
                    current_leader: Some(1),
                    last_applied_term: Some(7),
                    last_applied_index: Some(41),
                },
                0,
            ));
            assert!(metrics.publish_watermark_at(
                DbQuorumWatermark {
                    term: 7,
                    leader_id: 1,
                    committed_index: 41,
                    local_read_protocol_version: hiqlite::DB_LOCAL_READ_PROTOCOL_VERSION,
                },
                0,
                0,
            ));
            metrics
        }

        /// Revoke the current proof inside a validation-only local operation,
        /// exercising production's post-query discard and Authority fallback.
        #[cfg(feature = "cluster-read-cost-validation")]
        #[doc(hidden)]
        pub fn validation_revoke_bounded_proof(&self) {
            let sequence = self.begin_write();
            self.inner
                .watermark_invalidated
                .store(true, Ordering::Relaxed);
            self.end_write(sequence);
        }

        /// Read one coherent snapshot without locks, Store access, or IO.
        #[must_use]
        pub fn snapshot(&self) -> PassiveRaftMetricsView {
            let elapsed = self.started_at.elapsed();
            self.snapshot_at_times(elapsed.as_secs(), duration_nanos(elapsed))
        }

        /// Run exactly one local read between bounded-replica issue and
        /// revalidation boundaries.
        ///
        /// Remote-only serving processes and SQLite backends always return
        /// `None` without invoking `local_read`. A result is discarded when
        /// the proof expires, changes generation, or exceeds the entry budget
        /// while the query is running. Store code must then perform its named
        /// authority fallback.
        pub(crate) async fn run_bounded_replica<T, F, Fut>(
            &self,
            max_apply_lag_entries: u64,
            local_read: F,
        ) -> Option<T>
        where
            F: FnOnce() -> Fut,
            Fut: Future<Output = T>,
        {
            let elapsed = self.started_at.elapsed();
            let permit = self.try_bounded_replica_at(
                elapsed.as_secs(),
                duration_nanos(elapsed),
                max_apply_lag_entries,
            )?;
            let result = local_read().await;
            permit.remains_valid().then_some(result)
        }

        fn try_bounded_replica_at(
            &self,
            elapsed_seconds: u64,
            elapsed_nanos: u64,
            max_apply_lag_entries: u64,
        ) -> Option<BoundedReplicaPermit> {
            let state = self.bounded_replica_state_at(
                elapsed_seconds,
                elapsed_nanos,
                max_apply_lag_entries,
            )?;
            Some(BoundedReplicaPermit {
                metrics: self.clone(),
                current_term: state.current_term,
                leader_id: state.leader_id,
                committed_index: state.committed_index,
                applied_index: state.applied_index,
                local_observation_epoch: state.local_observation_epoch,
                watermark_started_nanos: state.watermark_started_nanos,
                max_apply_lag_entries,
            })
        }

        fn bounded_replica_state_at(
            &self,
            elapsed_seconds: u64,
            elapsed_nanos: u64,
            max_apply_lag_entries: u64,
        ) -> Option<BoundedReplicaState> {
            if !self.local_source
                || !self.watermark_source
                || !self.watermark_requires_local_binding
            {
                return None;
            }
            loop {
                let before = self.inner.sequence.load(Ordering::Acquire);
                if before & 1 != 0 {
                    std::hint::spin_loop();
                    continue;
                }
                let published = self.inner.published.load(Ordering::Relaxed);
                let sampled_elapsed = self.inner.sampled_elapsed.load(Ordering::Relaxed);
                let current_term = self.inner.current_term.load(Ordering::Relaxed);
                let last_applied_present = self.inner.last_applied_present.load(Ordering::Relaxed);
                let applied_index = self.inner.last_applied_index.load(Ordering::Relaxed);
                let leader_known = self.inner.leader_known.load(Ordering::Relaxed);
                let current_leader_present =
                    self.inner.current_leader_present.load(Ordering::Relaxed);
                let leader_id = self.inner.current_leader.load(Ordering::Relaxed);
                let local_observation_epoch =
                    self.inner.local_observation_epoch.load(Ordering::Relaxed);
                let watermark_published = self.inner.watermark_published.load(Ordering::Relaxed);
                let watermark_term = self.inner.watermark_term.load(Ordering::Relaxed);
                let watermark_leader = self.inner.watermark_leader.load(Ordering::Relaxed);
                let committed_index = self.inner.watermark_committed_index.load(Ordering::Relaxed);
                let local_read_protocol_version = self
                    .inner
                    .watermark_local_read_protocol_version
                    .load(Ordering::Relaxed);
                let watermark_started_nanos =
                    self.inner.watermark_started_nanos.load(Ordering::Relaxed);
                let watermark_local_epoch =
                    self.inner.watermark_local_epoch.load(Ordering::Relaxed);
                let watermark_invalidated =
                    self.inner.watermark_invalidated.load(Ordering::Relaxed);
                let after = self.inner.sequence.load(Ordering::Acquire);
                if before != after {
                    continue;
                }
                let local_valid = published
                    && elapsed_seconds.saturating_sub(sampled_elapsed)
                        <= PASSIVE_METRICS_FRESHNESS_SECS;
                let watermark_valid = watermark_published
                    && elapsed_nanos.saturating_sub(watermark_started_nanos)
                        < duration_nanos(QUORUM_WATERMARK_LEASE)
                    && !watermark_invalidated;
                let local_binding_valid = last_applied_present
                    && leader_known
                    && current_leader_present
                    && current_term == watermark_term
                    && leader_id == watermark_leader
                    && local_observation_epoch == watermark_local_epoch;
                let apply_lag_entries = committed_index.saturating_sub(applied_index);
                return (local_valid
                    && watermark_valid
                    && local_binding_valid
                    && local_read_protocol_version == hiqlite::DB_LOCAL_READ_PROTOCOL_VERSION
                    && apply_lag_entries <= max_apply_lag_entries)
                    .then_some(BoundedReplicaState {
                        current_term,
                        leader_id,
                        committed_index,
                        applied_index,
                        local_observation_epoch,
                        watermark_started_nanos,
                    });
            }
        }

        #[cfg(test)]
        fn snapshot_at(&self, elapsed: u64) -> PassiveRaftMetricsView {
            self.snapshot_at_times(elapsed, elapsed.saturating_mul(1_000_000_000))
        }

        fn snapshot_at_times(
            &self,
            elapsed_seconds: u64,
            elapsed_nanos: u64,
        ) -> PassiveRaftMetricsView {
            loop {
                let before = self.inner.sequence.load(Ordering::Acquire);
                if before & 1 != 0 {
                    std::hint::spin_loop();
                    continue;
                }
                let published = self.inner.published.load(Ordering::Relaxed);
                let sampled_elapsed = self.inner.sampled_elapsed.load(Ordering::Relaxed);
                let current_term = self.inner.current_term.load(Ordering::Relaxed);
                let last_applied_present = self.inner.last_applied_present.load(Ordering::Relaxed);
                let last_applied_index = self.inner.last_applied_index.load(Ordering::Relaxed);
                let leader_known = self.inner.leader_known.load(Ordering::Relaxed);
                let is_leader = self.inner.is_leader.load(Ordering::Relaxed);
                let errors = self.inner.errors.load(Ordering::Relaxed);
                let leader_changes = self.inner.leader_changes.load(Ordering::Relaxed);
                let current_leader_present =
                    self.inner.current_leader_present.load(Ordering::Relaxed);
                let current_leader = self.inner.current_leader.load(Ordering::Relaxed);
                let local_observation_epoch =
                    self.inner.local_observation_epoch.load(Ordering::Relaxed);
                let watermark_published = self.inner.watermark_published.load(Ordering::Relaxed);
                let watermark_term = self.inner.watermark_term.load(Ordering::Relaxed);
                let watermark_leader = self.inner.watermark_leader.load(Ordering::Relaxed);
                let watermark_committed_index =
                    self.inner.watermark_committed_index.load(Ordering::Relaxed);
                let watermark_local_read_protocol_version = self
                    .inner
                    .watermark_local_read_protocol_version
                    .load(Ordering::Relaxed);
                let watermark_started_nanos =
                    self.inner.watermark_started_nanos.load(Ordering::Relaxed);
                let watermark_local_epoch =
                    self.inner.watermark_local_epoch.load(Ordering::Relaxed);
                let watermark_errors = self.inner.watermark_errors.load(Ordering::Relaxed);
                let watermark_invalidated =
                    self.inner.watermark_invalidated.load(Ordering::Relaxed);
                let after = self.inner.sequence.load(Ordering::Acquire);
                if before == after {
                    let age_seconds =
                        published.then(|| elapsed_seconds.saturating_sub(sampled_elapsed));
                    let local_valid =
                        age_seconds.is_some_and(|age| age <= PASSIVE_METRICS_FRESHNESS_SECS);
                    let watermark_age_nanos = watermark_published
                        .then(|| elapsed_nanos.saturating_sub(watermark_started_nanos));
                    let local_binding_valid = !self.watermark_requires_local_binding
                        || (published
                            && local_valid
                            && last_applied_present
                            && leader_known
                            && current_leader_present
                            && current_term == watermark_term
                            && current_leader == watermark_leader
                            && local_observation_epoch == watermark_local_epoch);
                    let watermark_valid = self.watermark_source
                        && watermark_age_nanos
                            .is_some_and(|age| age < duration_nanos(QUORUM_WATERMARK_LEASE))
                        && !watermark_invalidated
                        && local_binding_valid;
                    let apply_lag_entries = (watermark_valid
                        && self.watermark_requires_local_binding
                        && last_applied_present)
                        .then(|| watermark_committed_index.saturating_sub(last_applied_index));
                    return PassiveRaftMetricsView {
                        local_source: self.local_source,
                        sample: published.then_some(PassiveRaftSample {
                            node_id: self.inner.node_id.load(Ordering::Relaxed),
                            current_term,
                            leader_id: current_leader_present.then_some(current_leader),
                            last_applied_index: last_applied_present.then_some(last_applied_index),
                            leader_known,
                            is_leader,
                        }),
                        age_seconds,
                        valid: local_valid,
                        errors,
                        leader_changes,
                        watermark_source: self.watermark_source,
                        watermark_requires_local_binding: self.watermark_requires_local_binding,
                        watermark: watermark_published.then_some(QuorumWatermarkSample {
                            committed_index: watermark_committed_index,
                            apply_lag_entries,
                        }),
                        watermark_age_millis: watermark_age_nanos.map(|age| age / 1_000_000),
                        watermark_valid,
                        watermark_local_reads_supported: watermark_published
                            && watermark_local_read_protocol_version
                                == hiqlite::DB_LOCAL_READ_PROTOCOL_VERSION,
                        watermark_errors,
                        snapshot_metrics: self.snapshot_metrics.map(|metrics| metrics.snapshot()),
                    };
                }
            }
        }

        fn publish(&self, source: &LocalDbRaftSnapshot) -> bool {
            self.publish_at(source, self.started_at.elapsed().as_secs())
        }

        fn publish_at(&self, source: &LocalDbRaftSnapshot, elapsed: u64) -> bool {
            if !source.running {
                self.record_error();
                return false;
            }
            if self.inner.published.load(Ordering::Acquire) {
                let prior_term = self.inner.current_term.load(Ordering::Relaxed);
                let prior_applied = self
                    .inner
                    .last_applied_present
                    .load(Ordering::Relaxed)
                    .then(|| self.inner.last_applied_index.load(Ordering::Relaxed));
                let regressed = source.current_term < prior_term
                    || (prior_applied.is_some() && source.last_applied_index.is_none())
                    || matches!(
                        (prior_applied, source.last_applied_index),
                        (Some(prior), Some(current)) if current < prior
                    );
                if regressed {
                    self.record_error();
                    return false;
                }
            }

            let sequence = self.begin_write();
            let was_published = self.inner.published.load(Ordering::Relaxed);
            let prior_leader = self
                .inner
                .current_leader_present
                .load(Ordering::Relaxed)
                .then(|| self.inner.current_leader.load(Ordering::Relaxed));
            let observation_changed = !was_published
                || source.current_term != self.inner.current_term.load(Ordering::Relaxed)
                || source.current_leader != prior_leader;
            if observation_changed {
                saturating_increment(&self.inner.local_observation_epoch);
            }
            if observation_changed && self.inner.watermark_published.load(Ordering::Relaxed) {
                self.inner
                    .watermark_invalidated
                    .store(true, Ordering::Relaxed);
            }
            if let Some(leader) = source.current_leader {
                if self
                    .inner
                    .last_known_leader_present
                    .swap(true, Ordering::Relaxed)
                {
                    let previous = self.inner.last_known_leader.swap(leader, Ordering::Relaxed);
                    if previous != leader {
                        saturating_increment(&self.inner.leader_changes);
                    }
                } else {
                    self.inner
                        .last_known_leader
                        .store(leader, Ordering::Relaxed);
                }
            }
            self.inner
                .current_term
                .store(source.current_term, Ordering::Relaxed);
            self.inner.node_id.store(source.node_id, Ordering::Relaxed);
            self.inner
                .current_leader_present
                .store(source.current_leader.is_some(), Ordering::Relaxed);
            self.inner
                .current_leader
                .store(source.current_leader.unwrap_or_default(), Ordering::Relaxed);
            self.inner
                .last_applied_present
                .store(source.last_applied_index.is_some(), Ordering::Relaxed);
            self.inner.last_applied_index.store(
                source.last_applied_index.unwrap_or_default(),
                Ordering::Relaxed,
            );
            self.inner
                .leader_known
                .store(source.current_leader.is_some(), Ordering::Relaxed);
            self.inner.is_leader.store(
                source.current_leader == Some(source.node_id),
                Ordering::Relaxed,
            );
            self.inner.sampled_elapsed.store(elapsed, Ordering::Relaxed);
            self.inner.published.store(true, Ordering::Relaxed);
            self.end_write(sequence);
            true
        }

        fn record_error(&self) {
            let sequence = self.begin_write();
            saturating_increment(&self.inner.errors);
            self.inner
                .watermark_invalidated
                .store(true, Ordering::Relaxed);
            self.end_write(sequence);
        }

        fn elapsed_nanos(&self) -> u64 {
            duration_nanos(self.started_at.elapsed())
        }

        fn publish_watermark(&self, source: DbQuorumWatermark, started_nanos: u64) -> bool {
            self.publish_watermark_at(source, started_nanos, self.elapsed_nanos())
        }

        fn publish_watermark_at(
            &self,
            source: DbQuorumWatermark,
            started_nanos: u64,
            elapsed_nanos: u64,
        ) -> bool {
            let sequence = self.begin_write();
            let expired = elapsed_nanos.saturating_sub(started_nanos)
                >= duration_nanos(QUORUM_WATERMARK_LEASE);
            let local_matches = !self.watermark_requires_local_binding
                || (self.inner.published.load(Ordering::Relaxed)
                    && self.inner.leader_known.load(Ordering::Relaxed)
                    && self.inner.current_leader_present.load(Ordering::Relaxed)
                    && self.inner.current_term.load(Ordering::Relaxed) == source.term
                    && self.inner.current_leader.load(Ordering::Relaxed) == source.leader_id);
            let prior_published = self.inner.watermark_published.load(Ordering::Relaxed);
            let prior_term = self.inner.watermark_term.load(Ordering::Relaxed);
            let prior_leader = self.inner.watermark_leader.load(Ordering::Relaxed);
            let successor_or_conflicting_generation = prior_published
                && (source.term > prior_term
                    || (source.term == prior_term && source.leader_id != prior_leader));
            let regressed = prior_published
                && (source.term < prior_term
                    || source.committed_index
                        < self.inner.watermark_committed_index.load(Ordering::Relaxed)
                    || (source.term == prior_term && source.leader_id != prior_leader));
            if expired || !local_matches || regressed {
                saturating_increment(&self.inner.watermark_errors);
                if successor_or_conflicting_generation {
                    self.inner
                        .watermark_invalidated
                        .store(true, Ordering::Relaxed);
                }
                self.end_write(sequence);
                return false;
            }
            self.inner
                .watermark_term
                .store(source.term, Ordering::Relaxed);
            self.inner
                .watermark_leader
                .store(source.leader_id, Ordering::Relaxed);
            self.inner
                .watermark_committed_index
                .store(source.committed_index, Ordering::Relaxed);
            self.inner
                .watermark_local_read_protocol_version
                .store(source.local_read_protocol_version, Ordering::Relaxed);
            self.inner
                .watermark_started_nanos
                .store(started_nanos, Ordering::Relaxed);
            self.inner.watermark_local_epoch.store(
                self.inner.local_observation_epoch.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            self.inner
                .watermark_invalidated
                .store(false, Ordering::Relaxed);
            self.inner
                .watermark_published
                .store(true, Ordering::Relaxed);
            self.end_write(sequence);
            true
        }

        fn record_watermark_error(&self) {
            let sequence = self.begin_write();
            saturating_increment(&self.inner.watermark_errors);
            self.end_write(sequence);
        }

        fn begin_write(&self) -> u64 {
            loop {
                let current = self.inner.sequence.load(Ordering::Acquire);
                if current & 1 == 0
                    && self
                        .inner
                        .sequence
                        .compare_exchange(
                            current,
                            current.wrapping_add(1),
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                {
                    return current;
                }
                std::hint::spin_loop();
            }
        }

        fn end_write(&self, sequence: u64) {
            self.inner
                .sequence
                .store(sequence.wrapping_add(2), Ordering::Release);
        }
    }

    fn saturating_increment(value: &AtomicU64) {
        let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(1))
        });
    }

    fn duration_nanos(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }

    #[async_trait::async_trait]
    trait PassiveRaftSource: Send {
        fn snapshot(&self) -> LocalDbRaftSnapshot;
        async fn wait_for_change(&mut self) -> bool;
    }

    #[async_trait::async_trait]
    impl PassiveRaftSource for LocalDbRaftMetrics {
        fn snapshot(&self) -> LocalDbRaftSnapshot {
            LocalDbRaftMetrics::snapshot(self)
        }

        async fn wait_for_change(&mut self) -> bool {
            LocalDbRaftMetrics::wait_for_change(self).await
        }
    }

    #[async_trait::async_trait]
    trait QuorumWatermarkSource: Send + Sync {
        async fn sample(&self) -> Result<DbQuorumWatermark, hiqlite::Error>;
    }

    #[async_trait::async_trait]
    impl QuorumWatermarkSource for Client {
        async fn sample(&self) -> Result<DbQuorumWatermark, hiqlite::Error> {
            self.db_quorum_watermark().await
        }
    }

    async fn run_quorum_watermark_loop<S, F>(
        source: S,
        metrics: PassiveRaftMetrics,
        shutdown: F,
        initial_delay: Duration,
        refresh_period: Duration,
        request_timeout: Duration,
    ) where
        S: QuorumWatermarkSource,
        F: Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        if !initial_delay.is_zero() {
            tokio::select! {
                biased;
                _ = &mut shutdown => return,
                _ = tokio::time::sleep(initial_delay) => {}
            }
        }
        let mut refresh = tokio::time::interval(refresh_period);
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => return,
                _ = refresh.tick() => {}
            }
            let started_nanos = metrics.elapsed_nanos();
            let result = tokio::select! {
                biased;
                _ = &mut shutdown => return,
                result = tokio::time::timeout(request_timeout, source.sample()) => result,
            };
            match result {
                Ok(Ok(watermark)) => {
                    if !metrics.publish_watermark(watermark, started_nanos) {
                        tracing::warn!(
                            term = watermark.term,
                            committed_index = watermark.committed_index,
                            "rejected quorum watermark that does not match local Raft state"
                        );
                    }
                }
                Ok(Err(error)) => {
                    metrics.record_watermark_error();
                    tracing::debug!(error = %error, "quorum watermark sample failed");
                }
                Err(_) => {
                    metrics.record_watermark_error();
                    tracing::debug!("quorum watermark sample timed out");
                }
            }
        }
    }

    async fn run_passive_metrics_loop<S, F>(
        mut source: S,
        metrics: PassiveRaftMetrics,
        shutdown: F,
        refresh_period: Duration,
    ) where
        S: PassiveRaftSource,
        F: Future<Output = ()> + Send,
    {
        let initial = source.snapshot();
        if !metrics.publish(&initial) {
            return;
        }
        let mut refresh = tokio::time::interval(refresh_period);
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        refresh.tick().await;
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => return,
                changed = source.wait_for_change() => {
                    if !changed {
                        metrics.record_error();
                        return;
                    }
                }
                _ = refresh.tick() => {}
            }
            let sample = source.snapshot();
            if !metrics.publish(&sample) {
                tracing::warn!(
                    current_term = sample.current_term,
                    last_applied_index = sample.last_applied_index,
                    "stopping passive metrics after an invalid local Raft observation"
                );
                return;
            }
        }
    }

    /// Live status reader kept beside the selected store in daemon state.
    #[derive(Clone)]
    pub struct ReplicationMonitor {
        backend: ReplicationBackend,
        client: Option<Client>,
        local_metrics: Option<LocalDbRaftMetrics>,
        wal_status: Option<hiqlite::WalStatusHandle>,
        passive_metrics: PassiveRaftMetrics,
        previous: Arc<Mutex<Option<ReplicationStatus>>>,
    }

    impl ReplicationMonitor {
        /// A local SQLite install: truthful single-node status, never "synced".
        #[must_use]
        pub fn sqlite() -> Self {
            Self {
                backend: ReplicationBackend::Sqlite,
                client: None,
                local_metrics: None,
                wal_status: None,
                passive_metrics: PassiveRaftMetrics::new(false),
                previous: Arc::new(Mutex::new(None)),
            }
        }

        /// Monitor a local Hiqlite voter through the same client the Store uses.
        #[must_use]
        pub fn replicated(client: Client) -> Self {
            let local_metrics = client.local_db_raft_metrics().ok();
            let snapshot_metrics = client.local_db_snapshot_metrics().ok();
            let wal_status = client.local_db_wal_status().ok();
            let passive_metrics = PassiveRaftMetrics::new(local_metrics.is_some())
                .with_snapshot_metrics(snapshot_metrics);
            Self {
                backend: ReplicationBackend::Replicated,
                client: Some(client),
                local_metrics,
                wal_status,
                passive_metrics,
                previous: Arc::new(Mutex::new(None)),
            }
        }

        /// Monitor a distinct serving process through a remote Hiqlite
        /// client. Unlike the daemon's embedded-voter path this publishes only
        /// a bounded quorum authority lease. It has no local applied-index
        /// source, cannot claim apply lag, and is ineligible for bounded local
        /// replica reads.
        #[must_use]
        pub fn replicated_remote(client: Client) -> Self {
            Self {
                backend: ReplicationBackend::Replicated,
                client: Some(client),
                local_metrics: None,
                wal_status: None,
                passive_metrics: PassiveRaftMetrics::remote_authority(),
                previous: Arc::new(Mutex::new(None)),
            }
        }

        /// Store-free metrics handle suitable for unauthenticated scrapes.
        #[must_use]
        pub fn metrics_handle(&self) -> PassiveRaftMetrics {
            self.passive_metrics.clone()
        }

        /// Copy the process-local live WAL snapshot without filesystem IO.
        #[must_use]
        pub fn wal_status_snapshot(&self) -> Option<hiqlite::WalStatusSnapshot> {
            self.wal_status
                .as_ref()
                .map(hiqlite::WalStatusHandle::snapshot)
        }

        /// Keep the atomics-only metrics projection fresh from the local Raft
        /// watch. Changes publish immediately; the periodic refresh keeps an
        /// idle healthy cluster from looking stale.
        pub async fn passive_metrics_loop<F>(self, shutdown: F)
        where
            F: Future<Output = ()> + Send,
        {
            let Some(client) = self.client.clone() else {
                return;
            };
            if self
                .passive_metrics
                .inner
                .sampler_started
                .swap(true, Ordering::AcqRel)
            {
                return;
            }
            let stagger_slot = self
                .local_metrics
                .as_ref()
                .map_or(0, |watch| watch.snapshot().node_id.saturating_sub(1) % 5);
            let node_stagger = Duration::from_millis(stagger_slot.saturating_mul(75));
            let local_metrics = self.local_metrics.clone();
            let local_passive = self.passive_metrics.clone();
            let watermark_passive = self.passive_metrics;
            let local_loop = async move {
                if let Some(watch) = local_metrics {
                    run_passive_metrics_loop(
                        watch,
                        local_passive,
                        std::future::pending(),
                        PASSIVE_METRICS_REFRESH,
                    )
                    .await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            let watermark_loop = run_quorum_watermark_loop(
                client,
                watermark_passive,
                std::future::pending(),
                node_stagger,
                QUORUM_WATERMARK_REFRESH,
                QUORUM_WATERMARK_TIMEOUT,
            );
            tokio::pin!(shutdown, local_loop, watermark_loop);
            tokio::select! {
                biased;
                _ = &mut shutdown => {}
                _ = &mut local_loop => {}
                _ = &mut watermark_loop => {}
            }
        }

        /// Read the current projection without changing membership or durable data.
        pub async fn status(&self) -> ReplicationStatus {
            let checked_at = unix_seconds();
            if self.backend == ReplicationBackend::Sqlite {
                return ReplicationStatus {
                    backend: self.backend,
                    health: ReplicationHealth::SingleNode,
                    clustered: false,
                    last_applied_term: None,
                    last_applied_index: None,
                    behind_by: None,
                    last_converged_at: None,
                    last_converged_index: None,
                    checked_at,
                    explanation: "Watch progress is stored only on this server; this SQLite node is not clustered."
                        .to_owned(),
                };
            }

            let previous = self.previous();
            let client = self
                .client
                .as_ref()
                .expect("replicated monitor must carry a client");
            let status = match client.metrics_db().await {
                Ok(metrics) => {
                    let voter_ids = metrics
                        .membership_config
                        .voter_ids()
                        .collect::<BTreeSet<_>>();
                    let peer_matched_indexes = metrics.replication.as_ref().map(|replication| {
                        replication
                            .iter()
                            // Learner catch-up is reported per node by the
                            // membership projection. It must never make the
                            // voter replication status claim that quorum
                            // redundancy is degraded.
                            .filter(|(node_id, _)| {
                                **node_id != metrics.id && voter_ids.contains(node_id)
                            })
                            .map(|(_, applied)| applied.as_ref().map(|log| log.index))
                            .collect()
                    });
                    let observation = ReplicationObservation {
                        running: metrics.running_state.is_ok(),
                        leader_known: metrics.current_leader.is_some(),
                        voter_count: voter_ids.len(),
                        last_log_index: metrics.last_log_index,
                        last_applied_term: metrics
                            .last_applied
                            .as_ref()
                            .map(|log| log.leader_id.term),
                        last_applied_index: metrics.last_applied.as_ref().map(|log| log.index),
                        peer_matched_indexes,
                    };
                    classify_replicated(&observation, previous.as_ref(), checked_at)
                }
                Err(error) => {
                    tracing::warn!(%error, "reading replicated-store status failed");
                    unavailable(previous.as_ref(), checked_at)
                }
            };
            self.remember(status.clone());
            status
        }

        fn previous(&self) -> Option<ReplicationStatus> {
            self.previous
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn remember(&self, status: ReplicationStatus) {
            *self
                .previous
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
        }
    }

    fn classify_replicated(
        observation: &ReplicationObservation,
        previous: Option<&ReplicationStatus>,
        checked_at: i64,
    ) -> ReplicationStatus {
        let clustered = observation.voter_count > 1;
        let two_voter_reconfiguration = observation.voter_count == 2;
        let local_lag = match (observation.last_log_index, observation.last_applied_index) {
            (Some(log), Some(applied)) => log.saturating_sub(applied),
            (Some(log), None) => log,
            _ => 0,
        };

        let expected_peers = observation.voter_count.saturating_sub(1);
        let mut unknown_peer = false;
        let mut peer_lag = 0_u64;
        if let Some(peers) = &observation.peer_matched_indexes {
            unknown_peer = peers.len() < expected_peers;
            if let Some(target) = observation.last_applied_index {
                for applied in peers {
                    match applied {
                        Some(index) => peer_lag = peer_lag.max(target.saturating_sub(*index)),
                        None => unknown_peer = true,
                    }
                }
            }
        }

        // Once every voter was positively converged, the local applied-index
        // advance bounds how far behind a now-unreported peer could be. The peer
        // may have received unseen entries before disappearing, so this is a
        // conservative upper bound rather than a proven or exact peer index.
        let inferred_peer_lag = if unknown_peer {
            previous
                .and_then(|status| status.last_converged_index)
                .zip(observation.last_applied_index)
                .map_or(0, |(previous_index, current_index)| {
                    current_index.saturating_sub(previous_index)
                })
        } else {
            0
        };

        let degraded = !observation.running
            || !observation.leader_known
            || two_voter_reconfiguration
            || local_lag > 0
            || peer_lag > 0
            || (observation.peer_matched_indexes.is_some() && unknown_peer);
        let behind_by = local_lag.max(peer_lag).max(inferred_peer_lag);
        let last_converged_at = if degraded {
            previous.and_then(|status| status.last_converged_at)
        } else {
            Some(checked_at)
        };
        let last_converged_index = if degraded {
            previous.and_then(|status| status.last_converged_index)
        } else {
            observation.last_applied_index
        };
        let explanation = if two_voter_reconfiguration {
            "Two voters are a degraded reconfiguration state, not supported HA. Keep both voters online and add a third before treating the cluster as redundant."
                .to_owned()
        } else if !degraded && clustered && observation.peer_matched_indexes.is_some() {
            "Watch progress is replicated; this node has applied the latest known change and every reporting peer has received it."
                .to_owned()
        } else if !degraded && clustered {
            "Watch progress is replicated; this node has applied every change sent by the leader. Replication status for the other nodes is visible on the leader."
                .to_owned()
        } else if !degraded {
            "Replicated storage is active, but this is currently a one-node cluster; there is no second node to sync with."
                .to_owned()
        } else if local_lag > 0 {
            "Watch progress replication is behind on this node. Keep it running while it catches up."
                .to_owned()
        } else if peer_lag > 0 || unknown_peer {
            "Watch progress replication is behind on one or more nodes. Keep the cluster online while it catches up."
                .to_owned()
        } else {
            "Replication status cannot be confirmed, so watch progress may not yet appear on every node."
                .to_owned()
        };

        ReplicationStatus {
            backend: ReplicationBackend::Replicated,
            health: if degraded {
                ReplicationHealth::Degraded
            } else {
                ReplicationHealth::InSync
            },
            clustered,
            last_applied_term: observation.last_applied_term,
            last_applied_index: observation.last_applied_index,
            behind_by: (behind_by > 0).then_some(behind_by),
            last_converged_at,
            last_converged_index,
            checked_at,
            explanation,
        }
    }

    fn unavailable(previous: Option<&ReplicationStatus>, checked_at: i64) -> ReplicationStatus {
        ReplicationStatus {
            backend: ReplicationBackend::Replicated,
            health: ReplicationHealth::Degraded,
            clustered: previous.is_some_and(|status| status.clustered),
            last_applied_term: previous.and_then(|status| status.last_applied_term),
            last_applied_index: previous.and_then(|status| status.last_applied_index),
            behind_by: previous.and_then(|status| status.behind_by),
            last_converged_at: previous.and_then(|status| status.last_converged_at),
            last_converged_index: previous.and_then(|status| status.last_converged_index),
            checked_at,
            explanation:
                "Replication status cannot be confirmed, so watch progress may not yet appear on every node."
                    .to_owned(),
        }
    }

    fn unix_seconds() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn converged(voters: usize) -> ReplicationObservation {
            ReplicationObservation {
                running: true,
                leader_known: true,
                voter_count: voters,
                last_log_index: Some(42),
                last_applied_term: Some(7),
                last_applied_index: Some(42),
                peer_matched_indexes: (voters > 1).then(|| vec![Some(42); voters - 1]),
            }
        }

        fn local_sample(
            current_term: u64,
            last_applied_index: Option<u64>,
            current_leader: Option<u64>,
        ) -> LocalDbRaftSnapshot {
            LocalDbRaftSnapshot {
                running: true,
                node_id: 1,
                current_term,
                current_leader,
                last_applied_term: Some(current_term),
                last_applied_index,
            }
        }

        fn watermark(term: u64, leader_id: u64, committed_index: u64) -> DbQuorumWatermark {
            watermark_with_protocol(
                term,
                leader_id,
                committed_index,
                hiqlite::DB_LOCAL_READ_PROTOCOL_VERSION,
            )
        }

        fn watermark_with_protocol(
            term: u64,
            leader_id: u64,
            committed_index: u64,
            local_read_protocol_version: u64,
        ) -> DbQuorumWatermark {
            DbQuorumWatermark {
                term,
                leader_id,
                committed_index,
                local_read_protocol_version,
            }
        }

        #[test]
        fn quorum_watermark_deadline_is_anchored_before_the_request() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(40), Some(1)), 10));
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 42),
                10_000_000_000,
                10_999_999_999,
            ));
            let before_deadline = metrics.snapshot_at_times(10, 10_999_999_999);
            assert!(before_deadline.watermark_valid);
            assert_eq!(
                before_deadline
                    .watermark
                    .expect("watermark")
                    .apply_lag_entries,
                Some(2)
            );

            let at_deadline = metrics.snapshot_at_times(11, 11_000_000_000);
            assert!(!at_deadline.watermark_valid);
            assert_eq!(
                at_deadline
                    .watermark
                    .expect("retained watermark")
                    .apply_lag_entries,
                None
            );
            assert!(!metrics.publish_watermark_at(
                watermark(7, 1, 43),
                11_000_000_000,
                12_000_000_000,
            ));
            assert_eq!(
                metrics
                    .snapshot_at_times(12, 12_000_000_000)
                    .watermark_errors,
                1
            );
        }

        #[test]
        fn bounded_replica_permit_requires_both_fresh_proofs_and_the_entry_budget() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));

            assert!(
                metrics
                    .try_bounded_replica_at(10, 10_200_000_000, 2)
                    .is_none(),
                "three unapplied entries exceed a two-entry budget"
            );
            let permit = metrics
                .try_bounded_replica_at(10, 10_200_000_000, 3)
                .expect("fresh local and quorum proofs");
            assert_eq!(permit.committed_index, 45);
            assert_eq!(permit.applied_index, 42);
            assert_eq!(
                permit.committed_index.saturating_sub(permit.applied_index),
                3
            );
            assert!(permit.remains_valid_at(10, 10_999_999_999));
            assert!(!permit.remains_valid_at(11, 11_000_000_000));
        }

        #[test]
        fn bounded_replica_permit_requires_the_watermark_sources_read_protocol() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            assert!(metrics.publish_watermark_at(
                watermark_with_protocol(7, 1, 42, 0),
                10_000_000_000,
                10_100_000_000,
            ));

            let view = metrics.snapshot_at_times(10, 10_200_000_000);
            assert!(view.watermark_valid, "authority proof remains usable");
            assert!(!view.watermark_local_reads_supported);
            assert!(
                metrics
                    .try_bounded_replica_at(10, 10_200_000_000, 0)
                    .is_none(),
                "rolling-upgrade mismatch must use Authority"
            );

            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 42),
                10_300_000_000,
                10_400_000_000,
            ));
            assert!(
                metrics
                    .try_bounded_replica_at(10, 10_500_000_000, 0)
                    .is_some(),
                "matching source and receiver protocols may use the local replica"
            );
        }

        #[tokio::test]
        async fn bounded_replica_wrapper_issues_before_and_revalidates_after_one_local_read() {
            let metrics = PassiveRaftMetrics::new(true);
            let calls = Arc::new(AtomicU64::new(0));
            let unavailable_calls = Arc::clone(&calls);
            assert_eq!(
                metrics
                    .run_bounded_replica(0, move || {
                        unavailable_calls.fetch_add(1, Ordering::Relaxed);
                        async { 41 }
                    })
                    .await,
                None
            );
            assert_eq!(calls.load(Ordering::Relaxed), 0);

            assert!(metrics.publish(&local_sample(7, Some(45), Some(1))));
            let started_nanos = metrics.elapsed_nanos();
            assert!(metrics.publish_watermark(watermark(7, 1, 45), started_nanos));
            let available_calls = Arc::clone(&calls);
            assert_eq!(
                metrics
                    .run_bounded_replica(0, move || {
                        available_calls.fetch_add(1, Ordering::Relaxed);
                        async { 42 }
                    })
                    .await,
                Some(42)
            );
            assert_eq!(calls.load(Ordering::Relaxed), 1);
        }

        #[test]
        fn bounded_replica_permit_deadline_is_not_extended_by_a_newer_watermark() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(45), Some(1)), 10));
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));
            let permit = metrics
                .try_bounded_replica_at(10, 10_200_000_000, 0)
                .expect("initial permit");

            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 45),
                10_600_000_000,
                10_700_000_000,
            ));
            assert!(permit.remains_valid_at(10, 10_999_999_999));
            assert!(
                !permit.remains_valid_at(11, 11_000_000_000),
                "a refresh must not extend a capability issued from the older request"
            );
        }

        #[test]
        fn bounded_replica_permit_rejects_remote_authority_and_generation_changes() {
            let remote = PassiveRaftMetrics::remote_authority();
            assert!(remote.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));
            assert!(
                remote
                    .try_bounded_replica_at(10, 10_200_000_000, u64::MAX)
                    .is_none(),
                "a remote serving process has no local replica proof"
            );

            let local = PassiveRaftMetrics::new(true);
            assert!(local.publish_at(&local_sample(7, Some(45), Some(1)), 10));
            assert!(local.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));
            let permit = local
                .try_bounded_replica_at(10, 10_200_000_000, 0)
                .expect("initial permit");

            assert!(local.publish_at(&local_sample(8, Some(46), Some(2)), 10));
            assert!(local.publish_watermark_at(
                watermark(8, 2, 46),
                10_300_000_000,
                10_400_000_000,
            ));
            assert!(
                !permit.remains_valid_at(10, 10_500_000_000),
                "a fresh proof in a new term cannot revive an old permit"
            );
        }

        #[test]
        fn successor_watermark_revokes_old_permit_before_the_local_watch_catches_up() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(45), Some(1)), 10));
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));
            let permit = metrics
                .try_bounded_replica_at(10, 10_200_000_000, 0)
                .expect("old-generation permit");

            assert!(!metrics.publish_watermark_at(
                watermark(6, 9, 44),
                10_250_000_000,
                10_300_000_000,
            ));
            assert!(
                permit.remains_valid_at(10, 10_350_000_000),
                "a genuinely delayed older generation must not revoke the retained proof"
            );

            assert!(!metrics.publish_watermark_at(
                watermark(8, 2, 46),
                10_400_000_000,
                10_500_000_000,
            ));
            assert!(
                !permit.remains_valid_at(10, 10_600_000_000),
                "a successful successor proof must immediately revoke the old capability"
            );
            assert!(
                metrics
                    .try_bounded_replica_at(10, 10_600_000_000, u64::MAX)
                    .is_none(),
                "the stale local observation cannot negotiate a new permit"
            );

            assert!(metrics.publish_at(&local_sample(8, Some(46), Some(2)), 10));
            assert!(metrics.publish_watermark_at(
                watermark(8, 2, 46),
                10_650_000_000,
                10_700_000_000,
            ));
            assert!(
                metrics
                    .try_bounded_replica_at(10, 10_800_000_000, 0)
                    .is_some(),
                "matching local and quorum successor proofs recover eligibility"
            );
        }

        #[test]
        fn bounded_replica_permit_is_revoked_when_the_latest_entry_gap_grows() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(45), Some(1)), 10));
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));
            let permit = metrics
                .try_bounded_replica_at(10, 10_200_000_000, 0)
                .expect("zero-lag permit");

            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 46),
                10_300_000_000,
                10_400_000_000,
            ));
            assert!(
                !permit.remains_valid_at(10, 10_500_000_000),
                "the second phase must enforce the configured lag budget too"
            );
        }

        #[test]
        fn watermark_lease_path_has_no_wall_clock_dependency() {
            let source = include_str!("migration.rs");
            let snapshot_path = source
                .split_once("pub fn snapshot(&self) -> PassiveRaftMetricsView")
                .expect("snapshot path")
                .1
                .split_once("fn publish(&self")
                .expect("end snapshot path")
                .0;
            let publication_path = source
                .split_once("fn elapsed_nanos(&self)")
                .expect("watermark publication path")
                .1
                .split_once("fn record_watermark_error")
                .expect("end watermark publication path")
                .0;
            let sampler_path = source
                .split_once("async fn run_quorum_watermark_loop")
                .expect("watermark sampler path")
                .1
                .split_once("async fn run_passive_metrics_loop")
                .expect("end watermark sampler path")
                .0;

            for (name, path) in [
                ("snapshot", snapshot_path),
                ("publication", publication_path),
                ("sampler", sampler_path),
            ] {
                for forbidden in ["SystemTime", "UNIX_EPOCH", "Utc::", "unix_"] {
                    assert!(
                        !path.contains(forbidden),
                        "{name} path introduced wall-clock dependency {forbidden}"
                    );
                }
            }
            assert!(snapshot_path.contains("started_at.elapsed()"));
            assert!(publication_path.contains("started_at.elapsed()"));
            assert!(sampler_path.contains("metrics.elapsed_nanos()"));
        }

        #[test]
        fn every_term_or_leader_transition_invalidates_the_old_generation() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 42),
                10_000_000_000,
                10_100_000_000,
            ));
            assert!(
                metrics
                    .snapshot_at_times(10, 10_200_000_000)
                    .watermark_valid
            );

            assert!(metrics.publish_at(&local_sample(7, Some(42), None), 10));
            assert!(
                !metrics
                    .snapshot_at_times(10, 10_300_000_000)
                    .watermark_valid
            );
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            assert!(
                !metrics
                    .snapshot_at_times(10, 10_400_000_000)
                    .watermark_valid,
                "Some -> None -> same leader must not resurrect the old proof"
            );

            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 42),
                10_400_000_000,
                10_500_000_000,
            ));
            assert!(
                metrics
                    .snapshot_at_times(10, 10_600_000_000)
                    .watermark_valid
            );

            assert!(metrics.publish_at(&local_sample(8, Some(43), Some(2)), 10));
            assert!(
                !metrics
                    .snapshot_at_times(10, 10_700_000_000)
                    .watermark_valid
            );
            assert!(
                !metrics.publish_watermark_at(watermark(7, 1, 44), 10_700_000_000, 10_800_000_000,),
                "a delayed old-term response must not revalidate"
            );
            assert!(metrics.publish_watermark_at(
                watermark(8, 2, 44),
                10_800_000_000,
                10_900_000_000,
            ));
            assert!(
                metrics
                    .snapshot_at_times(10, 11_000_000_000)
                    .watermark_valid
            );
        }

        #[test]
        fn watermark_failures_do_not_extend_or_regress_the_last_proof() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));
            metrics.record_watermark_error();
            let retained = metrics.snapshot_at_times(10, 10_900_000_000);
            assert!(retained.watermark_valid);
            assert_eq!(retained.watermark_errors, 1);
            assert_eq!(retained.watermark_age_millis, Some(900));

            assert!(!metrics.publish_watermark_at(
                watermark(7, 1, 44),
                10_900_000_000,
                10_950_000_000,
            ));
            let expired = metrics.snapshot_at_times(11, 11_000_000_000);
            assert!(!expired.watermark_valid);
            assert_eq!(expired.watermark.expect("retained").committed_index, 45);
            assert_eq!(expired.watermark_errors, 2);
        }

        #[test]
        fn remote_authority_never_invents_a_local_replica_or_apply_lag() {
            let metrics = PassiveRaftMetrics::remote_authority();
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));

            let current = metrics.snapshot_at_times(10, 10_200_000_000);
            assert!(!current.local_source);
            assert!(current.sample.is_none());
            assert!(current.watermark_source);
            assert!(!current.watermark_requires_local_binding);
            assert!(current.watermark_valid);
            assert_eq!(
                current
                    .watermark
                    .expect("remote authority")
                    .apply_lag_entries,
                None
            );

            metrics.record_watermark_error();
            let retained = metrics.snapshot_at_times(10, 10_999_999_999);
            assert!(retained.watermark_valid);
            assert_eq!(retained.watermark_errors, 1);

            let expired = metrics.snapshot_at_times(11, 11_000_000_000);
            assert!(!expired.watermark_valid);
            assert_eq!(
                expired
                    .watermark
                    .expect("retained remote authority")
                    .apply_lag_entries,
                None
            );
        }

        #[test]
        fn remote_authority_rejects_delayed_and_invalidates_conflicting_generations() {
            let metrics = PassiveRaftMetrics::remote_authority();
            assert!(metrics.publish_watermark_at(
                watermark(7, 1, 45),
                10_000_000_000,
                10_100_000_000,
            ));
            assert!(metrics.publish_watermark_at(
                watermark(8, 2, 46),
                10_200_000_000,
                10_300_000_000,
            ));
            assert!(!metrics.publish_watermark_at(
                watermark(7, 1, 47),
                10_300_000_000,
                10_400_000_000,
            ));
            assert!(!metrics.publish_watermark_at(
                watermark(8, 3, 47),
                10_400_000_000,
                10_500_000_000,
            ));

            let current = metrics.snapshot_at_times(10, 10_600_000_000);
            assert!(
                !current.watermark_valid,
                "a conflicting same-term leader must revoke the retained proof immediately"
            );
            let watermark = current.watermark.expect("latest remote authority");
            assert_eq!(watermark.committed_index, 46);
            assert_eq!(watermark.apply_lag_entries, None);
            assert_eq!(current.watermark_errors, 2);
        }

        #[test]
        fn remote_authority_sampler_never_polls_replica_management_metrics() {
            let source = include_str!("migration.rs");
            let constructor = source
                .split_once("pub fn replicated_remote")
                .expect("remote monitor constructor")
                .1
                .split_once("pub fn metrics_handle")
                .expect("end remote monitor constructor")
                .0;
            let sampler = source
                .split_once("pub async fn passive_metrics_loop")
                .expect("passive sampler")
                .1
                .split_once("pub async fn status")
                .expect("end passive sampler")
                .0;

            assert!(constructor.contains("PassiveRaftMetrics::remote_authority()"));
            assert!(!constructor.contains("local_db_raft_metrics"));
            assert!(!sampler.contains("metrics_db"));
        }

        #[test]
        fn watermark_counter_saturates() {
            let metrics = PassiveRaftMetrics::new(true);
            metrics
                .inner
                .watermark_errors
                .store(u64::MAX, Ordering::Relaxed);
            metrics.record_watermark_error();
            assert_eq!(metrics.snapshot().watermark_errors, u64::MAX);
        }

        #[test]
        fn watermark_lease_is_shorter_than_the_vendor_election_floor() {
            let config = hiqlite::NodeConfig::default_raft_config(1_000);
            assert!(
                QUORUM_WATERMARK_LEASE.as_millis() < u128::from(config.election_timeout_min),
                "an isolated former leader must lose its proof before a new election can finish"
            );
        }

        #[test]
        fn watermark_vendor_boundary_is_bounded_and_rolling_safe() {
            let management = include_str!("../../../../vendor/hiqlite/src/client/mgmt.rs");
            let api = include_str!("../../../../vendor/hiqlite/src/network/api.rs");
            let stream = include_str!("../../../../vendor/hiqlite/src/client/stream.rs");
            assert!(management.contains("DB_QUORUM_WATERMARK_MARKER"));
            assert!(management.contains("query_remote_req"));
            assert!(management.contains("ensure_linearizable"));
            assert!(management.contains("Duration::from_secs(1)"));
            assert!(api.contains("ApiStreamRequestPayload::QueryConsistent"));
            assert!(api.contains("db_quorum_watermark_local"));
            assert!(!api.contains("ApiStreamRequestPayload::QuorumWatermark"));
            assert!(!stream.contains("ClientStreamReq::QuorumWatermark"));

            let connection = rusqlite::Connection::open_in_memory().expect("SQLite");
            let old_server_result =
                connection.prepare("/* hiqlite-internal:db-quorum-watermark:v1 */ THIS IS NOT SQL");
            assert!(
                old_server_result.is_err(),
                "an old leader must reject the reserved marker as harmless invalid SQL"
            );
        }

        #[test]
        fn snapshot_metrics_use_explicit_build_and_install_hooks() {
            let builder = include_str!(
                "../../../../vendor/hiqlite/src/store/state_machine/sqlite/snapshot_builder.rs"
            );
            let state_machine = include_str!(
                "../../../../vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs"
            );
            let metrics = include_str!("../../../../vendor/hiqlite/src/snapshot_metrics.rs");
            assert!(builder.contains("SnapshotTimer::start(SnapshotOperation::Build)"));
            assert!(builder.contains("timer.success()"));
            assert!(state_machine.contains("SnapshotTimer::start(SnapshotOperation::Install)"));
            assert!(state_machine.contains("timer.success()"));
            assert!(metrics.contains("impl Drop for SnapshotTimer"));
            assert!(metrics.contains("cancellation, panic, and an early `?` are errors"));
            let private_handle =
                ["pub struct LocalDbSnapshot", "Metrics {\n    _private: (),"].concat();
            let public_default = ["impl Default for LocalDb", "SnapshotMetrics"].concat();
            let lazy_global = ["Once", "Lock"].concat();
            assert!(metrics.contains(&private_handle));
            assert!(metrics.contains("ArcSwapOption::const_empty()"));
            assert!(metrics.contains("load_full()"));
            assert!(!metrics.contains(&public_default));
            assert!(!metrics.contains(&lazy_global));
            assert!(!metrics.contains("node_id"));
            assert!(!metrics.contains("snapshot_id"));
            assert!(!metrics.contains("path_snapshots"));
        }

        #[test]
        fn passive_metrics_refresh_idle_samples_and_expire_without_refresh() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            let stale = metrics.snapshot_at(41);
            assert_eq!(stale.age_seconds, Some(31));
            assert!(!stale.valid);

            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 50));
            let fresh = metrics.snapshot_at(51);
            assert_eq!(fresh.age_seconds, Some(1));
            assert!(fresh.valid);
            assert_eq!(fresh.leader_changes, 0);
        }

        #[test]
        fn passive_metrics_reject_regressions_and_preserve_the_last_sample() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            assert!(!metrics.publish_at(&local_sample(6, Some(41), Some(2)), 20));

            let view = metrics.snapshot_at(21);
            assert_eq!(view.errors, 1);
            assert_eq!(view.age_seconds, Some(11));
            assert_eq!(view.sample.expect("last good sample").current_term, 7);
            assert_eq!(
                view.sample.expect("last good sample").last_applied_index,
                Some(42)
            );
            assert_eq!(view.leader_changes, 0, "a rejected sample is not observed");
        }

        #[test]
        fn unhealthy_observation_stops_refreshing_the_last_good_sample() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            let mut unhealthy = local_sample(7, Some(42), Some(1));
            unhealthy.running = false;
            assert!(!metrics.publish_at(&unhealthy, 20));

            let view = metrics.snapshot_at(26);
            assert_eq!(view.errors, 1);
            assert_eq!(view.age_seconds, Some(16));
            assert!(!view.valid);
            assert_eq!(view.sample.expect("retained sample").current_term, 7);
        }

        #[test]
        fn transient_unknown_leader_does_not_double_count_the_same_identity() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            assert!(metrics.publish_at(&local_sample(7, Some(42), None), 11));
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 12));
            assert_eq!(metrics.snapshot_at(12).leader_changes, 0);

            assert!(metrics.publish_at(&local_sample(8, Some(43), Some(2)), 13));
            let changed = metrics.snapshot_at(13);
            assert_eq!(changed.leader_changes, 1);
            assert!(!changed.sample.expect("sample").is_leader);
        }

        #[test]
        fn passive_metric_counters_saturate() {
            let metrics = PassiveRaftMetrics::new(true);
            metrics.inner.errors.store(u64::MAX, Ordering::Relaxed);
            metrics
                .inner
                .leader_changes
                .store(u64::MAX, Ordering::Relaxed);
            metrics.record_error();
            assert!(metrics.publish_at(&local_sample(7, Some(42), Some(1)), 10));
            assert!(metrics.publish_at(&local_sample(8, Some(43), Some(2)), 11));
            let view = metrics.snapshot_at(11);
            assert_eq!(view.errors, u64::MAX);
            assert_eq!(view.leader_changes, u64::MAX);
        }

        #[test]
        fn passive_metrics_never_expose_a_torn_tuple() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish_at(&local_sample(1, Some(10), Some(1)), 1));
            let writer = metrics.clone();
            let finished = Arc::new(AtomicBool::new(false));
            let writer_finished = Arc::clone(&finished);
            let handle = std::thread::spawn(move || {
                for term in 2..=5_000 {
                    let leader = if term % 2 == 0 { 2 } else { 1 };
                    assert!(writer
                        .publish_at(&local_sample(term, Some(term * 10), Some(leader)), term,));
                }
                writer_finished.store(true, Ordering::Release);
            });

            while !finished.load(Ordering::Acquire) {
                let view = metrics.snapshot_at(5_000);
                let sample = view.sample.expect("published sample");
                assert_eq!(sample.last_applied_index, Some(sample.current_term * 10));
                assert_eq!(sample.is_leader, !sample.current_term.is_multiple_of(2));
                assert_eq!(view.leader_changes, sample.current_term.saturating_sub(1));
            }
            handle.join().expect("metrics writer");
        }

        #[test]
        fn combined_local_and_watermark_view_never_exposes_a_torn_valid_lag() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish(&local_sample(1, Some(10), Some(1))));
            let started = metrics.elapsed_nanos();
            assert!(metrics.publish_watermark(watermark(1, 1, 15), started));
            let writer = metrics.clone();
            let finished = Arc::new(AtomicBool::new(false));
            let writer_finished = Arc::clone(&finished);
            let handle = std::thread::spawn(move || {
                for term in 2..=5_000 {
                    let leader = if term % 2 == 0 { 2 } else { 1 };
                    assert!(writer.publish(&local_sample(term, Some(term * 10), Some(leader))));
                    let started = writer.elapsed_nanos();
                    assert!(
                        writer.publish_watermark(watermark(term, leader, term * 10 + 5), started,)
                    );
                }
                writer_finished.store(true, Ordering::Release);
            });

            while !finished.load(Ordering::Acquire) {
                let view = metrics.snapshot();
                if view.watermark_valid {
                    assert_eq!(
                        view.watermark.expect("valid watermark").apply_lag_entries,
                        Some(5)
                    );
                } else if let Some(watermark) = view.watermark {
                    assert_eq!(watermark.apply_lag_entries, None);
                }
            }
            handle.join().expect("metrics writer");
        }

        struct FakePassiveSource {
            receiver: tokio::sync::watch::Receiver<LocalDbRaftSnapshot>,
            snapshots: Arc<AtomicU64>,
        }

        struct FakeWatermarkSource {
            sample: DbQuorumWatermark,
            delay: Duration,
            calls: Arc<AtomicU64>,
        }

        #[async_trait::async_trait]
        impl QuorumWatermarkSource for FakeWatermarkSource {
            async fn sample(&self) -> Result<DbQuorumWatermark, hiqlite::Error> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(self.delay).await;
                Ok(self.sample)
            }
        }

        #[async_trait::async_trait]
        impl PassiveRaftSource for FakePassiveSource {
            fn snapshot(&self) -> LocalDbRaftSnapshot {
                self.snapshots.fetch_add(1, Ordering::Relaxed);
                self.receiver.borrow().clone()
            }

            async fn wait_for_change(&mut self) -> bool {
                self.receiver.changed().await.is_ok()
            }
        }

        #[tokio::test]
        async fn idle_observer_republishes_and_stops_on_cancellation() {
            let (_sender, receiver) =
                tokio::sync::watch::channel(local_sample(7, Some(42), Some(1)));
            let snapshots = Arc::new(AtomicU64::new(0));
            let metrics = PassiveRaftMetrics::new(true);
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(run_passive_metrics_loop(
                FakePassiveSource {
                    receiver,
                    snapshots: Arc::clone(&snapshots),
                },
                metrics.clone(),
                async move {
                    let _ = cancelled.await;
                },
                Duration::from_millis(1),
            ));

            tokio::time::timeout(Duration::from_millis(100), async {
                while snapshots.load(Ordering::Relaxed) < 3 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("idle observer periodic refresh");
            assert!(metrics.snapshot().valid);
            cancel.send(()).expect("cancel observer");
            tokio::time::timeout(Duration::from_millis(100), task)
                .await
                .expect("observer cancellation timeout")
                .expect("observer task");
        }

        #[tokio::test]
        async fn watch_change_publishes_without_waiting_for_the_fallback_tick() {
            let (sender, receiver) =
                tokio::sync::watch::channel(local_sample(7, Some(42), Some(1)));
            let snapshots = Arc::new(AtomicU64::new(0));
            let metrics = PassiveRaftMetrics::new(true);
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(run_passive_metrics_loop(
                FakePassiveSource {
                    receiver,
                    snapshots: Arc::clone(&snapshots),
                },
                metrics.clone(),
                async move {
                    let _ = cancelled.await;
                },
                Duration::from_secs(60),
            ));
            tokio::time::timeout(Duration::from_millis(100), async {
                while snapshots.load(Ordering::Relaxed) < 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("initial watch sample");

            sender
                .send(local_sample(8, Some(43), Some(2)))
                .expect("publish watch change");
            tokio::time::timeout(Duration::from_millis(100), async {
                while snapshots.load(Ordering::Relaxed) < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("watch change must beat fallback tick");
            let view = metrics.snapshot();
            assert_eq!(view.sample.expect("changed sample").current_term, 8);
            assert_eq!(view.leader_changes, 1);
            cancel.send(()).expect("cancel observer");
            tokio::time::timeout(Duration::from_millis(100), task)
                .await
                .expect("observer cancellation timeout")
                .expect("observer task");
        }

        #[tokio::test]
        async fn closed_watch_records_one_error_and_exits_without_spinning() {
            let (sender, receiver) =
                tokio::sync::watch::channel(local_sample(7, Some(42), Some(1)));
            drop(sender);
            let metrics = PassiveRaftMetrics::new(true);
            let task = tokio::spawn(run_passive_metrics_loop(
                FakePassiveSource {
                    receiver,
                    snapshots: Arc::new(AtomicU64::new(0)),
                },
                metrics.clone(),
                std::future::pending(),
                Duration::from_millis(1),
            ));
            tokio::time::timeout(Duration::from_millis(100), task)
                .await
                .expect("closed watch must not spin")
                .expect("observer task");
            assert_eq!(metrics.snapshot().errors, 1);
        }

        #[tokio::test]
        async fn watermark_sampler_cancellation_publishes_neither_value_nor_error() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish(&local_sample(7, Some(42), Some(1))));
            let calls = Arc::new(AtomicU64::new(0));
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(run_quorum_watermark_loop(
                FakeWatermarkSource {
                    sample: watermark(7, 1, 42),
                    delay: Duration::from_secs(60),
                    calls: Arc::clone(&calls),
                },
                metrics.clone(),
                async move {
                    let _ = cancelled.await;
                },
                Duration::ZERO,
                Duration::from_millis(1),
                Duration::from_secs(120),
            ));
            tokio::time::timeout(Duration::from_millis(100), async {
                while calls.load(Ordering::Relaxed) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("sampler request started");
            cancel.send(()).expect("cancel sampler");
            tokio::time::timeout(Duration::from_millis(100), task)
                .await
                .expect("sampler cancellation timeout")
                .expect("sampler task");
            let view = metrics.snapshot();
            assert_eq!(view.watermark, None);
            assert_eq!(view.watermark_errors, 0);
        }

        #[tokio::test]
        async fn watermark_sampler_timeout_keeps_the_source_absent() {
            let metrics = PassiveRaftMetrics::new(true);
            assert!(metrics.publish(&local_sample(7, Some(42), Some(1))));
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(run_quorum_watermark_loop(
                FakeWatermarkSource {
                    sample: watermark(7, 1, 42),
                    delay: Duration::from_secs(60),
                    calls: Arc::new(AtomicU64::new(0)),
                },
                metrics.clone(),
                async move {
                    let _ = cancelled.await;
                },
                Duration::ZERO,
                Duration::from_millis(50),
                Duration::from_millis(1),
            ));
            tokio::time::timeout(Duration::from_millis(100), async {
                while metrics.snapshot().watermark_errors == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("sampler timeout recorded");
            cancel.send(()).expect("cancel sampler");
            task.await.expect("sampler task");
            let view = metrics.snapshot();
            assert_eq!(view.watermark, None);
            assert!(view.watermark_errors >= 1);
        }

        #[tokio::test]
        async fn sqlite_is_single_node_instead_of_misleadingly_synced() {
            let status = ReplicationMonitor::sqlite().status().await;
            assert_eq!(status.backend, ReplicationBackend::Sqlite);
            assert_eq!(status.health, ReplicationHealth::SingleNode);
            assert!(!status.clustered);
            assert_eq!(status.last_converged_at, None);
            assert!(status.explanation.contains("stored only on this server"));
        }

        #[tokio::test]
        async fn remote_client_cannot_construct_local_raft_observers() {
            let remote = Client::remote(
                vec!["127.0.0.1:1".to_owned()],
                false,
                false,
                "not-used".to_owned(),
                true,
                None,
            )
            .await
            .expect("construct remote client without discovery");
            match remote.local_db_raft_metrics() {
                Ok(_) => panic!("remote client must not expose a local watch"),
                Err(error) => assert!(error.to_string().contains("require a local node client")),
            }
            match remote.local_db_snapshot_metrics() {
                Ok(_) => panic!("remote client must not expose local snapshot metrics"),
                Err(error) => assert!(error.to_string().contains("require a local node client")),
            }
        }

        #[test]
        fn one_voter_replicated_store_is_in_sync_but_not_clustered() {
            let status = classify_replicated(&converged(1), None, 100);
            assert_eq!(status.health, ReplicationHealth::InSync);
            assert!(!status.clustered);
            assert_eq!(status.last_applied_term, Some(7));
            assert_eq!(status.last_applied_index, Some(42));
            assert_eq!(status.last_converged_at, Some(100));
            assert!(status.explanation.contains("one-node cluster"));
        }

        #[test]
        fn two_voters_are_degraded_reconfiguration_not_healthy_ha() {
            let status = classify_replicated(&converged(2), None, 100);
            assert_eq!(status.health, ReplicationHealth::Degraded);
            assert!(status.clustered);
            assert_eq!(status.behind_by, None);
            assert!(status.explanation.contains("not supported HA"));
        }

        #[test]
        fn local_apply_lag_is_degraded_and_keeps_the_last_convergence() {
            let prior = classify_replicated(&converged(3), None, 100);
            let status = classify_replicated(
                &ReplicationObservation {
                    last_log_index: Some(47),
                    last_applied_index: Some(42),
                    peer_matched_indexes: None,
                    ..converged(3)
                },
                Some(&prior),
                200,
            );
            assert_eq!(status.health, ReplicationHealth::Degraded);
            assert_eq!(status.behind_by, Some(5));
            assert_eq!(status.last_converged_at, Some(100));
            assert!(status.explanation.contains("on this node"));
        }

        #[test]
        fn a_lagging_peer_degrades_the_cluster_surface() {
            let status = classify_replicated(
                &ReplicationObservation {
                    peer_matched_indexes: Some(vec![Some(42), Some(37)]),
                    ..converged(3)
                },
                None,
                200,
            );
            assert_eq!(status.health, ReplicationHealth::Degraded);
            assert_eq!(status.behind_by, Some(5));
            assert!(status.clustered);
            assert!(status.explanation.contains("one or more nodes"));
        }

        #[test]
        fn an_unknown_peer_is_degraded_instead_of_silently_healthy() {
            let status = classify_replicated(
                &ReplicationObservation {
                    peer_matched_indexes: Some(vec![Some(42)]),
                    ..converged(3)
                },
                None,
                200,
            );
            assert_eq!(status.health, ReplicationHealth::Degraded);
            assert_eq!(status.behind_by, None);
        }

        #[test]
        fn a_missing_peer_after_a_committed_write_has_a_conservative_lag_bound() {
            let prior = classify_replicated(&converged(3), None, 100);
            let status = classify_replicated(
                &ReplicationObservation {
                    last_log_index: Some(45),
                    last_applied_index: Some(45),
                    peer_matched_indexes: Some(vec![Some(45)]),
                    ..converged(3)
                },
                Some(&prior),
                200,
            );
            assert_eq!(status.health, ReplicationHealth::Degraded);
            assert_eq!(status.behind_by, Some(3));
            assert_eq!(status.last_converged_at, Some(100));
        }

        #[test]
        fn a_caught_up_follower_claims_only_the_visibility_it_has() {
            let status = classify_replicated(
                &ReplicationObservation {
                    peer_matched_indexes: None,
                    ..converged(3)
                },
                None,
                100,
            );

            assert_eq!(status.health, ReplicationHealth::InSync);
            assert!(status.clustered);
            assert!(status
                .explanation
                .contains("every change sent by the leader"));
            assert!(status
                .explanation
                .contains("other nodes is visible on the leader"));
            assert!(!status.explanation.contains("every reporting peer"));
        }

        #[test]
        fn missing_peer_lag_survives_repeated_samples_and_tracks_new_writes() {
            let prior = classify_replicated(&converged(3), None, 100);
            let first_degraded = classify_replicated(
                &ReplicationObservation {
                    last_log_index: Some(45),
                    last_applied_index: Some(45),
                    peer_matched_indexes: Some(vec![Some(45)]),
                    ..converged(3)
                },
                Some(&prior),
                200,
            );
            let later_degraded = classify_replicated(
                &ReplicationObservation {
                    last_log_index: Some(50),
                    last_applied_index: Some(50),
                    peer_matched_indexes: Some(vec![Some(50)]),
                    ..converged(3)
                },
                Some(&first_degraded),
                300,
            );

            assert_eq!(first_degraded.behind_by, Some(3));
            assert_eq!(later_degraded.health, ReplicationHealth::Degraded);
            assert_eq!(later_degraded.behind_by, Some(8));
            assert_eq!(later_degraded.last_converged_at, Some(100));
        }

        #[test]
        fn replicated_json_has_only_the_privacy_safe_status_contract() {
            let prior = classify_replicated(&converged(3), None, 100);
            let status = classify_replicated(
                &ReplicationObservation {
                    last_log_index: Some(45),
                    last_applied_index: Some(45),
                    peer_matched_indexes: Some(vec![Some(45)]),
                    ..converged(3)
                },
                Some(&prior),
                200,
            );
            let serialized = serde_json::to_value(status).expect("serialize status");
            let keys = serialized
                .as_object()
                .expect("status object")
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();

            assert_eq!(
                keys,
                [
                    "backend",
                    "behind_by",
                    "checked_at",
                    "clustered",
                    "explanation",
                    "health",
                    "last_applied_index",
                    "last_applied_term",
                    "last_converged_at",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                "replicated status must not expose users, media, paths, tokens, addresses, or membership"
            );
        }
    }
}
